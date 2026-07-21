//! Bases table-query parsing, expression evaluation, sorting, and response rendering.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
    time::Instant,
};

use serde::Serialize;

use crate::{
    error::{Result, WorkerError},
    expression::{Expression, Literal, parse_expression, safe_key},
    index::{FileRecord, Index},
    limits::Limits,
    protocol::{Column, QueryResult, View},
    value::{
        DataValue, EvalValue, format_utc_date, natural_cmp, parse_date, parse_duration,
        sanitize_text,
    },
};

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
    pub kind: String,
    /// Sanitized display text for the cell.
    pub text: String,
    /// Destination vault path when this is a link cell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
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
}

pub fn execute(
    index: &Index,
    limits: Limits,
    generation: u64,
    result_sequence: u64,
    result_id_allocated: &mut bool,
    input: QueryInput,
) -> Result<QueryOutput> {
    let base = crate::yaml::parse_mapping(&input.text, limits)?;
    let views = object_list(base.get("views"));
    let view = match input.view_name.as_deref() {
        Some(name) => views
            .iter()
            .find(|view| string(view.get("name")) == Some(name))
            .copied()
            .ok_or_else(|| WorkerError::new("unknown_view", format!("view not found: {name}")))?,
        None => views.first().copied().ok_or_else(|| {
            WorkerError::new("unsupported_view", "only table views are supported")
        })?,
    };
    if string(view.get("type")) != Some("table") {
        return Err(WorkerError::new(
            "unsupported_view",
            "only table views are supported",
        ));
    }
    if ["groupBy", "summaries", "layout"]
        .iter()
        .any(|key| view.contains_key(*key))
    {
        return Err(WorkerError::new(
            "unsupported_view",
            "grouping, summaries, and non-table layouts are not supported",
        ));
    }
    if !index.records.contains_key(&input.host_path) {
        return Err(WorkerError::new("missing_host", "host note is not indexed"));
    }
    let formulas = parse_formulas(base.get("formulas"), limits)?;
    let mut budget = Budget::new(limits);
    let mut contexts: HashMap<String, EvalContext> = HashMap::new();
    let predicates = [base.get("filters"), view.get("filters")]
        .into_iter()
        .flatten()
        .filter(|value| data_truthy(value))
        .collect::<Vec<_>>();
    let evaluator = Evaluator {
        index,
        limits,
        formulas: &formulas,
    };
    let mut matched = Vec::new();
    for record in index.records.values() {
        let context = contexts
            .entry(record.path.clone())
            .or_insert_with(|| EvalContext::new(&record.path, &input.host_path));
        let mut include = true;
        for predicate in &predicates {
            if !evaluator.matches(predicate, context, &mut budget)? {
                include = false;
                break;
            }
        }
        if include {
            matched.push(record.path.clone());
        }
    }
    let sort_rules = parse_sort(view.get("sort"), limits)?;
    if sort_rules.is_empty() {
        // Natural ordering is observable even without explicit sort rules.
        matched.sort_by(|left, right| natural_cmp(left, right));
    } else {
        let mut sort_keys = HashMap::new();
        for path in &matched {
            let context = contexts
                .entry(path.clone())
                .or_insert_with(|| EvalContext::new(path, &input.host_path));
            let keys = sort_rules
                .iter()
                .map(|rule| evaluator.evaluate(&rule.expression, context, &mut budget))
                .collect::<Result<Vec<_>>>()?;
            sort_keys.insert(path.clone(), keys);
        }
        matched.sort_by(|left, right| {
            let left_keys = &sort_keys[left];
            let right_keys = &sort_keys[right];
            for (index, rule) in sort_rules.iter().enumerate() {
                let order = compare(&left_keys[index], &right_keys[index]);
                let order = if rule.descending {
                    order.reverse()
                } else {
                    order
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
            natural_cmp(left, right)
        });
    }
    let columns = parse_columns(view.get("order"), base.get("properties"), limits)?;
    let view_limit = parse_view_limit(view.get("limit"), limits.result_rows)?;
    let visible = matched.iter().take(view_limit).cloned().collect::<Vec<_>>();
    let preview_count = input.preview_rows.min(visible.len());
    // Result IDs are generation-scoped so a reindex invalidates every cached row set at once.
    let result_id = format!("r{generation}-{result_sequence}");
    *result_id_allocated = true;
    let mut rendered_rows = Vec::with_capacity(visible.len());
    for path in &visible {
        let record = &index.records[path];
        let context = contexts
            .entry(path.clone())
            .or_insert_with(|| EvalContext::new(path, &input.host_path));
        let cells = columns
            .iter()
            .map(|column| {
                evaluator
                    .evaluate(&column.expression, context, &mut budget)
                    .map(cell)
            })
            .collect::<Result<Vec<_>>>()?;
        rendered_rows.push(ResultRow {
            path: path.clone(),
            display_name: record.name.clone(),
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
            name: string(view.get("name")).unwrap_or("table").to_owned(),
            kind: "table",
        },
        columns: public_columns,
        preview_rows: rendered_rows[..preview_count].to_vec(),
        matched_count: matched.len(),
        view_count: visible.len(),
        preview_count,
        truncated: matched.len() > visible.len(),
        warnings: Vec::new(),
        timings: BTreeMap::new(),
        index_generation: generation,
    };
    assert_size(&result, limits)?;
    assert_size(
        &RowsForLimit {
            result_id: &result_id,
            rows: &rendered_rows,
        },
        limits,
    )?;
    Ok(QueryOutput {
        result,
        rows: rendered_rows,
    })
}

#[derive(Serialize)]
struct RowsForLimit<'a> {
    result_id: &'a str,
    rows: &'a [ResultRow],
}

fn assert_size(value: &impl Serialize, limits: Limits) -> Result<()> {
    if serde_json::to_vec(value)?.len() > limits.result_bytes {
        Err(WorkerError::new(
            "result_too_large",
            "query result exceeds the configured size limit",
        ))
    } else {
        Ok(())
    }
}

/// A compiled sort expression and its requested direction.
struct SortRule {
    /// Expression evaluated for each matched record.
    expression: Expression,
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
                expression: parse_expression(source, limits)?,
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
    expression: Expression,
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
                expression: parse_expression(&key, limits)?,
                key,
                label,
            })
        })
        .collect()
}

fn parse_formulas(
    value: Option<&DataValue>,
    limits: Limits,
) -> Result<BTreeMap<String, Expression>> {
    let Some(DataValue::Object(values)) = value else {
        return Ok(BTreeMap::new());
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
            Ok((key.clone(), parse_expression(source, limits)?))
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
}

impl Budget {
    fn new(limits: Limits) -> Self {
        Self {
            started: Instant::now(),
            steps: 0,
            limits,
        }
    }

    fn step(&mut self) -> Result<()> {
        self.steps += 1;
        if self.steps > self.limits.evaluation_steps
            || self.started.elapsed().as_millis() > u128::from(self.limits.evaluation_ms)
        {
            Err(WorkerError::new(
                "evaluation_limit",
                "expression evaluation exceeded its limit",
            ))
        } else {
            Ok(())
        }
    }
}

/// Per-record state used to cache formulas and expose dynamic expression values.
struct EvalContext {
    /// Indexed path currently being evaluated.
    current: String,
    /// Indexed path hosting the Base, exposed as `this`.
    host: String,
    /// Cached formula values for the current record.
    formula_values: HashMap<String, EvalValue>,
    /// Formula names currently being evaluated for cycle detection.
    formula_stack: Vec<String>,
    /// Dynamic `value` bound while evaluating list callbacks.
    value: Option<EvalValue>,
}

impl EvalContext {
    fn new(current: &str, host: &str) -> Self {
        Self {
            current: current.into(),
            host: host.into(),
            formula_values: HashMap::new(),
            formula_stack: Vec::new(),
            value: None,
        }
    }
}

/// Evaluates expressions against an index under the configured resource limits.
struct Evaluator<'a> {
    /// Immutable index that supplies records and link targets.
    index: &'a Index,
    /// Limits used when parsing nested expressions in filters.
    limits: Limits,
    /// Compiled formulas referenced through the `formula` namespace.
    formulas: &'a BTreeMap<String, Expression>,
}

impl Evaluator<'_> {
    fn matches(
        &self,
        filter: &DataValue,
        context: &mut EvalContext,
        budget: &mut Budget,
    ) -> Result<bool> {
        match filter {
            DataValue::String(source) => Ok(self
                .evaluate(&parse_expression(source, self.limits)?, context, budget)?
                .truthy()),
            DataValue::List(values) => {
                for value in values {
                    if !self.matches(value, context, budget)? {
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
                                if self.matches(value, context, budget)? {
                                    return Ok(false);
                                }
                            }
                            Ok(true)
                        }
                        value => Ok(!self.matches(value, context, budget)?),
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
                        if !self.matches(value, context, budget)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                } else {
                    for value in values {
                        if self.matches(value, context, budget)? {
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
            Expression::Literal(value) => Ok(match value {
                Literal::String(value) => EvalValue::String(value.clone()),
                Literal::Number(value) => EvalValue::Number(*value),
                Literal::Boolean(value) => EvalValue::Bool(*value),
                Literal::Null => EvalValue::Null,
            }),
            Expression::Identifier(name) => Ok(self.identifier(name, context)),
            Expression::Member(object, property) => {
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
                Ok(if operator == "-" {
                    EvalValue::Number(-value.number())
                } else {
                    EvalValue::Bool(!value.truthy())
                })
            }
            Expression::Binary(operator, left, right) => {
                let left = self.evaluate(left, context, budget)?;
                let right = self.evaluate(right, context, budget)?;
                Ok(binary(operator, left, right))
            }
            Expression::Call(callee, args) => {
                let target = self.evaluate(callee, context, budget)?;
                self.call(target, args, context, budget)
            }
        }
    }

    fn identifier(&self, name: &str, context: &EvalContext) -> EvalValue {
        if name == "file" || name == "note" {
            return EvalValue::File(context.current.clone());
        }
        if name == "this" {
            return EvalValue::File(context.host.clone());
        }
        if name == "formula" {
            return EvalValue::Formula;
        }
        if name == "property" {
            return EvalValue::Object(self.current(context).properties.clone());
        }
        if name == "value" {
            return context.value.clone().unwrap_or(EvalValue::Empty);
        }
        if name == "date" {
            return EvalValue::Function(name.into());
        }
        self.current(context)
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
            EvalValue::File(path) => {
                let record = &self.index.records[&path];
                Ok(match property {
                    "file" => EvalValue::File(path),
                    "name" => EvalValue::String(record.name.clone()),
                    "path" => EvalValue::String(record.path.clone()),
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
                    "ctime" => EvalValue::Date(record.ctime),
                    "mtime" => EvalValue::Date(record.mtime),
                    "hasTag" | "inFolder" | "asLink" => EvalValue::Method {
                        target: Box::new(EvalValue::File(path)),
                        property: property.into(),
                    },
                    _ => record
                        .properties
                        .get(property)
                        .map(EvalValue::from)
                        .unwrap_or(EvalValue::Empty),
                })
            }
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

    fn call(
        &self,
        target: EvalValue,
        args: &[Expression],
        context: &mut EvalContext,
        budget: &mut Budget,
    ) -> Result<EvalValue> {
        if let EvalValue::Function(name) = &target {
            if name == "date" {
                let value = args
                    .first()
                    .map(|arg| self.evaluate(arg, context, budget))
                    .transpose()?
                    .unwrap_or(EvalValue::Empty);
                return Ok(to_date(&value).map_or(EvalValue::Empty, EvalValue::Date));
            }
        }
        let EvalValue::Method { target, property } = target else {
            return Ok(EvalValue::Empty);
        };
        match property.as_str() {
            "date" => Ok(*target),
            "isEmpty" => Ok(EvalValue::Bool(
                matches!(*target, EvalValue::Empty | EvalValue::Null)
                    || matches!(&*target, EvalValue::String(value) if value.is_empty())
                    || matches!(&*target, EvalValue::List(value) if value.is_empty()),
            )),
            "hasTag" => {
                let wanted = self.argument(args, 0, context, budget)?.string();
                let EvalValue::File(path) = *target else {
                    return Ok(EvalValue::Empty);
                };
                Ok(EvalValue::Bool(self.index.records[&path].tags.iter().any(
                    |tag| {
                        tag == &wanted
                            || tag
                                .strip_prefix(&wanted)
                                .is_some_and(|rest| rest.starts_with('/'))
                    },
                )))
            }
            "inFolder" => {
                let wanted = self.argument(args, 0, context, budget)?.string();
                let EvalValue::File(path) = *target else {
                    return Ok(EvalValue::Empty);
                };
                let folder = &self.index.records[&path].folder;
                Ok(EvalValue::Bool(
                    folder == &wanted || folder.starts_with(&format!("{wanted}/")),
                ))
            }
            "asLink" => {
                let EvalValue::File(path) = *target else {
                    return Ok(EvalValue::Empty);
                };
                let display = if args.is_empty() {
                    self.index.records[&path].name.clone()
                } else {
                    self.argument(args, 0, context, budget)?.string()
                };
                Ok(EvalValue::Link {
                    path,
                    display: Some(display),
                })
            }
            "asFile" => {
                let EvalValue::Link { path, .. } = *target else {
                    return Ok(EvalValue::Empty);
                };
                Ok(if self.index.records.contains_key(&path) {
                    EvalValue::File(path)
                } else {
                    EvalValue::Empty
                })
            }
            "sort" => {
                let EvalValue::List(mut values) = *target else {
                    return Ok(EvalValue::Empty);
                };
                values.sort_by(compare);
                Ok(EvalValue::List(values))
            }
            "slice" => {
                let EvalValue::List(values) = *target else {
                    return Ok(EvalValue::Empty);
                };
                let start = args
                    .first()
                    .map(|arg| {
                        self.evaluate(arg, context, budget)
                            .map(|value| value.number())
                    })
                    .transpose()?
                    .unwrap_or(0.0);
                let end = args
                    .get(1)
                    .map(|arg| {
                        self.evaluate(arg, context, budget)
                            .map(|value| value.number())
                    })
                    .transpose()?;
                let start = slice_index(start, values.len());
                let end = end.map_or(values.len(), |end| slice_index(end, values.len()));
                Ok(EvalValue::List(if start <= end {
                    values[start..end].to_vec()
                } else {
                    Vec::new()
                }))
            }
            "filter" | "map" => {
                let EvalValue::List(values) = *target else {
                    return Ok(EvalValue::Empty);
                };
                let Some(expression) = args.first() else {
                    return Ok(EvalValue::List(Vec::new()));
                };
                // `value` is dynamically scoped within a list callback and must be restored for
                // the surrounding expression.
                let previous = context.value.clone();
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
                context.value = previous;
                Ok(EvalValue::List(result))
            }
            _ => Ok(EvalValue::Empty),
        }
    }

    fn argument(
        &self,
        args: &[Expression],
        index: usize,
        context: &mut EvalContext,
        budget: &mut Budget,
    ) -> Result<EvalValue> {
        args.get(index)
            .map(|arg| self.evaluate(arg, context, budget))
            .unwrap_or(Ok(EvalValue::Empty))
    }

    fn formula(
        &self,
        name: &str,
        context: &mut EvalContext,
        budget: &mut Budget,
    ) -> Result<EvalValue> {
        if let Some(value) = context.formula_values.get(name) {
            return Ok(value.clone());
        }
        let Some(expression) = self.formulas.get(name) else {
            return Ok(EvalValue::Empty);
        };
        if context.formula_stack.iter().any(|current| current == name) {
            return Err(WorkerError::new(
                "formula_cycle",
                format!("formula cycle at {name}"),
            ));
        }
        if context.formula_stack.len() >= self.limits.formula_depth {
            return Err(WorkerError::new(
                "formula_limit",
                "formula dependency depth exceeds its limit",
            ));
        }
        context.formula_stack.push(name.into());
        let value = self.evaluate(expression, context, budget)?;
        context.formula_stack.pop();
        context.formula_values.insert(name.into(), value.clone());
        Ok(value)
    }

    fn current<'a>(&'a self, context: &EvalContext) -> &'a FileRecord {
        &self.index.records[&context.current]
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
    if let (Some(left), Some(right)) = (left.path(), right.path()) {
        return natural_cmp(left, right);
    }
    if let (Some(left), Some(right)) = (to_duration(left), to_duration(right)) {
        return left.partial_cmp(&right).unwrap_or(Ordering::Equal);
    }
    if let (Some(left), Some(right)) = (to_date(left), to_date(right)) {
        return left.cmp(&right);
    }
    natural_cmp(&left.string(), &right.string())
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
            let fallback = path
                .rsplit('/')
                .next()
                .unwrap_or(&path)
                .rsplit_once('.')
                .map_or_else(|| path.clone(), |(name, _)| name.to_owned());
            Cell {
                kind: "link".into(),
                text: sanitize_text(display.as_deref().unwrap_or(&fallback)),
                target: Some(path),
            }
        }
        EvalValue::Date(value) => Cell {
            kind: "date".into(),
            text: format_utc_date(value),
            target: None,
        },
        EvalValue::Duration(value) => Cell {
            kind: "duration".into(),
            text: format!("{} days", (value / 86_400_000.0).round()),
            target: None,
        },
        EvalValue::Empty | EvalValue::Null => Cell {
            kind: "empty".into(),
            text: String::new(),
            target: None,
        },
        EvalValue::Bool(value) => Cell {
            kind: "boolean".into(),
            text: value.to_string(),
            target: None,
        },
        EvalValue::Number(value) => Cell {
            kind: "number".into(),
            text: sanitize_text(&EvalValue::Number(value).string()),
            target: None,
        },
        EvalValue::String(value) => Cell {
            kind: "string".into(),
            text: sanitize_text(&value),
            target: None,
        },
        value => Cell {
            kind: "object".into(),
            text: sanitize_text(&value.string()),
            target: None,
        },
    }
}
