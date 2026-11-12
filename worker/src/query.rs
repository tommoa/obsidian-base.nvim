//! Bases table-query parsing, expression evaluation, sorting, and response rendering.

use std::{
    cell::OnceCell,
    cmp::Ordering,
    collections::{BTreeMap, HashMap, hash_map::Entry},
    io::{self, Write},
    rc::Rc,
    time::Instant,
};

use serde::Serialize;

use crate::{
    environment::ExecutionEnvironment,
    error::{Result, WorkerError},
    expression::{Expression, Literal, parse_expression, safe_key},
    index::{FileRecord, Index},
    limits::Limits,
    protocol::{Column, QueryResult, View},
    value::{
        DataValue, EvalValue, append_natural_sort_key, format_utc_date, natural_cmp,
        natural_sort_key, parse_date, parse_duration, round_number, sanitize_owned_text,
    },
};

mod projection;
use projection::{Projection, ValueRef};

#[derive(Clone, Debug)]
/// Request-specific source and display context for a query execution.
pub struct QueryInput {
    /// YAML text defining the Base and its views.
    pub text: String,
    /// Stable caller-provided identity for the query source.
    pub source_id: String,
    /// Vault-relative path of the note hosting the Base.
    pub host_path: String,
    /// Optional named view to evaluate instead of the first view.
    pub view_name: Option<String>,
    /// Number of visible rows included inline in the response.
    pub preview_rows: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
/// A rendered table cell, optionally linked to a vault path.
pub struct Cell {
    /// Protocol cell type used by the client renderer.
    #[serde(rename = "type")]
    pub kind: CellKind,
    /// Sanitized display text for the cell.
    pub text: String,
    /// Destination vault path when this is a link cell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// Closed set of cell types accepted by the client protocol. Keep these as
/// values rather than allocating the same type label for every rendered cell.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CellKind {
    Link,
    Date,
    Duration,
    Empty,
    Boolean,
    Number,
    String,
    Object,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
/// One rendered query row retained for previews and later row fetching.
pub struct ResultRow {
    /// Vault-relative path represented by this row.
    pub path: String,
    /// Default label shown when no cell-specific presentation is used.
    pub display_name: String,
    /// Rendered cells in declared column order.
    pub cells: Vec<Cell>,
}

/// The protocol-facing query response and its complete retained row set.
pub struct QueryOutput {
    /// Typed data sent as the immediate protocol response.
    pub result: QueryResult,
    /// Complete rendered rows retained for subsequent fetch_rows requests.
    pub rows: Vec<ResultRow>,
    /// Compact JSON bytes in the complete `{result_id,rows}` payload.
    pub rows_bytes: usize,
}

/// Keep per-record state together through filtering, sorting, and rendering.
/// Borrowed records avoid copying paths or looking them up during comparisons.
struct MatchedRow<'a> {
    record: &'a FileRecord,
    context: EvalContext<'a>,
    /// Construct only for records participating in an explicit-sort tie.
    natural_key: OnceCell<Vec<u8>>,
}

pub fn execute(
    index: &Index,
    limits: Limits,
    generation: u64,
    result_sequence: u64,
    result_id_allocated: &mut bool,
    environment: &ExecutionEnvironment,
    input: QueryInput,
) -> Result<QueryOutput> {
    let base = crate::yaml::parse_mapping(&input.text, limits)?;
    let views = object_list(base.get("views"));
    let renderable_views = views
        .iter()
        .copied()
        .filter(|view| renderable_table_view(view))
        .collect::<Vec<_>>();
    let available_views = table_views(&renderable_views);
    let recover = |error: WorkerError| with_recovery_views(error, &available_views, limits);
    let view = (match input.view_name.as_deref() {
        Some(name) => renderable_views
            .iter()
            .find(|view| named_view(view) == Some(name))
            .copied()
            .ok_or_else(|| WorkerError::new("unknown_view", format!("view not found: {name}"))),
        None => renderable_views
            .first()
            .copied()
            .ok_or_else(|| WorkerError::new("unsupported_view", "only table views are supported")),
    })
    .map_err(recover)?;
    if !index.records().contains_key(&input.host_path) {
        return Err(recover(WorkerError::new(
            "missing_host",
            "host note is not indexed",
        )));
    }
    let formulas = parse_formulas(base.get("formulas"), limits).map_err(recover)?;
    let mut budget = Budget::new(limits);
    let mut predicates = [base.get("filters"), view.get("filters")]
        .into_iter()
        .flatten()
        .filter(|value| data_truthy(value))
        .map(FilterPlan::new)
        .collect::<Vec<_>>();
    let evaluator = Evaluator {
        index,
        limits,
        formulas: &formulas,
        environment,
    };
    // Without predicates every indexed record will match. Reserve that known
    // size once instead of growing and copying the row contexts repeatedly.
    let mut matched = if predicates.is_empty() {
        Vec::with_capacity(index.records().len())
    } else {
        Vec::new()
    };
    let mut filter_expressions = HashMap::new();
    let mut reusable_formula_values = None;
    for record in index.records().values() {
        let mut context = EvalContext::new(record, &input.host_path);
        context.formula_values = reusable_formula_values.take();
        let mut include = true;
        for predicate in &mut predicates {
            if !predicate
                .matches(
                    &evaluator,
                    &mut filter_expressions,
                    &mut context,
                    &mut budget,
                )
                .map_err(recover)?
            {
                include = false;
                break;
            }
        }
        if include {
            matched.push(MatchedRow {
                record,
                context,
                natural_key: OnceCell::new(),
            });
        } else if let Some(mut values) = context.formula_values {
            // Recycle the allocation, with fresh per-record state. Matched rows
            // retain their own formula values for sorting and rendering.
            values.clear();
            reusable_formula_values = Some(values);
        }
    }
    let sort_rules = parse_sort(view.get("sort"), limits).map_err(recover)?;
    let sort_rules = sort_rules
        .into_iter()
        .map(|mut rule| {
            rule.expression = rule.expression.with_formulas(&formulas);
            rule
        })
        .collect::<Vec<_>>();
    let order = if sort_rules.is_empty() {
        // Keep path keys together rather than allocating one vector per path.
        // Sorting indices also leaves formula contexts in place.
        let mut keys = Vec::with_capacity(matched.iter().map(|row| row.record.path.len()).sum());
        let mut order = matched
            .iter()
            .enumerate()
            .map(|(ordinal, row)| {
                let start = keys.len();
                append_natural_sort_key(&row.record.path, &mut keys);
                (start..keys.len(), ordinal)
            })
            .collect::<Vec<_>>();
        order.sort_unstable_by(|(left, left_index), (right, right_index)| {
            keys[left.clone()]
                .cmp(&keys[right.clone()])
                .then_with(|| left_index.cmp(right_index))
        });
        order
            .into_iter()
            .map(|(_, ordinal)| ordinal)
            .collect::<Vec<_>>()
    } else {
        let mut columns = sort_rules
            .iter()
            .map(|_| Vec::with_capacity(matched.len()))
            .collect::<Vec<_>>();
        // Preserve row-major evaluation: formulas, eager errors and shared budget
        // exhaustion must occur in the same order even though keys are columnar.
        for row in &mut matched {
            for (rule, column) in sort_rules.iter().zip(&mut columns) {
                column.push(
                    rule.expression
                        .sort_key(&evaluator, &mut row.context, &mut budget)
                        .map_err(recover)?,
                );
            }
        }
        let columns = columns.into_iter().map(SortColumn::new).collect::<Vec<_>>();
        let mut order = (0..matched.len()).collect::<Vec<_>>();
        // Preserve the explicit sort's stable comparison behavior while moving
        // only indices rather than each record's retained evaluation state.
        order.sort_by(|&left_index, &right_index| {
            let left = &matched[left_index];
            let right = &matched[right_index];
            for (index, rule) in sort_rules.iter().enumerate() {
                let order = columns[index].compare(left_index, right_index);
                let order = if rule.descending {
                    order.reverse()
                } else {
                    order
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
            left.natural_key
                .get_or_init(|| natural_sort_key(&left.record.path))
                .cmp(
                    right
                        .natural_key
                        .get_or_init(|| natural_sort_key(&right.record.path)),
                )
                .then_with(|| left_index.cmp(&right_index))
        });
        order
    };
    let columns =
        parse_columns(view.get("order"), base.get("properties"), limits).map_err(recover)?;
    let columns = columns
        .into_iter()
        .map(|mut column| {
            column.expression = column.expression.with_formulas(&formulas);
            column
        })
        .collect::<Vec<_>>();
    let view_limit = parse_view_limit(view.get("limit"), limits.result_rows).map_err(recover)?;
    let matched_count = matched.len();
    let view_count = matched_count.min(view_limit);
    let preview_count = input.preview_rows.min(view_count);
    // Result IDs are generation-scoped so a reindex invalidates every cached row set at once.
    let result_id = format!("r{generation}-{result_sequence}");
    *result_id_allocated = true;
    let mut rendered_rows = Vec::with_capacity(view_count);
    for &index in &order[..view_count] {
        let row = &mut matched[index];
        let mut cells = Vec::with_capacity(columns.len());
        for column in &columns {
            cells.push(
                column
                    .expression
                    .render(&evaluator, &mut row.context, &mut budget)
                    .map_err(recover)?,
            );
        }
        rendered_rows.push(ResultRow {
            path: row.record.path.clone(),
            display_name: row.record.name.clone(),
            cells,
        });
    }
    let public_columns = columns
        .iter()
        .map(|column| Column {
            key: column.key.clone(),
            label: column.label.clone(),
        })
        .collect();
    let result = QueryResult {
        result_id: result_id.clone(),
        source_id: input.source_id,
        view: View {
            name: named_view(view).unwrap_or("table").to_owned(),
            kind: "table",
        },
        available_views: available_views.clone(),
        columns: public_columns,
        preview_rows: rendered_rows[..preview_count].to_vec(),
        matched_count,
        view_count,
        preview_count,
        truncated: matched_count > view_count,
        warnings: Vec::new(),
        timings: BTreeMap::new(),
        index_generation: generation,
    };
    assert_size(&result, limits).map_err(recover)?;
    let rows_bytes = assert_size(
        &RowsForLimit {
            result_id: &result_id,
            rows: &rendered_rows,
        },
        limits,
    )
    .map_err(recover)?;
    Ok(QueryOutput {
        result,
        rows: rendered_rows,
        rows_bytes,
    })
}

/// Keep comparison data dense and select homogeneous comparison semantics once.
/// Mixed columns retain the ordinary pair-dependent comparator. These values
/// are disposable sort keys; cached formulas and rendered values stay untouched.
enum SortKey<'a> {
    Borrowed(ValueRef<'a>),
    Owned(EvalValue),
}

impl SortKey<'_> {
    fn text(&self) -> Option<&str> {
        match self {
            Self::Borrowed(ValueRef::Text(value)) => Some(value),
            Self::Borrowed(ValueRef::Eval(EvalValue::String(value)))
            | Self::Owned(EvalValue::String(value)) => Some(value),
            _ => None,
        }
    }

    fn view(&self) -> ValueRef<'_> {
        match self {
            Self::Borrowed(value) => *value,
            Self::Owned(value) => ValueRef::Eval(value),
        }
    }
}

/// Repeated strings within one sort column need parsing only once. Cache keys
/// borrow the retained keys and disappear before those keys can be released.
fn parsed_column<T: Copy>(
    values: &[SortKey],
    parse: fn(&str) -> Option<T>,
    primitive: impl Fn(&SortKey) -> Option<T>,
) -> Option<Vec<T>> {
    let mut cache = HashMap::new();
    values
        .iter()
        .map(|value| match value.text() {
            Some(text) => *cache.entry(text).or_insert_with(|| parse(text)),
            None => primitive(value),
        })
        .collect()
}

enum SortColumn<'a> {
    Numbers(Vec<f64>),
    Durations(Vec<f64>),
    Dates(Vec<i64>),
    Mixed(Vec<SortKey<'a>>),
}

impl<'a> SortColumn<'a> {
    fn new(values: Vec<SortKey<'a>>) -> Self {
        if values.iter().all(|value| value.view().numeric().is_some()) {
            return Self::Numbers(
                values
                    .into_iter()
                    .map(|value| value.view().numeric().expect("numeric column"))
                    .collect(),
            );
        }
        if let Some(durations) =
            parsed_column(&values, parse_duration, |value| value.view().duration())
        {
            return Self::Durations(durations);
        }
        if let Some(dates) = parsed_column(&values, parse_date, |value| value.view().date()) {
            return Self::Dates(dates);
        }
        Self::Mixed(values)
    }

    fn compare(&self, left: usize, right: usize) -> Ordering {
        match self {
            Self::Numbers(values) => {
                values[left].partial_cmp(&values[right]).unwrap_or_else(|| {
                    natural_cmp(&values[left].to_string(), &values[right].to_string())
                })
            }
            Self::Durations(values) => values[left]
                .partial_cmp(&values[right])
                .unwrap_or(Ordering::Equal),
            Self::Dates(values) => values[left].cmp(&values[right]),
            Self::Mixed(values) => values[left].view().compare(&values[right].view(), false),
        }
    }
}

/// Return whether a view can be rendered by the current table-only UI.
fn renderable_table_view(view: &BTreeMap<String, DataValue>) -> bool {
    string(view.get("type")) == Some("table")
        && !["groupBy", "summaries", "layout"]
            .iter()
            .any(|key| view.contains_key(*key))
}

/// Return the non-empty name that makes a view explicitly selectable.
fn named_view(view: &BTreeMap<String, DataValue>) -> Option<&str> {
    string(view.get("name")).filter(|name| !name.is_empty())
}

/// Return the ordered, unique table views that the client can select.
fn table_views(views: &[&BTreeMap<String, DataValue>]) -> Vec<View> {
    let mut names = std::collections::BTreeSet::new();
    views
        .iter()
        .filter_map(|view| named_view(view))
        .filter(|name| names.insert((*name).to_owned()))
        .map(|name| View {
            name: name.to_owned(),
            kind: "table",
        })
        .collect()
}

/// Attach recovery views only when the complete error data fits the result-byte limit.
fn with_recovery_views(
    error: WorkerError,
    available_views: &[View],
    limits: Limits,
) -> WorkerError {
    let fits = assert_size(
        &ErrorWithViewsForLimit {
            code: error.code,
            message: &error.message,
            available_views,
        },
        limits,
    )
    .is_ok();
    if fits {
        error.with_available_views(available_views.to_vec())
    } else {
        error
    }
}

#[derive(Serialize)]
struct ErrorWithViewsForLimit<'a> {
    code: &'a str,
    message: &'a str,
    available_views: &'a [View],
}

#[derive(Serialize)]
struct RowsForLimit<'a> {
    result_id: &'a str,
    rows: &'a [ResultRow],
}

fn assert_size(value: &impl Serialize, limits: Limits) -> Result<usize> {
    // Use the serializer's exact escaping and field rules without retaining a
    // second copy of the complete payload solely to discover its byte length.
    let mut counter = JsonBytes(0);
    serde_json::to_writer(&mut counter, value)?;
    let bytes = counter.0;
    if bytes > limits.result_bytes {
        Err(WorkerError::new(
            "result_too_large",
            "query result exceeds the configured size limit",
        ))
    } else {
        Ok(bytes)
    }
}

struct JsonBytes(usize);

impl Write for JsonBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("serialized byte count overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A compiled sort expression and its requested direction.
struct SortRule {
    /// Expression evaluated for each matched record.
    expression: PreparedExpression,
    /// Whether this rule reverses the natural comparison order.
    descending: bool,
}

fn parse_sort(value: Option<&DataValue>, limits: Limits) -> Result<Vec<SortRule>> {
    let Some(DataValue::List(rules)) = value else {
        return Ok(Vec::new());
    };
    rules
        .iter()
        .filter_map(DataValue::as_object)
        .filter_map(|rule| {
            rule.get("property")
                .or_else(|| rule.get("column"))
                .and_then(DataValue::as_str)
                .map(|source| (source, string(rule.get("direction")) == Some("DESC")))
        })
        .map(|(source, descending)| {
            Ok(SortRule {
                expression: PreparedExpression::new(parse_expression(source, limits)?),
                descending,
            })
        })
        .collect()
}

/// A compiled column expression used only during evaluation.
struct CompiledColumn {
    key: String,
    label: String,
    /// Expression used to render the cell value.
    expression: PreparedExpression,
}

fn parse_columns(
    order: Option<&DataValue>,
    properties: Option<&DataValue>,
    limits: Limits,
) -> Result<Vec<CompiledColumn>> {
    let keys = match order {
        Some(DataValue::List(values)) => values
            .iter()
            .filter_map(DataValue::as_str)
            .map(str::to_owned)
            .collect(),
        _ => vec!["file.name".into()],
    };
    let properties = properties.and_then(DataValue::as_object);
    keys.into_iter()
        .map(|key| {
            let label = properties
                .and_then(|properties| properties.get(&key))
                .and_then(DataValue::as_object)
                .and_then(|property| property.get("displayName"))
                .and_then(DataValue::as_str)
                .unwrap_or(&key)
                .to_owned();
            Ok(CompiledColumn {
                expression: PreparedExpression::new(parse_expression(&key, limits)?),
                key,
                label,
            })
        })
        .collect()
}

fn parse_formulas(
    value: Option<&DataValue>,
    limits: Limits,
) -> Result<Vec<(String, PreparedExpression)>> {
    let Some(DataValue::Object(values)) = value else {
        return Ok(Vec::new());
    };
    if values.len() > limits.formulas {
        return Err(WorkerError::new(
            "formula_limit",
            "Base has too many formulas",
        ));
    }
    values
        .iter()
        .map(|(key, value)| {
            safe_key(key)?;
            let source = value.as_str().ok_or_else(|| {
                WorkerError::new("invalid_formula", format!("formula {key} must be a string"))
            })?;
            Ok((
                key.clone(),
                PreparedExpression::new(parse_expression(source, limits)?),
            ))
        })
        .collect()
}

fn parse_view_limit(value: Option<&DataValue>, result_rows: usize) -> Result<usize> {
    let Some(value) = value else {
        return Ok(result_rows);
    };
    let Some(number) = value.as_f64() else {
        return invalid_view();
    };
    if number < 0.0 || number.fract() != 0.0 || number > 9_007_199_254_740_991.0 {
        return invalid_view();
    }
    Ok((number as usize).min(result_rows))
}

fn invalid_view<T>() -> Result<T> {
    Err(WorkerError::new(
        "invalid_view",
        "view limit must be a non-negative integer",
    ))
}

fn object_list(value: Option<&DataValue>) -> Vec<&BTreeMap<String, DataValue>> {
    value
        .and_then(DataValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(DataValue::as_object)
        .collect()
}

fn string(value: Option<&DataValue>) -> Option<&str> {
    value.and_then(DataValue::as_str)
}

fn data_truthy(value: &DataValue) -> bool {
    match value {
        DataValue::Null => false,
        DataValue::Bool(value) => *value,
        DataValue::Number(value) => *value != 0.0 && !value.is_nan(),
        DataValue::String(value) => !value.is_empty(),
        _ => true,
    }
}

/// Cumulative step and wall-clock budget shared by one query evaluation.
struct Budget {
    /// Monotonic time at which this query evaluation began.
    started: Instant,
    /// Number of expression nodes evaluated so far.
    steps: u64,
    /// Limits against which elapsed work is checked.
    limits: Limits,
    /// Deterministic clock advancement for deadline-boundary regression tests.
    #[cfg(test)]
    deadline_checks: usize,
    #[cfg(test)]
    expire_after_checks: Option<usize>,
}

impl Budget {
    fn new(limits: Limits) -> Self {
        Self {
            started: Instant::now(),
            steps: 0,
            limits,
            #[cfg(test)]
            deadline_checks: 0,
            #[cfg(test)]
            expire_after_checks: None,
        }
    }

    #[inline]
    fn step(&mut self) -> Result<()> {
        self.steps += 1;
        if self.steps > self.limits.evaluation_steps || self.deadline_expired() {
            Err(evaluation_limit())
        } else {
            Ok(())
        }
    }

    /// Charge direct projection/receiver nodes before accessing their data.
    /// Composite expressions retain per-node checks: a small AST can still
    /// clone or stringify an arbitrarily large property or backlink collection.
    /// On exhaustion, stop at the same first failing step as ordinary evaluation.
    #[inline(always)]
    fn steps(&mut self, count: u64) -> Result<()> {
        let remaining = self.limits.evaluation_steps.saturating_sub(self.steps);
        self.steps = self
            .steps
            .saturating_add(count.min(remaining.saturating_add(1)));
        if count > remaining || self.deadline_expired() {
            Err(evaluation_limit())
        } else {
            Ok(())
        }
    }

    #[inline]
    fn deadline_expired(&mut self) -> bool {
        #[cfg(test)]
        {
            self.deadline_checks += 1;
            if self
                .expire_after_checks
                .is_some_and(|allowed| self.deadline_checks > allowed)
            {
                return true;
            }
        }
        self.started.elapsed().as_millis() > u128::from(self.limits.evaluation_ms)
    }
}

#[cold]
fn evaluation_limit() -> WorkerError {
    WorkerError::new(
        "evaluation_limit",
        "expression evaluation exceeded its limit",
    )
}

/// Per-record state used to cache formulas and expose dynamic expression values.
struct EvalContext<'a> {
    /// Indexed record currently being evaluated.
    current: &'a FileRecord,
    /// Indexed path hosting the Base, exposed as `this`.
    host: &'a str,
    /// Cache only reached formula slots from the sorted query definitions.
    /// Numeric keys avoid copying names; active slots also detect cycles.
    formula_values: Option<HashMap<usize, FormulaValue>>,
    formula_depth: usize,
    /// Dynamic `value` bound while evaluating list callbacks.
    value: Option<EvalValue>,
}

impl<'a> EvalContext<'a> {
    fn new(current: &'a FileRecord, host: &'a str) -> Self {
        Self {
            current,
            host,
            formula_values: None,
            formula_depth: 0,
            value: None,
        }
    }
}

#[derive(Clone)]
enum FormulaValue {
    Active,
    Cached(EvalValue),
}

/// Evaluates expressions against an index under the configured resource limits.
struct Evaluator<'a> {
    /// Immutable index that supplies records and link targets.
    index: &'a Index,
    /// Limits used for filter parsing and formula dependency depth.
    limits: Limits,
    /// Compiled formulas referenced through the `formula` namespace.
    formulas: &'a [(String, PreparedExpression)],
    /// Wall-clock and local-time values captured for this query.
    environment: &'a ExecutionEnvironment,
}

/// A query-local filter tree, prepared only as each branch is first reached.
/// Keeping child nodes avoids re-reading YAML groups and hashing expression
/// sources for every record, while unreachable invalid branches remain lazy.
struct FilterPlan<'a> {
    source: &'a DataValue,
    node: Option<FilterNode<'a>>,
}

enum FilterNode<'a> {
    Expression(Rc<PreparedExpression>),
    All(Vec<FilterPlan<'a>>),
    Any(Vec<FilterPlan<'a>>),
    None(Vec<FilterPlan<'a>>),
}

impl<'a> FilterPlan<'a> {
    fn new(source: &'a DataValue) -> Self {
        Self { source, node: None }
    }

    fn matches(
        &mut self,
        evaluator: &Evaluator,
        expressions: &mut HashMap<&'a str, Rc<PreparedExpression>>,
        context: &mut EvalContext,
        budget: &mut Budget,
    ) -> Result<bool> {
        if self.node.is_none() {
            self.node = Some(match self.source {
                DataValue::String(source) => {
                    let expression = match expressions.entry(source) {
                        Entry::Occupied(entry) => entry.into_mut(),
                        Entry::Vacant(entry) => entry.insert(Rc::new(
                            PreparedExpression::new(parse_expression(source, evaluator.limits)?)
                                .with_formulas(evaluator.formulas),
                        )),
                    };
                    FilterNode::Expression(Rc::clone(expression))
                }
                DataValue::List(values) => FilterNode::All(values.iter().map(Self::new).collect()),
                DataValue::Object(group) => {
                    if group.len() != 1 {
                        return Err(WorkerError::new(
                            "unsupported_filter",
                            "unsupported filter group",
                        ));
                    }
                    let (operator, values) = group.first_key_value().expect("one entry");
                    if !["and", "or", "not"].contains(&operator.as_str()) {
                        return Err(WorkerError::new(
                            "unsupported_filter",
                            "unsupported filter group",
                        ));
                    }
                    if operator == "not" {
                        let children = match values {
                            DataValue::List(values) => values.iter().map(Self::new).collect(),
                            value => vec![Self::new(value)],
                        };
                        FilterNode::None(children)
                    } else {
                        let DataValue::List(values) = values else {
                            return Err(WorkerError::new(
                                "invalid_filter",
                                format!("{operator} filter must be a list"),
                            ));
                        };
                        let children = values.iter().map(Self::new).collect();
                        if operator == "and" {
                            FilterNode::All(children)
                        } else {
                            FilterNode::Any(children)
                        }
                    }
                }
                _ => {
                    return Err(WorkerError::new(
                        "invalid_filter",
                        "filter must be an expression or group",
                    ));
                }
            });
        }
        match self.node.as_mut().expect("prepared above") {
            FilterNode::Expression(expression) => expression.matches(evaluator, context, budget),
            FilterNode::All(children) => {
                for child in children {
                    if !child.matches(evaluator, expressions, context, budget)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            FilterNode::Any(children) => {
                for child in children {
                    if child.matches(evaluator, expressions, context, budget)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            FilterNode::None(children) => {
                for child in children {
                    if child.matches(evaluator, expressions, context, budget)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        }
    }
}

/// Literal tag/folder predicates need no owned runtime arguments or receivers.
/// Other expressions continue through the ordinary evaluator unchanged.
enum PreparedExpression {
    Predicate {
        file: FileNamespace,
        predicate: FilePredicate,
        argument: String,
        steps: u64,
    },
    /// Bound fields borrow input values until a retained result needs ownership.
    Compiled {
        expression: Projection,
        steps: u64,
    },
    FormulaSlot(usize),
    General(Expression),
}

impl PreparedExpression {
    fn evaluate_ref<'a, 'record: 'a>(
        &'a self,
        evaluator: &'a Evaluator,
        context: &mut EvalContext<'record>,
        budget: &mut Budget,
    ) -> Result<ValueRef<'a>> {
        let Self::Compiled { expression, steps } = self else {
            unreachable!("compiled projection")
        };
        budget.steps(*steps)?;
        Ok(expression.evaluate(evaluator, context))
    }

    fn sort_key<'a, 'record: 'a>(
        &'a self,
        evaluator: &'a Evaluator,
        context: &mut EvalContext<'record>,
        budget: &mut Budget,
    ) -> Result<SortKey<'a>> {
        if matches!(self, Self::Compiled { .. }) {
            self.evaluate_ref(evaluator, context, budget)
                .map(SortKey::Borrowed)
        } else {
            self.evaluate(evaluator, context, budget)
                .map(SortKey::Owned)
        }
    }

    fn render(
        &self,
        evaluator: &Evaluator,
        context: &mut EvalContext,
        budget: &mut Budget,
    ) -> Result<Cell> {
        if matches!(self, Self::Compiled { .. }) {
            self.evaluate_ref(evaluator, context, budget)
                .map(ValueRef::into_cell)
        } else {
            self.evaluate(evaluator, context, budget).map(cell)
        }
    }

    fn with_formulas(self, formulas: &[(String, PreparedExpression)]) -> Self {
        match self {
            Self::General(expression) => {
                if let Expression::Member(object, name) = &expression {
                    if matches!(&**object, Expression::Identifier(namespace) if namespace == "formula")
                    {
                        if let Ok(slot) =
                            formulas.binary_search_by(|(current, _)| current.cmp(name))
                        {
                            return Self::FormulaSlot(slot);
                        }
                    }
                }
                Self::General(expression)
            }
            other => other,
        }
    }

    fn new(expression: Expression) -> Self {
        if let Expression::Call(callee, args) = &expression {
            if let Expression::Member(object, method) = &**callee {
                if let (
                    Some(predicate),
                    Some((file, receiver_steps)),
                    [Expression::Literal(Literal::String(argument))],
                ) = (
                    FilePredicate::from_method(method),
                    file_namespace(object),
                    args.as_slice(),
                ) {
                    return Self::Predicate {
                        file,
                        predicate,
                        argument: argument.clone(),
                        steps: receiver_steps as u64 + 3,
                    };
                }
            }
        }
        // Only direct reads can batch their entry checks. Operators may process
        // data-dependent collections, so retain the general evaluator's checks.
        if let Some((projection, steps)) = Projection::compile(&expression) {
            if steps <= 8 {
                return Self::Compiled {
                    expression: projection,
                    steps,
                };
            }
        }
        Self::General(expression)
    }

    #[inline(always)]
    fn matches(
        &self,
        evaluator: &Evaluator,
        context: &mut EvalContext,
        budget: &mut Budget,
    ) -> Result<bool> {
        // Filters consume a boolean directly. Avoid returning a large owned
        // EvalValue for each record when the prepared predicate already has it.
        if let Self::Predicate {
            file,
            predicate,
            argument,
            steps,
        } = self
        {
            budget.steps(*steps)?;
            return Ok(predicate.matches(evaluator.file(file.path(context), context), argument));
        }
        if matches!(self, Self::Compiled { .. }) {
            return Ok(self.evaluate_ref(evaluator, context, budget)?.truthy());
        }
        Ok(self.evaluate(evaluator, context, budget)?.truthy())
    }

    fn evaluate(
        &self,
        evaluator: &Evaluator,
        context: &mut EvalContext,
        budget: &mut Budget,
    ) -> Result<EvalValue> {
        match self {
            Self::Predicate { .. } => self
                .matches(evaluator, context, budget)
                .map(EvalValue::Bool),
            Self::Compiled { .. } => self
                .evaluate_ref(evaluator, context, budget)
                .map(ValueRef::into_owned),
            Self::FormulaSlot(slot) => {
                budget.steps(2)?;
                evaluator.formula_slot(*slot, context, budget)
            }
            Self::General(expression) => evaluator.evaluate(expression, context, budget),
        }
    }
}

#[derive(Clone, Copy)]
enum FileNamespace {
    Current,
    Host,
}

impl FileNamespace {
    fn path<'a>(self, context: &EvalContext<'a>) -> &'a str {
        match self {
            Self::Current => &context.current.path,
            Self::Host => context.host,
        }
    }
}

#[derive(Clone, Copy)]
enum FilePredicate {
    HasTag,
    InFolder,
}

impl FilePredicate {
    fn from_method(method: &str) -> Option<Self> {
        match method {
            "hasTag" => Some(Self::HasTag),
            "inFolder" => Some(Self::InFolder),
            _ => None,
        }
    }

    fn matches(self, record: &FileRecord, argument: &str) -> bool {
        match self {
            Self::HasTag => record.tags.iter().any(|tag| {
                tag == argument
                    || tag
                        .strip_prefix(argument)
                        .is_some_and(|rest| rest.starts_with('/'))
            }),
            Self::InFolder => {
                record.folder == argument
                    || record
                        .folder
                        .strip_prefix(argument)
                        .is_some_and(|rest| rest.starts_with('/'))
            }
        }
    }
}

impl Evaluator<'_> {
    /// Match a filter, compiling each reached source once in the query's cache.
    #[cfg(test)]
    fn matches<'filter>(
        &self,
        filter: &'filter DataValue,
        expressions: &mut HashMap<&'filter str, Expression>,
        context: &mut EvalContext,
        budget: &mut Budget,
    ) -> Result<bool> {
        match filter {
            DataValue::String(source) => {
                let expression = match expressions.entry(source.as_str()) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => entry.insert(parse_expression(source, self.limits)?),
                };
                Ok(self.evaluate(expression, context, budget)?.truthy())
            }
            DataValue::List(values) => {
                for value in values {
                    if !self.matches(value, expressions, context, budget)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            DataValue::Object(group) => {
                if group.len() != 1 {
                    return Err(WorkerError::new(
                        "unsupported_filter",
                        "unsupported filter group",
                    ));
                }
                let (operator, values) = group.first_key_value().expect("one entry");
                if !["and", "or", "not"].contains(&operator.as_str()) {
                    return Err(WorkerError::new(
                        "unsupported_filter",
                        "unsupported filter group",
                    ));
                }
                if operator == "not" {
                    return match values {
                        DataValue::List(values) => {
                            for value in values {
                                if self.matches(value, expressions, context, budget)? {
                                    return Ok(false);
                                }
                            }
                            Ok(true)
                        }
                        value => Ok(!self.matches(value, expressions, context, budget)?),
                    };
                }
                let DataValue::List(values) = values else {
                    return Err(WorkerError::new(
                        "invalid_filter",
                        format!("{operator} filter must be a list"),
                    ));
                };
                if operator == "and" {
                    for value in values {
                        if !self.matches(value, expressions, context, budget)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                } else {
                    for value in values {
                        if self.matches(value, expressions, context, budget)? {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                }
            }
            _ => Err(WorkerError::new(
                "invalid_filter",
                "filter must be an expression or group",
            )),
        }
    }

    fn evaluate(
        &self,
        expression: &Expression,
        context: &mut EvalContext,
        budget: &mut Budget,
    ) -> Result<EvalValue> {
        budget.step()?;
        match expression {
            Expression::Literal(value) => Ok(literal(value)),
            Expression::Identifier(name) => Ok(self.identifier(name, context)),
            Expression::Member(object, property) => {
                // Select directly from known namespaces without materializing an
                // object or owned file path. Still charge the identifier's step.
                if let Expression::Identifier(name) = &**object {
                    if name == "property" && property != "isEmpty" {
                        budget.step()?;
                        return Ok(context
                            .current
                            .properties
                            .get(property)
                            .map(EvalValue::from)
                            .unwrap_or(EvalValue::Empty));
                    }
                    if matches!(name.as_str(), "file" | "note" | "this") {
                        budget.step()?;
                        let path = if name == "this" {
                            context.host
                        } else {
                            &context.current.path
                        };
                        return Ok(file_member(self.file(path, context), path, property));
                    }
                }
                let value = self.evaluate(object, context, budget)?;
                self.member(value, property, context, budget)
            }
            Expression::Index(object, index) => {
                let object = self.evaluate(object, context, budget)?;
                let index = self.evaluate(index, context, budget)?.number();
                if let EvalValue::List(values) = object {
                    if index >= 0.0 && index.fract() == 0.0 {
                        Ok(values
                            .get(index as usize)
                            .cloned()
                            .unwrap_or(EvalValue::Empty))
                    } else {
                        Ok(EvalValue::Empty)
                    }
                } else {
                    Ok(EvalValue::Empty)
                }
            }
            Expression::Unary(operator, argument) => {
                let value = self.evaluate(argument, context, budget)?;
                Ok(unary(operator, value))
            }
            Expression::Binary(operator, left, right) => {
                let left = self.evaluate(left, context, budget)?;
                let right = self.evaluate(right, context, budget)?;
                Ok(binary(operator, left, right))
            }
            Expression::Call(callee, args) => {
                // Known file receivers can stay borrowed through a method call.
                // Evaluate every argument eagerly and charge all receiver nodes,
                // just as the ordinary owned Method path below does.
                if let Expression::Member(object, property) = &**callee {
                    if matches!(property.as_str(), "hasTag" | "inFolder" | "asLink") {
                        if let Some((path, receiver_steps)) = file_reference(object, context) {
                            budget.step()?; // The callee member itself.
                            for _ in 0..receiver_steps {
                                budget.step()?;
                            }
                            let args = args
                                .iter()
                                .map(|argument| self.evaluate(argument, context, budget))
                                .collect::<Result<Vec<_>>>()?;
                            return Ok(call_file_method(
                                self.file(path, context),
                                path,
                                property,
                                &args,
                            ));
                        }
                    }
                }
                let target = self.evaluate(callee, context, budget)?;
                // Ordinary Bases calls follow JavaScript's eager, left-to-right argument
                // evaluation. `filter` and `map` are expression-language special forms:
                // their argument must remain an AST so it can be re-evaluated with a new
                // dynamic `value` for each list item.
                if callback_call(&target) {
                    self.call_callback(target, args, context, budget)
                } else {
                    let args = args
                        .iter()
                        .map(|argument| self.evaluate(argument, context, budget))
                        .collect::<Result<Vec<_>>>()?;
                    self.call_eager(target, &args, context)
                }
            }
        }
    }

    fn identifier(&self, name: &str, context: &EvalContext) -> EvalValue {
        if name == "file" || name == "note" {
            return EvalValue::File(context.current.path.clone());
        }
        if name == "this" {
            return EvalValue::File(context.host.to_owned());
        }
        if name == "formula" {
            return EvalValue::Formula;
        }
        if name == "property" {
            return EvalValue::Object(context.current.properties.clone());
        }
        if name == "value" {
            return context.value.clone().unwrap_or(EvalValue::Empty);
        }
        if name == "date" || name == "today" {
            return EvalValue::Function(name.into());
        }
        context
            .current
            .properties
            .get(name)
            .map(EvalValue::from)
            .unwrap_or(EvalValue::Empty)
    }

    fn member(
        &self,
        value: EvalValue,
        property: &str,
        context: &mut EvalContext,
        budget: &mut Budget,
    ) -> Result<EvalValue> {
        match value {
            EvalValue::File(path) => Ok(file_member(self.file(&path, context), &path, property)),
            EvalValue::Link { .. } if property == "asFile" => Ok(EvalValue::Method {
                target: Box::new(value),
                property: property.into(),
            }),
            EvalValue::Date(_) if property == "date" => Ok(EvalValue::Method {
                target: Box::new(value),
                property: property.into(),
            }),
            EvalValue::Duration(milliseconds) if property == "days" => {
                Ok(EvalValue::Number(milliseconds / 86_400_000.0))
            }
            EvalValue::Number(_) if property == "round" => Ok(EvalValue::Method {
                target: Box::new(value),
                property: property.into(),
            }),
            EvalValue::Formula => self.formula(property, context, budget),
            _ if property == "isEmpty" => Ok(EvalValue::Method {
                target: Box::new(value),
                property: property.into(),
            }),
            EvalValue::List(_) if ["filter", "map", "sort", "slice"].contains(&property) => {
                Ok(EvalValue::Method {
                    target: Box::new(value),
                    property: property.into(),
                })
            }
            EvalValue::Object(object) => Ok(object
                .get(property)
                .map(EvalValue::from)
                .unwrap_or(EvalValue::Empty)),
            _ => Ok(EvalValue::Empty),
        }
    }

    /// Dispatch an ordinary builtin after all of its arguments have been evaluated.
    fn call_eager(
        &self,
        target: EvalValue,
        args: &[EvalValue],
        context: &EvalContext,
    ) -> Result<EvalValue> {
        if let EvalValue::Function(name) = &target {
            if name == "date" {
                let value = argument(args, 0);
                return Ok(to_date(value).map_or(EvalValue::Empty, EvalValue::Date));
            }
            if name == "today" {
                return Ok(EvalValue::Date(self.environment.today_milliseconds()));
            }
        }
        let EvalValue::Method { target, property } = target else {
            return Ok(EvalValue::Empty);
        };
        match property.as_str() {
            "date" => Ok(*target),
            "round" => {
                let EvalValue::Number(value) = *target else {
                    return Ok(EvalValue::Empty);
                };
                let digits = if args.is_empty() {
                    0.0
                } else {
                    argument(args, 0).number()
                };
                Ok(EvalValue::Number(round_number(value, digits)))
            }
            "isEmpty" => Ok(EvalValue::Bool(
                matches!(*target, EvalValue::Empty | EvalValue::Null)
                    || matches!(&*target, EvalValue::String(value) if value.is_empty())
                    || matches!(&*target, EvalValue::List(value) if value.is_empty()),
            )),
            "hasTag" | "inFolder" | "asLink" => {
                let EvalValue::File(path) = *target else {
                    return Ok(EvalValue::Empty);
                };
                Ok(call_file_method(
                    self.file(&path, context),
                    &path,
                    &property,
                    args,
                ))
            }
            "asFile" => {
                let EvalValue::Link { path, .. } = *target else {
                    return Ok(EvalValue::Empty);
                };
                Ok(if self.index.records().contains_key(&path) {
                    EvalValue::File(path)
                } else {
                    EvalValue::Empty
                })
            }
            "sort" => {
                let EvalValue::List(mut values) = *target else {
                    return Ok(EvalValue::Empty);
                };
                let numeric_sort = values
                    .iter()
                    .all(|value| matches!(value, EvalValue::Number(_)));
                values.sort_by(|left, right| compare_with_numeric_order(left, right, numeric_sort));
                Ok(EvalValue::List(values))
            }
            "slice" => {
                let EvalValue::List(values) = *target else {
                    return Ok(EvalValue::Empty);
                };
                let start = args.first().map(EvalValue::number).unwrap_or(0.0);
                let end = args.get(1).map(EvalValue::number);
                let start = slice_index(start, values.len());
                let end = end.map_or(values.len(), |end| slice_index(end, values.len()));
                Ok(EvalValue::List(if start <= end {
                    values[start..end].to_vec()
                } else {
                    Vec::new()
                }))
            }
            _ => Ok(EvalValue::Empty),
        }
    }

    /// Evaluate the one expression accepted by a list callback for every list item.
    fn call_callback(
        &self,
        target: EvalValue,
        args: &[Expression],
        context: &mut EvalContext,
        budget: &mut Budget,
    ) -> Result<EvalValue> {
        let EvalValue::Method { target, property } = target else {
            return Ok(EvalValue::Empty);
        };
        let EvalValue::List(values) = *target else {
            return Ok(EvalValue::Empty);
        };
        if args.len() != 1 {
            return Err(WorkerError::new(
                "invalid_arguments",
                format!("{property} expects exactly one expression"),
            ));
        }
        let expression = &args[0];
        // `value` is dynamically scoped within a list callback and must be restored for
        // the surrounding expression, including when callback evaluation fails.
        let previous = context.value.clone();
        let result = (|| {
            let mut result = Vec::new();
            for item in values {
                context.value = Some(item.clone());
                let mapped = self.evaluate(expression, context, budget)?;
                if property == "map" {
                    result.push(mapped);
                } else if mapped.truthy() {
                    result.push(item);
                }
            }
            Ok(EvalValue::List(result))
        })();
        context.value = previous;
        result
    }

    fn formula(
        &self,
        name: &str,
        context: &mut EvalContext,
        budget: &mut Budget,
    ) -> Result<EvalValue> {
        let Ok(slot) = self
            .formulas
            .binary_search_by(|(current, _)| current.as_str().cmp(name))
        else {
            return Ok(EvalValue::Empty);
        };
        self.formula_slot(slot, context, budget)
    }

    fn formula_slot(
        &self,
        slot: usize,
        context: &mut EvalContext,
        budget: &mut Budget,
    ) -> Result<EvalValue> {
        let name = &self.formulas[slot].0;
        match context
            .formula_values
            .as_ref()
            .and_then(|values| values.get(&slot))
        {
            Some(FormulaValue::Cached(value)) => return Ok(value.clone()),
            Some(FormulaValue::Active) => {
                return Err(WorkerError::new(
                    "formula_cycle",
                    format!("formula cycle at {name}"),
                ));
            }
            None => {}
        }
        if context.formula_depth >= self.limits.formula_depth {
            return Err(WorkerError::new(
                "formula_limit",
                "formula dependency depth exceeds its limit",
            ));
        }
        context
            .formula_values
            .get_or_insert_with(HashMap::new)
            .insert(slot, FormulaValue::Active);
        context.formula_depth += 1;
        let value = self.formulas[slot].1.evaluate(self, context, budget);
        context.formula_depth -= 1;
        match value {
            Ok(value) => {
                context
                    .formula_values
                    .as_mut()
                    .expect("active formula cache")
                    .insert(slot, FormulaValue::Cached(value.clone()));
                Ok(value)
            }
            Err(error) => {
                context
                    .formula_values
                    .as_mut()
                    .expect("active formula cache")
                    .remove(&slot);
                Err(error)
            }
        }
    }

    fn file<'a>(&'a self, path: &str, context: &EvalContext<'a>) -> &'a FileRecord {
        if path == context.current.path {
            context.current
        } else {
            &self.index.records()[path]
        }
    }
}

/// Resolve only file namespaces and their `.file` aliases, counting AST nodes
/// so the borrowed-call path preserves the exact expression budget.
fn file_reference<'a>(
    expression: &Expression,
    context: &EvalContext<'a>,
) -> Option<(&'a str, usize)> {
    file_namespace(expression).map(|(file, steps)| (file.path(context), steps))
}

fn file_namespace(mut expression: &Expression) -> Option<(FileNamespace, usize)> {
    let mut steps = 1;
    while let Expression::Member(object, property) = expression {
        if property != "file" {
            return None;
        }
        expression = object;
        steps += 1;
    }
    match expression {
        Expression::Identifier(name) if matches!(name.as_str(), "file" | "note") => {
            Some((FileNamespace::Current, steps))
        }
        Expression::Identifier(name) if name == "this" => Some((FileNamespace::Host, steps)),
        _ => None,
    }
}

/// Shared file-method semantics for borrowed namespaces and dynamic receivers.
fn call_file_method(
    record: &FileRecord,
    path: &str,
    property: &str,
    args: &[EvalValue],
) -> EvalValue {
    match property {
        "hasTag" => {
            let wanted = argument(args, 0).string();
            EvalValue::Bool(FilePredicate::HasTag.matches(record, &wanted))
        }
        "inFolder" => {
            let wanted = argument(args, 0).string();
            EvalValue::Bool(FilePredicate::InFolder.matches(record, &wanted))
        }
        "asLink" => EvalValue::Link {
            path: path.to_owned(),
            display: Some(if args.is_empty() {
                record.name.clone()
            } else {
                argument(args, 0).string()
            }),
        },
        _ => EvalValue::Empty,
    }
}

/// File namespace semantics shared by direct and dynamically resolved access.
fn file_member(record: &FileRecord, path: &str, property: &str) -> EvalValue {
    if let Some(field) = projection::FileField::named(property) {
        return field.read(record, path, property).into_owned();
    }
    match property {
        "backlinks" => EvalValue::List(
            record
                .backlinks
                .iter()
                .map(|path| EvalValue::Link {
                    path: path.clone(),
                    display: None,
                })
                .collect(),
        ),
        "hasTag" | "inFolder" | "asLink" => EvalValue::Method {
            target: Box::new(EvalValue::File(path.to_owned())),
            property: property.into(),
        },
        _ => unreachable!("all direct file fields are classified above"),
    }
}

/// Return whether a resolved method uses callback-expression evaluation.
fn callback_call(target: &EvalValue) -> bool {
    matches!(
        target,
        EvalValue::Method { property, .. } if property == "filter" || property == "map"
    )
}

fn argument(args: &[EvalValue], index: usize) -> &EvalValue {
    args.get(index).unwrap_or(&EvalValue::Empty)
}

fn literal(value: &Literal) -> EvalValue {
    match value {
        Literal::String(value) => EvalValue::String(value.clone()),
        Literal::Number(value) => EvalValue::Number(*value),
        Literal::Boolean(value) => EvalValue::Bool(*value),
        Literal::Null => EvalValue::Null,
    }
}

fn unary(operator: &str, value: EvalValue) -> EvalValue {
    if operator == "-" {
        EvalValue::Number(-value.number())
    } else {
        EvalValue::Bool(!value.truthy())
    }
}

fn binary(operator: &str, left: EvalValue, right: EvalValue) -> EvalValue {
    match operator {
        "&&" | "and" => EvalValue::Bool(left.truthy() && right.truthy()),
        "||" | "or" => EvalValue::Bool(left.truthy() || right.truthy()),
        "-" => match (to_date(&left), to_date(&right)) {
            (Some(left), Some(right)) => EvalValue::Duration((left - right) as f64),
            _ => EvalValue::Number(left.number() - right.number()),
        },
        "+" => EvalValue::Number(left.number() + right.number()),
        "*" => EvalValue::Number(left.number() * right.number()),
        "/" => EvalValue::Number(left.number() / right.number()),
        operator => {
            let order = compare(&left, &right);
            EvalValue::Bool(match operator {
                ">" => order == Ordering::Greater,
                ">=" => order != Ordering::Less,
                "<" => order == Ordering::Less,
                "<=" => order != Ordering::Greater,
                "==" => order == Ordering::Equal,
                _ => order != Ordering::Equal,
            })
        }
    }
}

fn compare(left: &EvalValue, right: &EvalValue) -> Ordering {
    compare_with_numeric_order(left, right, true)
}

/// Mixed sort keys keep natural order so the numeric rule cannot form a comparison cycle.
fn compare_with_numeric_order(
    left: &EvalValue,
    right: &EvalValue,
    numeric_order: bool,
) -> Ordering {
    ValueRef::Eval(left).compare(&ValueRef::Eval(right), numeric_order)
}

fn to_date(value: &EvalValue) -> Option<i64> {
    match value {
        EvalValue::Date(value) => Some(*value),
        EvalValue::String(value) => parse_date(value),
        _ => None,
    }
}

fn to_duration(value: &EvalValue) -> Option<f64> {
    match value {
        EvalValue::Duration(value) => Some(*value),
        EvalValue::String(value) => parse_duration(value),
        _ => None,
    }
}

fn slice_index(number: f64, length: usize) -> usize {
    let number = if number.is_nan() {
        0
    } else {
        number.trunc() as isize
    };
    if number < 0 {
        length.saturating_sub(number.unsigned_abs())
    } else {
        usize::try_from(number).unwrap_or(usize::MAX).min(length)
    }
}

fn cell(value: EvalValue) -> Cell {
    match value {
        EvalValue::Link { path, display } => {
            let display = display.unwrap_or_else(|| {
                path.rsplit('/')
                    .next()
                    .unwrap_or(&path)
                    .rsplit_once('.')
                    .map_or_else(|| path.clone(), |(name, _)| name.to_owned())
            });
            Cell {
                kind: CellKind::Link,
                text: sanitize_owned_text(display),
                target: Some(path),
            }
        }
        EvalValue::Date(value) => Cell {
            kind: CellKind::Date,
            text: format_utc_date(value),
            target: None,
        },
        EvalValue::Duration(value) => Cell {
            kind: CellKind::Duration,
            text: format!("{} days", (value / 86_400_000.0).round()),
            target: None,
        },
        EvalValue::Empty | EvalValue::Null => Cell {
            kind: CellKind::Empty,
            text: String::new(),
            target: None,
        },
        EvalValue::Bool(value) => Cell {
            kind: CellKind::Boolean,
            text: value.to_string(),
            target: None,
        },
        EvalValue::Number(value) => Cell {
            kind: CellKind::Number,
            text: EvalValue::Number(value).string(),
            target: None,
        },
        EvalValue::String(value) => Cell {
            kind: CellKind::String,
            text: sanitize_owned_text(value),
            target: None,
        },
        value => Cell {
            kind: CellKind::Object,
            text: sanitize_owned_text(value.string()),
            target: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn note(path: &str) -> FileRecord {
        let (folder, basename) = path.rsplit_once('/').unwrap_or(("", path));
        let name = basename.strip_suffix(".md").unwrap();
        FileRecord {
            path: path.into(),
            name: name.into(),
            basename: basename.into(),
            folder: folder.into(),
            extension: "md".into(),
            ctime: 0,
            mtime: 0,
            properties: BTreeMap::new(),
            tags: Vec::new(),
            links: Vec::new(),
            backlinks: Vec::new(),
        }
    }

    fn compile_formulas(
        formulas: &BTreeMap<String, Expression>,
    ) -> Vec<(String, PreparedExpression)> {
        formulas
            .iter()
            .map(|(name, expression)| (name.clone(), PreparedExpression::new(expression.clone())))
            .collect()
    }

    #[test]
    fn prepared_expressions_recheck_deadlines_after_data_dependent_operands() {
        let limits = Limits::default();
        let mut index = Index::empty(PathBuf::from("/vault"));
        index.insert_test_record(
            "Host.md".into(),
            FileRecord {
                properties: BTreeMap::from([(
                    "items".into(),
                    DataValue::List(vec![DataValue::String("item".into()); 2_048]),
                )]),
                backlinks: vec!["Source.md".into(); 2_048],
                ..note("Host.md")
            }
            .into(),
        );
        let formulas = BTreeMap::from([(
            "collection".into(),
            parse_expression("file.backlinks", limits).unwrap(),
        )]);
        let compiled = compile_formulas(&formulas);
        // Keep formula bodies on the ordinary AST path in the oracle too,
        // rather than sharing any prepared body's deadline batching.
        let reference_formulas = formulas
            .iter()
            .map(|(name, body)| (name.clone(), PreparedExpression::General(body.clone())))
            .collect::<Vec<_>>();
        let environment = ExecutionEnvironment::capture();
        let evaluator = Evaluator {
            index: &index,
            limits,
            formulas: &compiled,
            environment: &environment,
        };
        let reference = Evaluator {
            formulas: &reference_formulas,
            ..evaluator
        };
        for (source, allowed_checks) in [
            ("file.backlinks + 1", 3),
            ("items + 1", 2),
            ("property + 1", 2),
            ("formula.collection + 1", 5),
        ] {
            let expression = parse_expression(source, limits).unwrap();
            let prepared = PreparedExpression::new(expression.clone()).with_formulas(&compiled);
            for optimized in [false, true] {
                let mut budget = Budget::new(limits);
                // Expire at the next boundary after the collection operand's
                // entry checks. This simulates time spent processing its data,
                // without sleeps, hardware-speed assumptions or large fixtures.
                budget.expire_after_checks = Some(allowed_checks);
                let mut context = EvalContext::new(&index.records()["Host.md"], "Host.md");
                let result = if optimized {
                    prepared.evaluate(&evaluator, &mut context, &mut budget)
                } else {
                    reference.evaluate(&expression, &mut context, &mut budget)
                };
                assert_eq!(
                    result.unwrap_err().code,
                    "evaluation_limit",
                    "{source}, optimized={optimized}"
                );
            }
        }

        for source in [
            "(file.backlinks + 1) + 2",
            "items[0] == 1",
            "items.map(value == 1)",
            "items.slice(0, 2).map(items.slice(0, 2).filter(value == 1))",
            "file.asLink(items)",
            "formula.collection + formula.collection",
        ] {
            let expression = parse_expression(source, limits).unwrap();
            let prepared = PreparedExpression::new(expression.clone()).with_formulas(&compiled);
            // Measure ordinary entry-check positions without a time dependency,
            // then expire immediately before the final node. Cover both cold
            // and already cached collection formulas in a fresh query context.
            for cached in [false, true] {
                let mut reference_context =
                    EvalContext::new(&index.records()["Host.md"], "Host.md");
                let mut actual_context = EvalContext::new(&index.records()["Host.md"], "Host.md");
                if cached {
                    reference
                        .formula(
                            "collection",
                            &mut reference_context,
                            &mut Budget::new(limits),
                        )
                        .unwrap();
                    evaluator
                        .formula("collection", &mut actual_context, &mut Budget::new(limits))
                        .unwrap();
                }
                let mut probe_context = EvalContext::new(&index.records()["Host.md"], "Host.md");
                if cached {
                    reference
                        .formula("collection", &mut probe_context, &mut Budget::new(limits))
                        .unwrap();
                }
                let mut probe = Budget::new(limits);
                reference
                    .evaluate(&expression, &mut probe_context, &mut probe)
                    .unwrap();
                let allowed = probe.deadline_checks - 1;
                let mut expected_budget = Budget::new(limits);
                expected_budget.expire_after_checks = Some(allowed);
                let mut actual_budget = Budget::new(limits);
                actual_budget.expire_after_checks = Some(allowed);
                let expected = reference
                    .evaluate(&expression, &mut reference_context, &mut expected_budget)
                    .unwrap_err();
                let actual = prepared
                    .evaluate(&evaluator, &mut actual_context, &mut actual_budget)
                    .unwrap_err();
                assert_eq!(
                    expected.code, "evaluation_limit",
                    "{source}, cached={cached}"
                );
                assert_eq!(actual.code, expected.code, "{source}, cached={cached}");
                assert_eq!(
                    actual_budget.steps, expected_budget.steps,
                    "{source}, cached={cached}"
                );
            }
        }
    }

    #[test]
    fn counted_json_preserves_escaping_and_exact_limit_boundaries() {
        let cells = (0..=0x9f)
            .filter_map(char::from_u32)
            .map(|character| Cell {
                kind: CellKind::String,
                text: format!("{character}\"\\é😀\n"),
                target: Some(format!("folder/{character}.md")),
            })
            .collect::<Vec<_>>();
        let payload = RowsForLimit {
            result_id: "r\"é-1",
            rows: &[ResultRow {
                path: "folder/日本語.md".into(),
                display_name: "name\t\r".into(),
                cells,
            }],
        };
        let expected = serde_json::to_vec(&payload).unwrap().len();
        for limit in [expected - 1, expected, expected + 1] {
            let actual = assert_size(
                &payload,
                Limits {
                    result_bytes: limit,
                    ..Limits::default()
                },
            );
            if limit < expected {
                assert_eq!(actual.unwrap_err().code, "result_too_large");
            } else {
                assert_eq!(actual.unwrap(), expected);
            }
        }
    }

    #[test]
    fn planned_filters_match_reference_results_errors_and_step_counts() {
        let mut index = Index::empty(PathBuf::from("/vault"));
        for (name, folder, tags) in [
            ("First", "Projects/Sub", vec!["#work/project".to_owned()]),
            ("Second", "Projects", Vec::new()),
        ] {
            index.insert_test_record(
                format!("{name}.md"),
                FileRecord {
                    folder: folder.into(),
                    tags,
                    ..note(&format!("{name}.md"))
                }
                .into(),
            );
        }
        let limits = Limits::default();
        let formulas = [
            ("cycle", "formula.cycle"),
            ("score", "1 + 2"),
            ("other", "formula.score + 1"),
            ("callback", "file.backlinks.map(value)"),
        ]
        .into_iter()
        .map(|(name, source)| (name.into(), parse_expression(source, limits).unwrap()))
        .collect();
        let environment = ExecutionEnvironment::capture();
        let evaluator = Evaluator {
            index: &index,
            limits,
            formulas: &compile_formulas(&formulas),
            environment: &environment,
        };
        for source in [
            "42",
            "null",
            "-2 + 3 * 4",
            "!false",
            "note.name",
            "this.file.name",
            "property.missing",
            "property.isEmpty",
            "file.backlinks",
            "file.hasTag",
            "file.file",
            "property",
            "value",
            "file.inFolder(\"Projects\")",
            r#""2 days" - "1 day""#,
            r#"file.name == "First""#,
            "formula.cycle",
            "formula.score < 500",
            "formula.score + formula.score",
            "formula.other < 500",
            "formula.callback",
            "1 + 2 + 3 + 4 + 5 + 6",
            "file.backlinks.map(value)",
        ] {
            let expression = parse_expression(source, limits).unwrap();
            let prepared =
                PreparedExpression::new(expression.clone()).with_formulas(evaluator.formulas);
            for steps in 0..=40 {
                let limits = Limits {
                    evaluation_steps: steps,
                    ..limits
                };
                let mut reference_budget = Budget::new(limits);
                let mut prepared_budget = Budget::new(limits);
                let mut reference_context =
                    EvalContext::new(&index.records()["First.md"], "Second.md");
                let mut prepared_context =
                    EvalContext::new(&index.records()["First.md"], "Second.md");
                let reference = evaluator
                    .evaluate(&expression, &mut reference_context, &mut reference_budget)
                    .map(|value| format!("{value:?}"))
                    .map_err(|error| error.code);
                let actual = prepared
                    .evaluate(&evaluator, &mut prepared_context, &mut prepared_budget)
                    .map(|value| format!("{value:?}"))
                    .map_err(|error| error.code);
                assert_eq!(actual, reference, "{source}, budget {steps}");
                assert_eq!(
                    prepared_budget.steps, reference_budget.steps,
                    "{source}, budget {steps}"
                );
            }
        }
        for source in [
            "'file.hasTag(\"#work\")'",
            "'file.file.file.hasTag(\"#work/project\")'",
            "'note.hasTag(\"#wor\")'",
            "'this.file.hasTag(\"#work\")'",
            "'file.inFolder(\"Projects\")'",
            "'file.inFolder(\"Project\")'",
            "'file.inFolder(\"Projects/\")'",
            "'file.inFolder(\"\")'",
            "'file.hasTag()'",
            "'file.hasTag(null)'",
            "'file.hasTag(value)'",
            "'file.hasTag(\"#work\", formula.cycle)'",
            "'file.name.hasTag(\"#work\")'",
            "['false', '(']",
            "{or: ['true', '(']}",
            "{not: ['true', {unsupported: []}]}",
            "{and: []}",
            "{or: []}",
            "{not: []}",
            "{not: 'false'}",
            "{and: false}",
            "{or: 'true'}",
            "{unsupported: []}",
            "{and: [], or: []}",
            "[12]",
            "[true]",
            "['file.name != \"Second\"', '(']",
            "{or: ['file.hasTag(\"#work\")', '(']}",
            "['file.inFolder(\"Projects\")', 'this.hasTag(\"#work\")']",
        ] {
            let base = crate::yaml::parse_mapping(&format!("filters: {source}\n"), limits).unwrap();
            let filter = &base["filters"];
            for steps in 1..=40 {
                let mut plan = FilterPlan::new(filter);
                let mut planned_expressions = HashMap::new();
                let mut reference_expressions = HashMap::new();
                let budget_limits = Limits {
                    evaluation_steps: steps,
                    ..limits
                };
                let mut planned_budget = Budget::new(budget_limits);
                let mut reference_budget = Budget::new(budget_limits);
                for record in index.records().values() {
                    let mut planned_context = EvalContext::new(record, "First.md");
                    let mut reference_context = EvalContext::new(record, "First.md");
                    planned_context.value = Some(EvalValue::String("#work".into()));
                    reference_context.value = planned_context.value.clone();
                    let planned = plan
                        .matches(
                            &evaluator,
                            &mut planned_expressions,
                            &mut planned_context,
                            &mut planned_budget,
                        )
                        .map_err(|error| error.code);
                    let reference = evaluator
                        .matches(
                            filter,
                            &mut reference_expressions,
                            &mut reference_context,
                            &mut reference_budget,
                        )
                        .map_err(|error| error.code);
                    assert_eq!(planned, reference, "{source}, budget {steps}");
                    assert_eq!(
                        planned_budget.steps, reference_budget.steps,
                        "{source}, budget {steps}"
                    );
                    if planned.is_err() {
                        break;
                    }
                }
            }
        }
    }

    #[test]
    fn prepared_predicates_keep_parse_limits_and_expired_deadlines() {
        let mut index = Index::empty(PathBuf::from("/vault"));
        index.insert_test_record("Host.md".into(), note("Host.md").into());
        let formulas = BTreeMap::new();
        let environment = ExecutionEnvironment::capture();
        let source = DataValue::String("file.hasTag(\"#work\")".into());
        for (limits, code) in [
            (
                Limits {
                    expression_bytes: 3,
                    ..Limits::default()
                },
                "expression_too_large",
            ),
            (
                Limits {
                    ast_nodes: 1,
                    ..Limits::default()
                },
                "ast_too_large",
            ),
            (
                Limits {
                    ast_depth: 1,
                    ..Limits::default()
                },
                "ast_too_deep",
            ),
            (Limits::default(), "evaluation_limit"),
        ] {
            let evaluator = Evaluator {
                index: &index,
                limits,
                formulas: &compile_formulas(&formulas),
                environment: &environment,
            };
            let mut budget = Budget::new(limits);
            budget.started -= std::time::Duration::from_millis(limits.evaluation_ms + 10);
            let error = FilterPlan::new(&source)
                .matches(
                    &evaluator,
                    &mut HashMap::new(),
                    &mut EvalContext::new(&index.records()["Host.md"], "Host.md"),
                    &mut budget,
                )
                .unwrap_err();
            assert_eq!(error.code, code);
        }
    }

    #[test]
    fn borrowed_file_calls_match_dynamic_dispatch_and_exact_budgets() {
        let mut index = Index::empty(PathBuf::from("/vault"));
        let record = FileRecord {
            tags: vec!["#work/project".into()],
            ..note("Projects/Sub/Current.md")
        };
        index.insert_test_record(record.path.clone(), record.into());
        let mut host = (*index.records()["Projects/Sub/Current.md"]).clone();
        host.path = "Host.md".into();
        host.name = "Host".into();
        host.folder.clear();
        host.tags.clear();
        index.insert_test_record(host.path.clone(), host.into());
        let limits = Limits::default();
        let formulas = BTreeMap::from([(
            "cycle".into(),
            parse_expression("formula.cycle", limits).unwrap(),
        )]);
        let environment = ExecutionEnvironment::capture();
        let evaluator = Evaluator {
            index: &index,
            limits,
            formulas: &compile_formulas(&formulas),
            environment: &environment,
        };
        for source in [
            "file.hasTag(\"#work\")",
            "file.file.file.hasTag(\"#work/project\")",
            "note.hasTag(\"#wor\")",
            "this.file.hasTag(\"#work\")",
            "file.hasTag()",
            "file.hasTag(null)",
            "file.hasTag(value)",
            "file.asLink().asFile().hasTag(\"#work\")",
            "file.name.hasTag(\"#work\")",
            "property.file.hasTag(\"#work\")",
            "file.inFolder(\"Projects\")",
            "file.inFolder(\"Project\")",
            "file.inFolder(\"Projects/\")",
            "this.inFolder(\"\")",
            "file.inFolder()",
            "file.inFolder(null)",
            "file.asLink()",
            "this.file.asLink(\"label\")",
            "note.asLink(null)",
            "file.hasTag(\"#work\", formula.cycle)",
            "file.inFolder(formula.cycle, \"ignored\")",
            "file.asLink(\"label\", formula.cycle)",
        ] {
            let expression = parse_expression(source, limits).unwrap();
            let Expression::Call(callee, arguments) = &expression else {
                panic!("expected call")
            };
            // Compare optimized evaluation with the ordinary dynamic method
            // path, at every nearby budget boundary and with eager errors.
            for steps in 1..=12 {
                let mut actual_context =
                    EvalContext::new(&index.records()["Projects/Sub/Current.md"], "Host.md");
                actual_context.value = Some(EvalValue::String("#work".into()));
                let mut expected_context =
                    EvalContext::new(&index.records()["Projects/Sub/Current.md"], "Host.md");
                expected_context.value = actual_context.value.clone();
                let budget_limits = Limits {
                    evaluation_steps: steps,
                    ..limits
                };
                let mut actual_budget = Budget::new(budget_limits);
                let mut expected_budget = Budget::new(budget_limits);
                let actual = evaluator
                    .evaluate(&expression, &mut actual_context, &mut actual_budget)
                    .map(cell)
                    .map_err(|error| error.code);
                let expected = (|| {
                    expected_budget.step()?; // The outer Call node.
                    let target =
                        evaluator.evaluate(callee, &mut expected_context, &mut expected_budget)?;
                    let args = arguments
                        .iter()
                        .map(|argument| {
                            evaluator.evaluate(
                                argument,
                                &mut expected_context,
                                &mut expected_budget,
                            )
                        })
                        .collect::<Result<Vec<_>>>()?;
                    evaluator.call_eager(target, &args, &expected_context)
                })()
                .map(cell)
                .map_err(|error| error.code);
                assert_eq!(actual, expected, "{source}, budget {steps}");
                assert_eq!(
                    actual_budget.steps, expected_budget.steps,
                    "{source}, budget {steps}"
                );
            }
        }
    }

    #[test]
    fn prepared_sort_columns_preserve_pairwise_comparisons() {
        for values in [
            vec![
                EvalValue::Date(0),
                EvalValue::String("1970-01-02".into()),
                EvalValue::String("1970-01-01T01:00:00+02:00".into()),
            ],
            vec![
                EvalValue::Duration(f64::NAN),
                EvalValue::String("2 days".into()),
                EvalValue::Duration(-1.0),
            ],
            vec![
                EvalValue::Number(-10.0),
                EvalValue::Number(1.5),
                EvalValue::Number(f64::NAN),
            ],
            vec![
                EvalValue::String("1970-01-02".into()),
                EvalValue::Empty,
                EvalValue::Date(0),
            ],
            vec![
                EvalValue::String("2 days".into()),
                EvalValue::Null,
                EvalValue::Number(3.0),
            ],
            vec![
                EvalValue::Number(-10.0),
                EvalValue::String("-2".into()),
                EvalValue::Number(-2.0),
            ],
        ] {
            let numeric = values
                .iter()
                .all(|value| matches!(value, EvalValue::Number(_)));
            let column = SortColumn::new(values.iter().cloned().map(SortKey::Owned).collect());
            for (left_index, left) in values.iter().enumerate() {
                for (right_index, right) in values.iter().enumerate() {
                    assert_eq!(
                        column.compare(left_index, right_index),
                        compare_with_numeric_order(left, right, numeric),
                    );
                }
            }
        }
    }

    #[test]
    fn filter_parsing_scales_with_reached_sources_in_each_query() {
        let environment = ExecutionEnvironment::capture();
        for record_count in [1, 100, 1000] {
            let mut index = Index::empty(PathBuf::from("/vault"));
            for ordinal in 0..record_count {
                let path = format!("{ordinal}.md");
                index.insert_test_record(
                    path.clone(),
                    FileRecord {
                        path: path.clone(),
                        name: ordinal.to_string(),
                        basename: path,
                        folder: String::new(),
                        extension: "md".into(),
                        ctime: 0,
                        mtime: 0,
                        properties: BTreeMap::new(),
                        tags: Vec::new(),
                        links: Vec::new(),
                        backlinks: Vec::new(),
                    }
                    .into(),
                );
            }
            // Repeated sources span the base and view filters. The invalid branch
            // is unreachable, and no formulas or columns contribute parse calls.
            let source = "filters: ['true', '1 == 1', 'true']\nviews:\n  - type: table\n    filters:\n      or: ['true', '(']\n    order: []\n";
            for _ in 0..2 {
                let before = crate::expression::PARSE_INVOCATIONS.with(|count| count.get());
                let output = execute(
                    &index,
                    Limits::default(),
                    1,
                    1,
                    &mut false,
                    &environment,
                    QueryInput {
                        text: source.into(),
                        source_id: "test".into(),
                        host_path: "0.md".into(),
                        view_name: None,
                        preview_rows: 0,
                    },
                )
                .unwrap();
                assert_eq!(output.result.matched_count, record_count);
                let parsed =
                    crate::expression::PARSE_INVOCATIONS.with(|count| count.get()) - before;
                assert_eq!(parsed, 2, "record count: {record_count}");
            }
        }
    }

    #[test]
    fn cached_filters_preserve_parse_and_evaluation_limits() {
        let mut index = Index::empty(PathBuf::from("/vault"));
        index.insert_test_record(
            "Host.md".into(),
            FileRecord {
                path: "Host.md".into(),
                name: "Host".into(),
                basename: "Host.md".into(),
                folder: String::new(),
                extension: "md".into(),
                ctime: 0,
                mtime: 0,
                properties: BTreeMap::new(),
                tags: Vec::new(),
                links: Vec::new(),
                backlinks: Vec::new(),
            }
            .into(),
        );
        let formulas = BTreeMap::new();
        let environment = ExecutionEnvironment::capture();
        for (limits, source, code) in [
            (
                Limits {
                    expression_bytes: 3,
                    ..Limits::default()
                },
                "true",
                "expression_too_large",
            ),
            (
                Limits {
                    ast_nodes: 1,
                    ..Limits::default()
                },
                "1 == 1",
                "ast_too_large",
            ),
            (
                Limits {
                    ast_depth: 1,
                    ..Limits::default()
                },
                "!true",
                "ast_too_deep",
            ),
        ] {
            let evaluator = Evaluator {
                index: &index,
                limits,
                formulas: &compile_formulas(&formulas),
                environment: &environment,
            };
            let filter = DataValue::List(vec![
                DataValue::String("0".into()),
                DataValue::String(source.into()),
            ]);
            let mut expressions = HashMap::new();
            let mut context = EvalContext::new(&index.records()["Host.md"], "Host.md");
            let mut budget = Budget::new(limits);
            assert!(
                !evaluator
                    .matches(&filter, &mut expressions, &mut context, &mut budget)
                    .unwrap()
            );
            let DataValue::List(values) = &filter else {
                unreachable!()
            };
            assert_eq!(
                evaluator
                    .matches(&values[1], &mut expressions, &mut context, &mut budget)
                    .unwrap_err()
                    .code,
                code
            );
        }

        let limits = Limits {
            evaluation_steps: 1,
            ..Limits::default()
        };
        let evaluator = Evaluator {
            index: &index,
            limits,
            formulas: &compile_formulas(&formulas),
            environment: &environment,
        };
        let filter = DataValue::String("true".into());
        let mut expressions = HashMap::new();
        let mut context = EvalContext::new(&index.records()["Host.md"], "Host.md");
        let mut budget = Budget::new(limits);
        assert!(
            evaluator
                .matches(&filter, &mut expressions, &mut context, &mut budget)
                .unwrap()
        );
        assert_eq!(
            evaluator
                .matches(&filter, &mut expressions, &mut context, &mut budget)
                .unwrap_err()
                .code,
            "evaluation_limit"
        );
    }

    #[test]
    fn property_members_preserve_object_semantics_and_evaluation_budget() {
        let limits = Limits::default();
        let properties = BTreeMap::from([
            ("rank".into(), DataValue::Number(2.0)),
            ("name".into(), DataValue::String("frontmatter name".into())),
            ("null".into(), DataValue::Null),
            ("isEmpty".into(), DataValue::Bool(true)),
            ("date".into(), DataValue::Date(0)),
            (
                "nested".into(),
                DataValue::Object(BTreeMap::from([("rank".into(), DataValue::Number(3.0))])),
            ),
            (
                "values".into(),
                DataValue::List(vec![DataValue::Number(4.0)]),
            ),
        ]);
        let mut index = Index::empty(PathBuf::from("/vault"));
        index.insert_test_record(
            "Host.md".into(),
            FileRecord {
                path: "Host.md".into(),
                name: "Host".into(),
                basename: "Host.md".into(),
                folder: String::new(),
                extension: "md".into(),
                ctime: 0,
                mtime: 0,
                properties: properties.clone(),
                tags: Vec::new(),
                links: Vec::new(),
                backlinks: Vec::new(),
            }
            .into(),
        );
        let formulas =
            BTreeMap::from([("all".into(), parse_expression("property", limits).unwrap())]);
        let environment =
            ExecutionEnvironment::new(jiff::Timestamp::UNIX_EPOCH, jiff::tz::TimeZone::UTC);
        let evaluator = Evaluator {
            index: &index,
            limits,
            formulas: &compile_formulas(&formulas),
            environment: &environment,
        };
        let mut context = EvalContext::new(&index.records()["Host.md"], "Host.md");
        for (source, kind, text) in [
            ("property.rank", "number", "2"),
            ("(property).rank", "number", "2"),
            ("file.rank", "number", "2"),
            ("rank", "number", "2"),
            ("property.name", "string", "frontmatter name"),
            ("file.name", "string", "Host"),
            ("property.missing", "empty", ""),
            ("property.null", "empty", ""),
            ("property.nested.rank", "number", "3"),
            ("property.values[0]", "number", "4"),
            ("property.values.map(value + 1)[0]", "number", "5"),
            ("property.date.date()", "date", "1970-01-01"),
            // Object methods take precedence even over a frontmatter key of this name.
            ("property.isEmpty()", "boolean", "false"),
            ("property", "object", "[object Object]"),
            ("formula.all.rank", "number", "2"),
            ("formula.all.isEmpty()", "boolean", "false"),
        ] {
            let value = evaluator
                .evaluate(
                    &parse_expression(source, limits).unwrap(),
                    &mut context,
                    &mut Budget::new(limits),
                )
                .unwrap();
            let rendered = cell(value);
            assert_eq!(
                (
                    serde_json::to_value(rendered.kind)
                        .unwrap()
                        .as_str()
                        .unwrap(),
                    rendered.text.as_str()
                ),
                (kind, text),
                "{source}"
            );
        }
        let EvalValue::Object(mut standalone) = evaluator.identifier("property", &context) else {
            panic!("standalone property must remain an owned object");
        };
        assert_eq!(standalone, properties);
        standalone.clear();
        assert_eq!(context.current.properties, properties);

        let expression = parse_expression("property.rank", limits).unwrap();
        for steps in [1, 2] {
            let mut budget = Budget::new(Limits {
                evaluation_steps: steps,
                ..limits
            });
            let result = evaluator.evaluate(&expression, &mut context, &mut budget);
            assert_eq!(budget.steps, 2);
            if steps == 1 {
                assert_eq!(result.unwrap_err().code, "evaluation_limit");
            } else {
                assert!(matches!(result.unwrap(), EvalValue::Number(2.0)));
            }
        }
    }

    #[test]
    fn numeric_comparisons_and_sort_use_numeric_order() {
        let number = EvalValue::Number;
        for (operator, left, right, expected) in [
            ("<", -10.0, -2.0, true),
            (">", -2.0, -10.0, true),
            ("<", 2.0, 10.0, true),
            (">", 1.2, 1.10, true),
            ("==", 1.2, 1.20, true),
            ("<=", -2.5, -2.5, true),
            ("<", -2.0, -10.0, false),
            ("==", 1.2, 1.10, false),
        ] {
            assert!(matches!(
                binary(operator, number(left), number(right)),
                EvalValue::Bool(value) if value == expected
            ));
        }

        let mut values = [-2.0, 1.2, -10.0, 10.0, 1.10, 2.0].map(number).to_vec();
        values.sort_by(compare);
        assert_eq!(
            values.iter().map(EvalValue::number).collect::<Vec<_>>(),
            [-10.0, -2.0, 1.10, 1.2, 2.0, 10.0]
        );
    }

    #[test]
    fn non_numeric_comparisons_keep_their_order() {
        let earlier = parse_date("2026-07-01").unwrap();
        assert_eq!(
            compare(
                &EvalValue::Date(earlier),
                &EvalValue::String("2026-07-02".into())
            ),
            Ordering::Less
        );
        assert_eq!(
            compare(
                &EvalValue::Duration(86_400_000.0),
                &EvalValue::String("2 days".into())
            ),
            Ordering::Less
        );
        assert_eq!(
            compare(
                &EvalValue::String("item2".into()),
                &EvalValue::String("item10".into())
            ),
            Ordering::Less
        );
        assert_eq!(
            compare(&EvalValue::Number(1.2), &EvalValue::String("1.10".into())),
            Ordering::Less
        );
    }

    #[test]
    fn mixed_sort_values_keep_natural_order() {
        let mut values = [
            EvalValue::Number(-10.0),
            EvalValue::Number(-2.0),
            EvalValue::String("-10".into()),
        ];
        let numeric_sort = values
            .iter()
            .all(|value| matches!(value, EvalValue::Number(_)));
        values.sort_by(|left, right| compare_with_numeric_order(left, right, numeric_sort));
        assert!(matches!(values[0], EvalValue::Number(-2.0)));
        assert!(matches!(values[1], EvalValue::Number(-10.0)));
        assert!(matches!(&values[2], EvalValue::String(value) if value == "-10"));
    }

    #[test]
    fn today_and_round_work_through_formula_dependencies() {
        let mut index = Index::empty(PathBuf::from("/vault"));
        index.insert_test_record(
            "Host.md".into(),
            FileRecord {
                path: "Host.md".into(),
                name: "Host".into(),
                basename: "Host.md".into(),
                folder: String::new(),
                extension: "md".into(),
                ctime: parse_date("2026-07-12").unwrap(),
                mtime: parse_date("2026-07-12").unwrap(),
                properties: BTreeMap::from([(
                    "numbers".into(),
                    DataValue::List(vec![DataValue::Number(1.0), DataValue::Number(2.0)]),
                )]),
                tags: Vec::new(),
                links: Vec::new(),
                backlinks: Vec::new(),
            }
            .into(),
        );
        let mut formulas = BTreeMap::new();
        formulas.insert(
            "time_alive".into(),
            parse_expression("today() - file.ctime.date()", Limits::default()).unwrap(),
        );
        formulas.insert(
            "days_alive".into(),
            parse_expression("formula.time_alive.days.round()", Limits::default()).unwrap(),
        );
        formulas.insert(
            "cycle".into(),
            parse_expression("formula.cycle", Limits::default()).unwrap(),
        );
        formulas.insert(
            "first_cycle".into(),
            parse_expression("formula.first_cycle", Limits::default()).unwrap(),
        );
        formulas.insert(
            "second_cycle".into(),
            parse_expression("formula.second_cycle", Limits::default()).unwrap(),
        );
        let today = parse_date("2026-07-24").unwrap();
        let environment = ExecutionEnvironment::new(
            jiff::Timestamp::from_millisecond(today).unwrap(),
            jiff::tz::TimeZone::UTC,
        );
        let evaluator = Evaluator {
            index: &index,
            limits: Limits::default(),
            formulas: &compile_formulas(&formulas),
            environment: &environment,
        };
        let mut context = EvalContext::new(&index.records()["Host.md"], "Host.md");
        let mut budget = Budget::new(Limits::default());

        let current = evaluator
            .evaluate(
                &parse_expression("today()", Limits::default()).unwrap(),
                &mut context,
                &mut budget,
            )
            .unwrap();
        assert!(matches!(current, EvalValue::Date(value) if value == today));

        let rounded = evaluator
            .evaluate(
                &parse_expression("(2.345).round(2)", Limits::default()).unwrap(),
                &mut context,
                &mut budget,
            )
            .unwrap();
        assert!(matches!(rounded, EvalValue::Number(value) if value == 2.35));

        let days_alive = evaluator
            .evaluate(
                &parse_expression("formula.days_alive", Limits::default()).unwrap(),
                &mut context,
                &mut budget,
            )
            .unwrap();
        assert!(matches!(days_alive, EvalValue::Number(value) if value == 12.0));

        for source in ["today(formula.cycle)", "(2.345).round(2, formula.cycle)"] {
            let mut context = EvalContext::new(&index.records()["Host.md"], "Host.md");
            let mut budget = Budget::new(Limits::default());
            let error = evaluator
                .evaluate(
                    &parse_expression(source, Limits::default()).unwrap(),
                    &mut context,
                    &mut budget,
                )
                .unwrap_err();
            assert_eq!(error.code, "formula_cycle");
        }

        let mut context = EvalContext::new(&index.records()["Host.md"], "Host.md");
        let mut budget = Budget::new(Limits::default());
        let error = evaluator
            .evaluate(
                &parse_expression(
                    "today(formula.first_cycle, formula.second_cycle)",
                    Limits::default(),
                )
                .unwrap(),
                &mut context,
                &mut budget,
            )
            .unwrap_err();
        assert_eq!(error.message, "formula cycle at first_cycle");

        let mut context = EvalContext::new(&index.records()["Host.md"], "Host.md");
        let mut budget = Budget::new(Limits::default());
        let mapped = evaluator
            .evaluate(
                &parse_expression("numbers.map(value + 1)", Limits::default()).unwrap(),
                &mut context,
                &mut budget,
            )
            .unwrap();
        assert!(matches!(
            mapped,
            EvalValue::List(values)
                if matches!(values.as_slice(),
                    [EvalValue::Number(first), EvalValue::Number(second)]
                        if *first == 2.0 && *second == 3.0)
        ));

        for source in ["numbers.map()", "numbers.filter(value, value)"] {
            let mut context = EvalContext::new(&index.records()["Host.md"], "Host.md");
            let mut budget = Budget::new(Limits::default());
            let error = evaluator
                .evaluate(
                    &parse_expression(source, Limits::default()).unwrap(),
                    &mut context,
                    &mut budget,
                )
                .unwrap_err();
            assert_eq!(error.code, "invalid_arguments");
        }
    }
}
