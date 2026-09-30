//! Safe YAML parsing that rejects aliases, tags, duplicate keys, and deep input.

use std::collections::{BTreeMap, HashSet};

use yaml_rust2::{
    Yaml, YamlLoader,
    parser::{Event, EventReceiver, Parser},
};

use crate::{
    error::{Result, WorkerError},
    expression::safe_key,
    limits::Limits,
    value::DataValue,
};

#[derive(Default)]
/// Event receiver that identifies unsupported YAML constructs before conversion.
struct SafetyReceiver {
    /// Whether an alias, anchor, tag, or merge key was encountered.
    unsafe_feature: bool,
    /// Number of YAML documents seen in the source stream.
    documents: usize,
    /// Open sequences and mappings used to validate scalar positions.
    stack: Vec<Container>,
    /// Whether a mapping repeated a scalar key.
    duplicate: bool,
    /// Whether a mapping used a non-scalar key.
    complex_key: bool,
}

/// Stack entry used to track mapping keys while scanning parser events.
enum Container {
    /// Sequence whose items do not require key tracking.
    Sequence,
    /// Mapping whose scalar keys are checked for safety and duplicates.
    Mapping {
        /// Whether the next scalar or container occupies the key position.
        expecting_key: bool,
        /// Scalar keys already encountered in this mapping.
        keys: HashSet<String>,
    },
}

impl EventReceiver for SafetyReceiver {
    fn on_event(&mut self, event: Event) {
        match event {
            Event::DocumentStart => self.documents += 1,
            Event::Alias(_) => self.unsafe_feature = true,
            Event::Scalar(value, _, anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.unsafe_feature = true;
                }
                self.scalar(value);
            }
            Event::SequenceStart(anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.unsafe_feature = true;
                }
                self.container_value();
                self.stack.push(Container::Sequence);
            }
            Event::MappingStart(anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.unsafe_feature = true;
                }
                self.container_value();
                self.stack.push(Container::Mapping {
                    expecting_key: true,
                    keys: HashSet::new(),
                });
            }
            Event::SequenceEnd | Event::MappingEnd => {
                self.stack.pop();
            }
            _ => {}
        }
    }
}

impl SafetyReceiver {
    /// Validate collected safety errors in their established priority order.
    fn check(&self) -> Result<()> {
        if self.unsafe_feature {
            return Err(WorkerError::new(
                "unsafe_yaml",
                "YAML aliases and tags are not supported",
            ));
        }
        if self.documents != 1 {
            return Err(WorkerError::new(
                "yaml_parse",
                "exactly one YAML document is required",
            ));
        }
        if self.duplicate {
            return Err(WorkerError::new("yaml_parse", "duplicate mapping key"));
        }
        if self.complex_key {
            return Err(WorkerError::new(
                "yaml_parse",
                "complex mapping keys are not supported",
            ));
        }
        Ok(())
    }

    fn scalar(&mut self, value: String) {
        if let Some(Container::Mapping {
            expecting_key,
            keys,
        }) = self.stack.last_mut()
        {
            if *expecting_key {
                if value == "<<" {
                    self.unsafe_feature = true;
                }
                if !keys.insert(value) {
                    self.duplicate = true;
                }
            }
            *expecting_key = !*expecting_key;
        }
    }

    fn container_value(&mut self) {
        if let Some(Container::Mapping { expecting_key, .. }) = self.stack.last_mut() {
            if *expecting_key {
                self.complex_key = true;
            }
            *expecting_key = !*expecting_key;
        }
    }
}

/// Parse one mapping with the root at depth zero and scalar keys uncounted.
/// Over-limit values stop the event scan; previously observed safety errors
/// retain their priority, while diagnostics in the unread suffix are skipped.
pub fn parse_mapping(text: &str, limits: Limits) -> Result<BTreeMap<String, DataValue>> {
    limits.check_text(text)?;
    let mut receiver = SafetyReceiver::default();
    // `Parser::load` recursively visits collections, so even the safety pass
    // must consume events incrementally and stop before descending too far.
    let mut parser = Parser::new_from_str(text);
    loop {
        let (event, _) = parser
            .next_token()
            .map_err(|error| WorkerError::new("yaml_parse", error.to_string()))?;
        // The root is depth zero; open containers count the depth of the next
        // value. Scalar mapping keys are not values visited by `convert`.
        let too_deep = match &event {
            Event::SequenceStart(..) | Event::MappingStart(..) => {
                receiver.stack.len() > limits.yaml_depth
            }
            Event::Scalar(..)
                if !matches!(
                    receiver.stack.last(),
                    Some(Container::Mapping {
                        expecting_key: true,
                        ..
                    })
                ) =>
            {
                receiver.stack.len() > limits.yaml_depth
            }
            _ => false,
        };
        let stream_end = event == Event::StreamEnd;
        receiver.on_event(event);
        if too_deep {
            // Keep the priority of safety errors already observed, but do not
            // parse the remaining suffix merely to choose another diagnostic.
            receiver.check()?;
            return Err(WorkerError::new(
                "yaml_too_deep",
                "YAML nesting exceeds its limit",
            ));
        }
        if stream_end {
            break;
        }
    }
    receiver.check()?;
    let documents = YamlLoader::load_from_str(text)
        .map_err(|error| WorkerError::new("yaml_parse", error.to_string()))?;
    let value = documents
        .first()
        .ok_or_else(|| WorkerError::new("yaml_parse", "Base document must be a mapping"))?;
    let converted = convert(value, 0, limits)?;
    match converted {
        DataValue::Object(value) => Ok(value),
        _ => Err(WorkerError::new(
            "yaml_parse",
            "Base document must be a mapping",
        )),
    }
}

fn convert(value: &Yaml, depth: usize, limits: Limits) -> Result<DataValue> {
    if depth > limits.yaml_depth {
        return Err(WorkerError::new(
            "yaml_too_deep",
            "YAML nesting exceeds its limit",
        ));
    }
    match value {
        Yaml::Null => Ok(DataValue::Null),
        Yaml::Boolean(value) => Ok(DataValue::Bool(*value)),
        Yaml::Integer(value) => Ok(DataValue::Number(*value as f64)),
        Yaml::Real(value) => {
            let number = value
                .parse::<f64>()
                .map_err(|_| WorkerError::new("yaml_parse", "invalid YAML number"))?;
            if !number.is_finite() {
                return Err(WorkerError::new(
                    "unsafe_yaml",
                    "non-finite YAML numbers are not supported",
                ));
            }
            Ok(DataValue::Number(number))
        }
        Yaml::String(value) => Ok(DataValue::String(value.clone())),
        Yaml::Array(values) => values
            .iter()
            .map(|value| convert(value, depth + 1, limits))
            .collect::<Result<Vec<_>>>()
            .map(DataValue::List),
        Yaml::Hash(values) => {
            let mut object = BTreeMap::new();
            for (key, value) in values {
                let Yaml::String(key) = key else {
                    return Err(WorkerError::new(
                        "yaml_parse",
                        "mapping keys must be strings",
                    ));
                };
                safe_key(key)?;
                object.insert(key.clone(), convert(value, depth + 1, limits)?);
            }
            Ok(DataValue::Object(object))
        }
        Yaml::Alias(_) => Err(WorkerError::new(
            "unsafe_yaml",
            "YAML aliases and tags are not supported",
        )),
        Yaml::BadValue => Err(WorkerError::new("yaml_parse", "invalid YAML")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_boundary_counts_values_and_empty_containers_but_not_scalar_keys() {
        for (open, close) in [("[", "]"), ("{key: ", "}")] {
            for leaf in ["0", "null", "[]", "{}"] {
                for depth in [63, 64, 65] {
                    let source = format!(
                        "x: {}{leaf}{}",
                        open.repeat(depth - 1),
                        close.repeat(depth - 1)
                    );
                    let result = parse_mapping(&source, Limits::default());
                    if depth <= 64 {
                        assert!(result.is_ok(), "depth {depth}: {source}");
                    } else {
                        assert_eq!(result.unwrap_err().code, "yaml_too_deep");
                    }
                }
            }
        }
        let limits = Limits {
            yaml_depth: 0,
            ..Limits::default()
        };
        assert!(parse_mapping("{}", limits).is_ok());
        assert_eq!(
            parse_mapping("x: null", limits).unwrap_err().code,
            "yaml_too_deep"
        );
    }

    #[test]
    fn rejects_deep_block_sequences_and_mappings_before_recursive_loading() {
        let sequences = format!("x:\n  {}0\n", "- ".repeat(20_000));
        let mappings = (0..1400)
            .map(|depth| format!("{}x:\n", " ".repeat(depth)))
            .collect::<String>();
        for source in [sequences, mappings] {
            assert!(source.len() < Limits::default().input_bytes);
            assert_eq!(
                parse_mapping(&source, Limits::default()).unwrap_err().code,
                "yaml_too_deep"
            );
        }
    }

    #[test]
    fn stops_at_depth_limit_instead_of_parsing_the_remaining_suffix() {
        // The suffix is malformed, but inspecting it would require continuing
        // beyond the value-depth budget. A quoted scalar stays one value.
        for suffix in ["broken: [", "a: &a value\nb: *a", "a: 1\na: 2"] {
            let source = format!("x:\n  {}0\n{suffix}", "- ".repeat(65));
            assert_eq!(
                parse_mapping(&source, Limits::default()).unwrap_err().code,
                "yaml_too_deep"
            );
        }
        let quoted = format!("x: '{}'", "[".repeat(20_000));
        assert!(parse_mapping(&quoted, Limits::default()).is_ok());
    }

    #[test]
    fn retains_observed_safety_error_priority_before_depth_rejection() {
        let deep = format!("x:\n  {}0\n", "- ".repeat(65));
        for (prefix, code, message) in [
            ("a: &a value\nb: *a\n", "unsafe_yaml", "aliases and tags"),
            ("a: !!str value\n", "unsafe_yaml", "aliases and tags"),
            ("a: !custom []\n", "unsafe_yaml", "aliases and tags"),
            ("<<: value\n", "unsafe_yaml", "aliases and tags"),
            ("a: 1\na: 2\n", "yaml_parse", "duplicate mapping key"),
            ("? [a, b]\n: value\n", "yaml_parse", "complex mapping keys"),
            (
                "---\na: 1\n---\n",
                "yaml_parse",
                "exactly one YAML document",
            ),
        ] {
            let error = parse_mapping(&format!("{prefix}{deep}"), Limits::default()).unwrap_err();
            assert_eq!(error.code, code, "{prefix}");
            assert!(error.message.contains(message), "{error}");
        }
        let limits = Limits {
            input_bytes: deep.len() - 1,
            ..Limits::default()
        };
        assert_eq!(
            parse_mapping(&deep, limits).unwrap_err().code,
            "input_too_large"
        );
        // Safety metadata on the depth-breaching event is still observed.
        let limits = Limits {
            yaml_depth: 0,
            ..Limits::default()
        };
        for source in ["x: !!str value", "x: &a []", "x: !custom {}"] {
            assert_eq!(
                parse_mapping(source, limits).unwrap_err().code,
                "unsafe_yaml"
            );
        }
    }

    #[test]
    fn preserves_block_and_flow_value_semantics() {
        let block =
            "---\nempty:\nitems:\n  - true\n  - 'null'\n  - 2\ntext: |\n  [brackets]\n...\n";
        let flow = "{empty: null, items: [true, 'null', 2], text: \"[brackets]\\n\"}";
        assert_eq!(
            parse_mapping(block, Limits::default()).unwrap(),
            parse_mapping(flow, Limits::default()).unwrap()
        );
    }

    #[test]
    fn accepts_core_mapping_and_rejects_unsafe_yaml() {
        let value = parse_mapping(
            "name: Test\ncount: 2\nitems: [true, null]",
            Limits::default(),
        );
        assert!(value.is_ok());
        for source in [
            "x: &x value\ny: *x",
            "x: !!str value",
            "x: !custom value",
            "x: 1\nx: 2",
            "<<: value",
            "? [a, b]\n: value",
            "x: .nan",
        ] {
            assert!(
                parse_mapping(source, Limits::default()).is_err(),
                "{source}"
            );
        }
    }
}
