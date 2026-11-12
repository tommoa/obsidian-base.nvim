//! Bound field projections and borrowed comparison operands. Complex
//! methods and callbacks remain in the general evaluator.

use std::{borrow::Cow, cmp::Ordering, collections::BTreeMap};

use super::{
    Cell, CellKind, DataValue, EvalContext, EvalValue, Evaluator, Expression, FileNamespace,
    FileRecord, Literal, cell, file_namespace, natural_cmp, parse_date, parse_duration,
    sanitize_owned_text, to_date, to_duration,
};

#[derive(Clone, Copy)]
pub(super) enum ValueRef<'a> {
    Empty,
    Null,
    Bool(bool),
    Number(f64),
    Date(i64),
    Text(&'a str),
    List(&'a [DataValue]),
    File(&'a str),
    Properties(&'a BTreeMap<String, DataValue>),
    Eval(&'a EvalValue),
}

impl<'a> ValueRef<'a> {
    /// Copy cheap scalars now so dense sort columns need not chase property-map
    /// pointers again. Strings and collections still borrow their backing data.
    fn data(value: &'a DataValue) -> Self {
        match value {
            DataValue::Null => Self::Null,
            DataValue::Bool(value) => Self::Bool(*value),
            DataValue::Number(value) => Self::Number(*value),
            DataValue::Date(value) => Self::Date(*value),
            DataValue::String(value) => Self::Text(value),
            DataValue::List(values) => Self::List(values),
            DataValue::Object(values) => Self::Properties(values),
        }
    }

    pub(super) fn truthy(&self) -> bool {
        match self {
            Self::Empty | Self::Null => false,
            Self::Bool(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::Text(value) => !value.is_empty(),
            Self::Eval(value) => value.truthy(),
            _ => true,
        }
    }

    pub(super) fn numeric(&self) -> Option<f64> {
        match self {
            Self::Number(value) | Self::Eval(EvalValue::Number(value)) => Some(*value),
            _ => None,
        }
    }

    fn text(&self) -> Cow<'_, str> {
        match self {
            Self::Text(value) => Cow::Borrowed(value),
            Self::Eval(EvalValue::String(value)) => Cow::Borrowed(value),
            Self::Empty | Self::Null => Cow::Borrowed(""),
            Self::File(_) | Self::Properties(_) => Cow::Borrowed("[object Object]"),
            Self::List(values) => Cow::Owned(list_text(values)),
            Self::Eval(value) => Cow::Owned(value.string()),
            Self::Bool(value) => Cow::Owned(value.to_string()),
            Self::Number(value) => Cow::Owned(EvalValue::Number(*value).string()),
            Self::Date(value) => Cow::Owned(EvalValue::Date(*value).string()),
        }
    }

    pub(super) fn date(&self) -> Option<i64> {
        match self {
            Self::Date(value) => Some(*value),
            Self::Text(value) => parse_date(value),
            Self::Eval(value) => to_date(value),
            _ => None,
        }
    }

    pub(super) fn duration(&self) -> Option<f64> {
        match self {
            Self::Text(value) => parse_duration(value),
            Self::Eval(value) => to_duration(value),
            _ => None,
        }
    }

    fn path(&self) -> Option<&str> {
        match self {
            Self::File(path) => Some(path),
            Self::Eval(value) => value.path(),
            _ => None,
        }
    }

    pub(super) fn compare(&self, right: &Self, numeric_order: bool) -> Ordering {
        if let (Some(left), Some(right)) = (self.path(), right.path()) {
            return natural_cmp(left, right);
        }
        if let (Some(left), Some(right)) = (self.duration(), right.duration()) {
            return left.partial_cmp(&right).unwrap_or(Ordering::Equal);
        }
        if let (Some(left), Some(right)) = (self.date(), right.date()) {
            return left.cmp(&right);
        }
        if let (true, Some(left), Some(right)) = (numeric_order, self.numeric(), right.numeric()) {
            return left
                .partial_cmp(&right)
                .unwrap_or_else(|| natural_cmp(&left.to_string(), &right.to_string()));
        }
        natural_cmp(&self.text(), &right.text())
    }

    pub(super) fn into_owned(self) -> EvalValue {
        match self {
            Self::Empty => EvalValue::Empty,
            Self::Null => EvalValue::Null,
            Self::Bool(value) => EvalValue::Bool(value),
            Self::Number(value) => EvalValue::Number(value),
            Self::Date(value) => EvalValue::Date(value),
            Self::Text(value) => EvalValue::String(value.to_owned()),
            Self::List(values) => EvalValue::List(values.iter().map(EvalValue::from).collect()),
            Self::File(path) => EvalValue::File(path.to_owned()),
            Self::Properties(values) => EvalValue::Object(values.clone()),
            Self::Eval(value) => value.clone(),
        }
    }

    pub(super) fn into_cell(self) -> Cell {
        match self {
            Self::Properties(_) => Cell {
                kind: CellKind::Object,
                text: "[object Object]".into(),
                target: None,
            },
            Self::List(values) => Cell {
                kind: CellKind::Object,
                text: sanitize_owned_text(list_text(values)),
                target: None,
            },
            Self::Text(value) => Cell {
                kind: CellKind::String,
                text: sanitize_owned_text(value.to_owned()),
                target: None,
            },
            other => cell(other.into_owned()),
        }
    }
}

/// Stringify borrowed YAML collections without first cloning them into runtime
/// lists. Formatting primitives still uses the ordinary value implementation.
fn data_text(value: &DataValue) -> Cow<'_, str> {
    match value {
        DataValue::Null => Cow::Borrowed(""),
        DataValue::String(value) => Cow::Borrowed(value),
        DataValue::Object(_) => Cow::Borrowed("[object Object]"),
        DataValue::List(values) => Cow::Owned(list_text(values)),
        value => Cow::Owned(EvalValue::from(value).string()),
    }
}

fn list_text(values: &[DataValue]) -> String {
    values.iter().map(data_text).collect::<Vec<_>>().join(",")
}

/// Shared file-member classification for prepared and dynamic access.
#[derive(Clone, Copy)]
pub(super) enum FileField {
    File,
    Name,
    Path,
    Created,
    Modified,
    Property,
}

impl FileField {
    pub(super) fn named(name: &str) -> Option<Self> {
        Some(match name {
            "file" => Self::File,
            "name" => Self::Name,
            "path" => Self::Path,
            "ctime" => Self::Created,
            "mtime" => Self::Modified,
            "backlinks" | "hasTag" | "inFolder" | "asLink" => return None,
            _ => Self::Property,
        })
    }

    pub(super) fn read<'a>(
        self,
        record: &'a FileRecord,
        path: &'a str,
        property: &str,
    ) -> ValueRef<'a> {
        match self {
            Self::File => ValueRef::File(path),
            Self::Name => ValueRef::Text(&record.name),
            Self::Path => ValueRef::Text(&record.path),
            Self::Created => ValueRef::Date(record.ctime),
            Self::Modified => ValueRef::Date(record.mtime),
            Self::Property => record
                .properties
                .get(property)
                .map_or(ValueRef::Empty, ValueRef::data),
        }
    }
}

pub(super) enum Projection {
    Literal(Literal),
    Property(String),
    Properties,
    File {
        namespace: FileNamespace,
        field: FileField,
        property: String,
    },
}

impl Projection {
    /// Bind a direct read and its entry charge together. Operators and calls
    /// cannot enter this path; their data-dependent work keeps node checks.
    pub(super) fn compile(expression: &Expression) -> Option<(Self, u64)> {
        let field = |namespace, name: &str| {
            let field = FileField::named(name)?;
            let property = if matches!(field, FileField::Property) {
                name.into()
            } else {
                String::new()
            };
            Some(Self::File {
                namespace,
                field,
                property,
            })
        };
        let (projection, steps) = match expression {
            Expression::Literal(value) => (Self::Literal(value.clone()), 1),
            Expression::Identifier(name) => (
                match name.as_str() {
                    "file" | "note" => field(FileNamespace::Current, "file")?,
                    "this" => field(FileNamespace::Host, "file")?,
                    "property" => Self::Properties,
                    "formula" | "date" | "today" | "value" => return None,
                    _ => Self::Property(name.clone()),
                },
                1,
            ),
            Expression::Member(object, name) => {
                if matches!(&**object, Expression::Identifier(namespace) if namespace == "property")
                    && name != "isEmpty"
                {
                    (Self::Property(name.clone()), 2)
                } else {
                    let (namespace, steps) = file_namespace(object)?;
                    (field(namespace, name)?, steps as u64 + 1)
                }
            }
            _ => return None,
        };
        Some((projection, steps))
    }

    pub(super) fn evaluate<'a, 'record: 'a>(
        &'a self,
        evaluator: &'a Evaluator,
        context: &EvalContext<'record>,
    ) -> ValueRef<'a> {
        match self {
            Self::Literal(value) => match value {
                Literal::String(value) => ValueRef::Text(value),
                Literal::Number(value) => ValueRef::Number(*value),
                Literal::Boolean(value) => ValueRef::Bool(*value),
                Literal::Null => ValueRef::Null,
            },
            Self::Property(name) => context
                .current
                .properties
                .get(name)
                .map_or(ValueRef::Empty, ValueRef::data),
            Self::Properties => ValueRef::Properties(&context.current.properties),
            Self::File {
                namespace,
                field,
                property,
            } => {
                let path = namespace.path(context);
                let record = evaluator.file(path, context);
                field.read(record, path, property)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reference_compare(left: &EvalValue, right: &EvalValue, numeric_order: bool) -> Ordering {
        if let (Some(left), Some(right)) = (left.path(), right.path()) {
            return natural_cmp(left, right);
        }
        if let (Some(left), Some(right)) = (to_duration(left), to_duration(right)) {
            return left.partial_cmp(&right).unwrap_or(Ordering::Equal);
        }
        if let (Some(left), Some(right)) = (to_date(left), to_date(right)) {
            return left.cmp(&right);
        }
        if let (true, EvalValue::Number(left), EvalValue::Number(right)) =
            (numeric_order, left, right)
        {
            // Keep the existing string ordering for NaN, which has no numeric order.
            return left
                .partial_cmp(right)
                .unwrap_or_else(|| natural_cmp(&left.to_string(), &right.to_string()));
        }
        natural_cmp(&left.string(), &right.string())
    }

    #[test]
    fn borrowed_values_match_owned_truthiness_comparisons_and_cells() {
        let values = vec![
            DataValue::Null,
            DataValue::Bool(false),
            DataValue::Bool(true),
            DataValue::Number(-0.0),
            DataValue::Number(1.5),
            DataValue::Number(f64::NAN),
            DataValue::Number(f64::INFINITY),
            DataValue::Date(0),
            DataValue::String(String::new()),
            DataValue::String(" ".into()),
            DataValue::String("2 days".into()),
            DataValue::String("1970-01-02".into()),
            DataValue::String("é😀".into()),
            DataValue::Object(BTreeMap::new()),
            DataValue::List(vec![
                DataValue::Number(1.0),
                DataValue::String("two".into()),
                DataValue::List(vec![DataValue::Date(0), DataValue::Null]),
            ]),
        ];
        for left in &values {
            let owned = EvalValue::from(left);
            assert_eq!(
                format!("{:?}", ValueRef::data(left).into_owned()),
                format!("{owned:?}")
            );
            assert_eq!(ValueRef::data(left).into_cell(), cell(owned.clone()));
            assert_eq!(ValueRef::data(left).truthy(), owned.truthy());
            for right in &values {
                for numeric in [true, false] {
                    assert_eq!(
                        ValueRef::data(left).compare(&ValueRef::data(right), numeric),
                        reference_compare(&owned, &EvalValue::from(right), numeric)
                    );
                }
            }
        }
    }
}
