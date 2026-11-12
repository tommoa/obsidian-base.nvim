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

/// Round a number to decimal precision using JavaScript `Math.round` tie rules.
///
/// Fractional precision is truncated toward zero. Precisions beyond what an
/// `f64` can scale preserve the closest representable value (or signed zero
/// for sufficiently large negative precision).
pub fn round_number(value: f64, digits: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    if !digits.is_finite() {
        return f64::NAN;
    }
    let scale = 10_f64.powf(digits.trunc());
    if scale.is_infinite() {
        // The input already carries less precision than the requested scale can express.
        return value;
    }
    if scale == 0.0 {
        // Preserve JavaScript's observable negative zero when rounding negative values.
        return 0.0_f64.copysign(value);
    }
    let shifted = value * scale;
    if !shifted.is_finite() {
        // Scaling overflow cannot change any representable digit in the input.
        return value;
    }
    let lower = shifted.floor();
    // Exact ties move toward positive infinity, matching `Math.round` for negatives too.
    let rounded = if shifted - lower < 0.5 {
        lower
    } else {
        lower + 1.0
    };
    let rounded = if rounded == 0.0 {
        0.0_f64.copysign(value)
    } else {
        rounded
    };
    rounded / scale
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
        // UTF-8 byte order preserves Unicode scalar order. ASCII digits cannot
        // occur within a multibyte character, so digit runs still begin here.
        let order = left_bytes[a].cmp(&right_bytes[b]);
        if order != Ordering::Equal {
            return order;
        }
        a += 1;
        b += 1;
    }
    left_bytes
        .len()
        .cmp(&right_bytes.len())
        .then_with(|| left_bytes.cmp(right_bytes))
}

/// Encode natural order as a byte key for sorting many paths without reparsing
/// digit runs during comparisons. Non-digit UTF-8 bytes retain scalar order.
pub(crate) fn natural_sort_key(path: &str) -> Vec<u8> {
    let bytes = path.as_bytes();
    let digit_runs = usize::from(bytes.first().is_some_and(u8::is_ascii_digit))
        + bytes
            .windows(2)
            .filter(|pair| !pair[0].is_ascii_digit() && pair[1].is_ascii_digit())
            .count();
    // Common short runs need a marker and two one-byte lengths. Longer headers
    // grow the vector if needed; leading zeroes can shorten the significant run.
    let capacity = bytes.len() + digit_runs * 3;
    let mut key = Vec::with_capacity(capacity);
    append_natural_sort_key(path, &mut key);
    key
}

/// Append a key to query-owned storage, allowing many paths to share one arena.
pub(crate) fn append_natural_sort_key(path: &str, key: &mut Vec<u8>) {
    let bytes = path.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at].is_ascii_digit() {
            let end = digit_end(bytes, at);
            let run = &bytes[at..end];
            let zeros = leading_zeros(run);
            let significant = &run[zeros.min(run.len() - 1)..];
            // All digit runs belong between '/' and ':'. Within that range,
            // order-preserving lengths compare magnitude, then digits
            // compare value, and the zero count breaks equal-number ties.
            key.push(b'0');
            natural_key_length(key, significant.len());
            key.extend_from_slice(significant);
            natural_key_length(key, zeros);
            at = end;
        } else {
            key.push(bytes[at]);
            at += 1;
        }
    }
}

/// Small lengths occupy one byte. Larger values sort after every small value,
/// first by byte width and then by big-endian magnitude. The prefix keeps the
/// encoding self-delimiting without eight-byte headers on ordinary filenames.
fn natural_key_length(key: &mut Vec<u8>, value: usize) {
    if value < 128 {
        key.push(value as u8);
    } else {
        let width = ((usize::BITS - value.leading_zeros()).div_ceil(8)) as usize;
        let bytes = value.to_be_bytes();
        key.push(128 + width as u8);
        key.extend_from_slice(&bytes[bytes.len() - width..]);
    }
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
        // Integer formatting avoids the floating-point formatter for ordinary
        // property values. Keep negative zero and out-of-range integers exact.
        if value == 0.0 && value.is_sign_negative() {
            "-0".into()
        } else if value >= i64::MIN as f64 && value < i64::MAX as f64 {
            (value as i64).to_string()
        } else {
            format!("{value:.0}")
        }
    } else {
        value.to_string()
    }
}

pub fn sanitize_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| display_character(*character))
        .collect()
}

/// Sanitize an already-owned cell string without allocating a second buffer.
pub(crate) fn sanitize_owned_text(mut value: String) -> String {
    // C1 controls all start with UTF-8 byte C2. Without that byte or a forbidden
    // ASCII control, the owned text needs no decoding or in-place rewriting.
    if value.bytes().any(|byte| {
        byte == 0xC2 || (byte.is_ascii_control() && !matches!(byte, b'\t' | b'\n' | b'\r'))
    }) {
        value.retain(display_character);
    }
    value
}

fn display_character(character: char) -> bool {
    let code = character as u32;
    !((code <= 0x08)
        || matches!(code, 0x0B | 0x0C)
        || (0x0E..=0x1F).contains(&code)
        || (0x7F..=0x9F).contains(&code))
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
    fn rounds_numbers_with_javascript_ties_and_decimal_precision() {
        assert_eq!(round_number(2.5, 0.0), 3.0);
        assert_eq!(round_number(-2.5, 0.0), -2.0);
        assert_eq!(round_number(2.345, 2.0), 2.35);
        assert_eq!(round_number(123.4, -1.0), 120.0);
        assert!(round_number(f64::NAN, 0.0).is_nan());
        assert_eq!(round_number(f64::INFINITY, 2.0), f64::INFINITY);
    }

    #[test]
    fn natural_sort_keys_match_original_comparator() {
        // The original scalar-by-scalar comparator is the compatibility oracle
        // for cached sort keys, including digit-run boundaries.
        fn original_natural_cmp(left: &str, right: &str) -> Ordering {
            let left_bytes = left.as_bytes();
            let right_bytes = right.as_bytes();
            let (mut a, mut b) = (0, 0);
            while a < left_bytes.len() && b < right_bytes.len() {
                if left_bytes[a].is_ascii_digit() && right_bytes[b].is_ascii_digit() {
                    let a_end = digit_end(left_bytes, a);
                    let b_end = digit_end(right_bytes, b);
                    let a_zero = leading_zeros(&left_bytes[a..a_end]);
                    let b_zero = leading_zeros(&right_bytes[b..b_end]);
                    let a_significant =
                        &left_bytes[(a + a_zero).min(a_end.saturating_sub(1))..a_end];
                    let b_significant =
                        &right_bytes[(b + b_zero).min(b_end.saturating_sub(1))..b_end];
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

        let mut suffixes = vec![String::new()];
        let mut frontier = vec![String::new()];
        for _ in 0..3 {
            frontier = frontier
                .iter()
                .flat_map(|prefix| {
                    ["0", "1", "2", "a", "/", ".", "é", "🦀"].map(|part| format!("{prefix}{part}"))
                })
                .collect();
            suffixes.extend(frontier.iter().cloned());
        }
        suffixes.extend(
            [
                "n2",
                "n20",
                "n2a",
                "n02",
                "n002",
                "n000",
                "n00/2",
                "n999999999999999999999",
                "n1000000000000000000000",
                ":",
                "A2",
                "a2",
                "\u{7f}",
                "\u{80}",
                "\u{7ff}",
                "\u{800}",
                "\u{ffff}",
                "\u{10000}",
                "\u{10ffff}",
            ]
            .map(str::to_owned),
        );
        suffixes.extend([
            format!("n{}1", "0".repeat(127)),
            format!("n{}1", "0".repeat(128)),
            format!("n{}", "9".repeat(127)),
            format!("n1{}", "0".repeat(127)),
            format!("n{}1", "0".repeat(255)),
            format!("n{}1", "0".repeat(256)),
            format!("n{}", "9".repeat(255)),
            format!("n1{}", "0".repeat(255)),
        ]);
        for prefix in ["", "folder-02/note-", "🦀/0002/"] {
            let paths = suffixes
                .iter()
                .map(|suffix| format!("{prefix}{suffix}"))
                .collect::<Vec<_>>();
            let keys = paths
                .iter()
                .map(|path| natural_sort_key(path))
                .collect::<Vec<_>>();
            for (left_index, left) in paths.iter().enumerate() {
                for (right_index, right) in paths.iter().enumerate() {
                    let expected = original_natural_cmp(left, right);
                    assert_eq!(natural_cmp(left, right), expected, "{left:?} vs {right:?}");
                    assert_eq!(
                        keys[left_index].cmp(&keys[right_index]),
                        expected,
                        "key {left:?} vs {right:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn natural_order_preserves_unicode_with_ascii_digit_runs() {
        let expected = [
            "note2",
            "note02",
            "note10",
            "nöté",
            "nöté2",
            "nöté02",
            "nöté10",
            "\u{7f}",
            "\u{80}",
            "\u{7ff}",
            "\u{800}",
            "筆記2",
            "筆記02",
            "筆記10",
            "\u{ffff}",
            "\u{10000}",
            "🦀2",
            "🦀10",
            "\u{10ffff}",
        ];
        // Check every pair, including prefixes, digit widths and UTF-8 widths.
        for (left_index, left) in expected.iter().enumerate() {
            for (right_index, right) in expected.iter().enumerate() {
                assert_eq!(
                    natural_cmp(left, right),
                    left_index.cmp(&right_index),
                    "{left:?} compared with {right:?}",
                );
            }
        }
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
    fn integer_display_matches_exact_float_formatting_at_boundaries() {
        for value in [
            0.0,
            -0.0,
            1.0,
            -1.0,
            42.0,
            -42.0,
            9_007_199_254_740_992.0,
            i64::MIN as f64,
            i64::MIN as f64 - 2048.0,
            i64::MAX as f64 - 1024.0,
            i64::MAX as f64,
            1e23,
            -1e23,
            f64::MAX,
            f64::MIN,
        ] {
            assert_eq!(number_string(value), format!("{value:.0}"));
        }
        assert_eq!(number_string(1.25), "1.25");
        assert_eq!(number_string(f64::NAN), "NaN");
        assert_eq!(number_string(f64::INFINITY), "Infinity");
        assert_eq!(number_string(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn owned_sanitization_preserves_unicode_and_permitted_whitespace() {
        let value = format!(
            "🦀{}\t\n\ré",
            (0..=0x9f).filter_map(char::from_u32).collect::<String>()
        );
        assert_eq!(sanitize_owned_text(value.clone()), sanitize_text(&value));
        assert_eq!(
            sanitize_owned_text("🦀\t\n\ré\u{a0}¢¿".into()),
            "🦀\t\n\ré\u{a0}¢¿"
        );
        // C1 controls and allowed Latin-1 characters share the C2 lead byte.
        // Exercise them without ASCII controls that could mask a missed branch.
        for code in 0x80..0xc0 {
            let text = format!("before{}after", char::from_u32(code).unwrap());
            assert_eq!(sanitize_owned_text(text.clone()), sanitize_text(&text));
        }
    }

    #[test]
    fn strips_c0_and_c1_controls() {
        assert_eq!(sanitize_text("safe\u{1b}[2Jtext\u{85}"), "safe[2Jtext");
    }
}
