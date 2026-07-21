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

pub fn parse_mapping(text: &str, limits: Limits) -> Result<BTreeMap<String, DataValue>> {
    limits.check_text(text)?;
    let mut receiver = SafetyReceiver::default();
    Parser::new_from_str(text)
        .load(&mut receiver, true)
        .map_err(|error| WorkerError::new("yaml_parse", error.to_string()))?;
    if receiver.unsafe_feature {
        return Err(WorkerError::new(
            "unsafe_yaml",
            "YAML aliases and tags are not supported",
        ));
    }
    if receiver.documents != 1 {
        return Err(WorkerError::new(
            "yaml_parse",
            "exactly one YAML document is required",
        ));
    }
    if receiver.duplicate {
        return Err(WorkerError::new("yaml_parse", "duplicate mapping key"));
    }
    if receiver.complex_key {
        return Err(WorkerError::new(
            "yaml_parse",
            "complex mapping keys are not supported",
        ));
    }
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
