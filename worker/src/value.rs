//! Shared parsed-data and runtime-value representations plus comparison helpers.

use std::{cmp::Ordering, collections::BTreeMap};

use jiff::Timestamp;

#[derive(Clone, Debug, PartialEq)]
/// Value produced from YAML frontmatter or a parsed Base document.
pub enum DataValue {
    /// YAML null value.
    Null,
    /// YAML boolean value.
    Bool(bool),
    /// Finite YAML numeric value.
    Number(f64),
    /// YAML string value.
    String(String),
    /// Ordered YAML sequence.
    List(Vec<DataValue>),
    /// String-keyed YAML mapping.
    Object(BTreeMap<String, DataValue>),
    /// Milliseconds since the Unix epoch for a typed date property.
    Date(i64),
}

impl DataValue {
    pub fn as_object(&self) -> Option<&BTreeMap<String, DataValue>> {
        match self {
            Self::Object(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[DataValue]> {
        match self {
            Self::List(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
/// Runtime value used while evaluating a Bases expression.
pub enum EvalValue {
    /// Absent property or unsupported operation result.
    Empty,
    /// Explicit null value.
    Null,
    /// Boolean runtime value.
    Bool(bool),
    /// Numeric runtime value, including NaN when coercion fails.
    Number(f64),
    /// String runtime value.
    String(String),
    /// Milliseconds since the Unix epoch.
    Date(i64),
    /// Duration measured in milliseconds.
    Duration(f64),
    /// Link target and optional display text.
    Link {
        /// Vault-relative destination path.
        path: String,
        /// Explicit label supplied by the expression, if any.
        display: Option<String>,
    },
    /// Reference to an indexed file record by path.
    File(String),
    /// Ordered collection of runtime values.
    List(Vec<EvalValue>),
    /// Mapping retained from parsed YAML data.
    Object(BTreeMap<String, DataValue>),
    /// Namespace used to resolve named Base formulas.
    Formula,
    /// Callable builtin identified by its name.
    Function(String),
    /// Deferred method invocation bound to an evaluated receiver.
    Method {
        /// Receiver preserved until the method call is evaluated.
        target: Box<EvalValue>,
        /// Supported method name selected through member access.
        property: String,
    },
}

impl From<&DataValue> for EvalValue {
    fn from(value: &DataValue) -> Self {
        match value {
            DataValue::Null => Self::Null,
            DataValue::Bool(value) => Self::Bool(*value),
            DataValue::Number(value) => Self::Number(*value),
            DataValue::String(value) => Self::String(value.clone()),
            DataValue::List(value) => Self::List(value.iter().map(Self::from).collect()),
            DataValue::Object(value) => Self::Object(value.clone()),
            DataValue::Date(value) => Self::Date(*value),
        }
    }
}

impl EvalValue {
    pub fn truthy(&self) -> bool {
        match self {
            Self::Empty | Self::Null => false,
            Self::Bool(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::String(value) => !value.is_empty(),
            _ => true,
        }
    }

    pub fn number(&self) -> f64 {
        match self {
            Self::Null => 0.0,
            Self::Bool(value) => u8::from(*value).into(),
            Self::Number(value) | Self::Duration(value) => *value,
            Self::String(value) if value.trim().is_empty() => 0.0,
            Self::String(value) => value.parse().unwrap_or(f64::NAN),
            _ => f64::NAN,
        }
    }

    pub fn string(&self) -> String {
        match self {
            Self::Empty | Self::Null => String::new(),
            Self::Bool(value) => value.to_string(),
            Self::Number(value) => number_string(*value),
            Self::String(value) => value.clone(),
            Self::Date(value) => format_utc_date(*value),
            Self::Duration(value) => number_string(*value),
            Self::Link { .. } | Self::File(_) => "[object Object]".into(),
            Self::List(values) => values
                .iter()
                .map(Self::string)
                .collect::<Vec<_>>()
                .join(","),
            Self::Object(_) | Self::Formula | Self::Function(_) | Self::Method { .. } => {
                "[object Object]".into()
            }
        }
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Link { path, .. } | Self::File(path) => Some(path),
            _ => None,
        }
    }
}

pub fn parse_date(value: &str) -> Option<i64> {
    let source = if is_date_only(value) {
        format!("{value}T00:00:00Z")
    } else {
        if !has_explicit_offset(value) {
            return None;
        }
        value.to_owned()
    };
    source
        .parse::<Timestamp>()
        .ok()
        .map(Timestamp::as_millisecond)
}

fn is_date_only(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}

fn has_explicit_offset(value: &str) -> bool {
    if !value.contains('T') && !value.contains('t') {
        return false;
    }
    value.ends_with('Z')
        || value.ends_with('z')
        || value
            .rfind(['+', '-'])
            .is_some_and(|index| index > value.find(['T', 't']).unwrap_or(value.len()))
}

pub fn format_utc_date(milliseconds: i64) -> String {
    Timestamp::from_millisecond(milliseconds)
        .map(|timestamp| timestamp.strftime("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

pub fn parse_duration(value: &str) -> Option<f64> {
    let value = value.trim();
    let split = value.find(char::is_whitespace)?;
    let amount: f64 = value[..split].parse().ok()?;
    let unit = value[split..].trim().to_ascii_lowercase();
    let unit = unit.strip_suffix('s').unwrap_or(&unit);
    let scale = match unit {
        "day" => 86_400_000.0,
        "week" => 604_800_000.0,
        "month" => 2_629_800_000.0,
        "year" => 31_557_600_000.0,
        _ => return None,
    };
    Some(amount * scale)
}

pub fn natural_cmp(left: &str, right: &str) -> Ordering {
    let left_bytes = left.as_bytes();
    let right_bytes = right.as_bytes();
    let (mut a, mut b) = (0, 0);
    while a < left_bytes.len() && b < right_bytes.len() {
        if left_bytes[a].is_ascii_digit() && right_bytes[b].is_ascii_digit() {
            let a_end = digit_end(left_bytes, a);
            let b_end = digit_end(right_bytes, b);
            let a_zero = leading_zeros(&left_bytes[a..a_end]);
            let b_zero = leading_zeros(&right_bytes[b..b_end]);
            let a_significant = &left_bytes[(a + a_zero).min(a_end.saturating_sub(1))..a_end];
            let b_significant = &right_bytes[(b + b_zero).min(b_end.saturating_sub(1))..b_end];
            let order = a_significant
                .len()
                .cmp(&b_significant.len())
                .then_with(|| a_significant.cmp(b_significant))
                .then_with(|| a_zero.cmp(&b_zero))
                .then_with(|| left_bytes[a..a_end].cmp(&right_bytes[b..b_end]));
            if order != Ordering::Equal {
                return order;
            }
            a = a_end;
            b = b_end;
            continue;
        }
        let a_char = left[a..].chars().next().expect("valid character boundary");
        let b_char = right[b..].chars().next().expect("valid character boundary");
        let order = a_char.cmp(&b_char);
        if order != Ordering::Equal {
            return order;
        }
        a += a_char.len_utf8();
        b += b_char.len_utf8();
    }
    left_bytes
        .len()
        .cmp(&right_bytes.len())
        .then_with(|| left_bytes.cmp(right_bytes))
}

fn digit_end(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && bytes[at].is_ascii_digit() {
        at += 1;
    }
    at
}

fn leading_zeros(bytes: &[u8]) -> usize {
    bytes.iter().take_while(|byte| **byte == b'0').count()
}

fn number_string(value: f64) -> String {
    if value.is_nan() {
        "NaN".into()
    } else if value.is_infinite() {
        if value.is_sign_positive() {
            "Infinity"
        } else {
            "-Infinity"
        }
        .into()
    } else if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

pub fn sanitize_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| {
            let code = *character as u32;
            !((code <= 0x08)
                || matches!(code, 0x0B | 0x0C)
                || (0x0E..=0x1F).contains(&code)
                || (0x7F..=0x9F).contains(&code))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_strict_and_render_in_utc() {
        assert_eq!(
            format_utc_date(parse_date("2026-07-15").unwrap()),
            "2026-07-15"
        );
        assert_eq!(
            format_utc_date(parse_date("2026-07-15T00:30:00+02:00").unwrap()),
            "2026-07-14"
        );
        assert!(parse_date("July 15 2026").is_none());
        assert!(parse_date("2026-07-15T00:00:00").is_none());
    }

    #[test]
    fn natural_order_is_overflow_free_and_deterministic() {
        let mut values = vec!["a10", "a02", "a2", "a0002", "A2", "ä2"];
        values.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(values, ["A2", "a2", "a02", "a0002", "a10", "ä2"]);
        assert_eq!(
            natural_cmp("n999999999999999999999", "n1000000000000000000000"),
            Ordering::Less
        );
    }

    #[test]
    fn strips_c0_and_c1_controls() {
        assert_eq!(sanitize_text("safe\u{1b}[2Jtext\u{85}"), "safe[2Jtext");
    }
}
