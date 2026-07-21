//! Resource limits that bound untrusted Base sources and query evaluation.

use std::{fs, io::Read, path::Path};

use crate::{
    error::{Result, WorkerError},
    protocol::LimitsPatch,
};

#[derive(Clone, Copy, Debug)]
/// Limits applied while parsing, indexing, evaluating, and serializing a request.
pub struct Limits {
    /// Maximum bytes accepted from vault files and overlay text.
    pub input_bytes: usize,
    /// Maximum bytes accepted for one expression source.
    pub expression_bytes: usize,
    /// Maximum nodes permitted in an expression tree.
    pub ast_nodes: usize,
    /// Maximum nesting depth for expressions and parser recursion.
    pub ast_depth: usize,
    /// Maximum nesting depth for parsed YAML values.
    pub yaml_depth: usize,
    /// Maximum formulas declared by one Base.
    pub formulas: usize,
    /// Maximum recursive formula dependency depth.
    pub formula_depth: usize,
    /// Maximum expression-evaluation operations per query.
    pub evaluation_steps: u64,
    /// Maximum wall-clock milliseconds spent evaluating a query.
    pub evaluation_ms: u64,
    /// Maximum rows retained for a result set.
    pub result_rows: usize,
    /// Maximum serialized bytes for a result response or row set.
    pub result_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            input_bytes: 1024 * 1024,
            expression_bytes: 64 * 1024,
            ast_nodes: 10_000,
            ast_depth: 64,
            yaml_depth: 64,
            formulas: 256,
            formula_depth: 64,
            evaluation_steps: 5_000_000,
            evaluation_ms: 2_000,
            result_rows: 10_000,
            result_bytes: 4 * 1024 * 1024,
        }
    }
}

impl Limits {
    pub fn apply(mut self, patch: LimitsPatch) -> Self {
        if let Some(value) = patch.source_bytes {
            self.input_bytes = value.get();
        }
        if let Some(value) = patch.expression_bytes {
            self.expression_bytes = value.get();
        }
        if let Some(value) = patch.query_ms {
            self.evaluation_ms = value.get();
        }
        if let Some(value) = patch.evaluation_steps {
            self.evaluation_steps = value.get();
        }
        if let Some(value) = patch.result_rows {
            self.result_rows = value.get();
        }
        if let Some(value) = patch.result_bytes {
            self.result_bytes = value.get();
        }
        self
    }

    pub fn checked_text<'a>(&self, text: &'a str) -> Result<&'a str> {
        self.check_text(text)?;
        Ok(text)
    }

    pub fn check_text(&self, text: &str) -> Result<()> {
        self.check_bytes(text.len())
    }

    /// Read UTF-8 text without allowing a file to allocate beyond the input limit.
    pub fn read_text(&self, path: &Path) -> Result<String> {
        let file = fs::File::open(path).map_err(WorkerError::io)?;
        let mut bytes = Vec::new();
        file.take(self.input_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(WorkerError::io)?;
        self.check_bytes(bytes.len())?;
        String::from_utf8(bytes).map_err(|error| {
            WorkerError::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                error.utf8_error(),
            ))
        })
    }

    fn check_bytes(&self, bytes: usize) -> Result<()> {
        if bytes > self.input_bytes {
            return Err(WorkerError::new(
                "input_too_large",
                "input exceeds the configured size limit",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU64, NonZeroUsize};

    use super::Limits;
    use crate::protocol::LimitsPatch;

    #[test]
    fn empty_patch_retains_worker_defaults() {
        let defaults = Limits::default();
        let applied = defaults.apply(LimitsPatch::default());
        assert_eq!(applied.input_bytes, defaults.input_bytes);
        assert_eq!(applied.evaluation_ms, defaults.evaluation_ms);
        assert_eq!(applied.result_rows, defaults.result_rows);
    }

    #[test]
    fn patch_changes_only_supplied_limits() {
        let defaults = Limits::default();
        let applied = defaults.apply(LimitsPatch {
            query_ms: Some(NonZeroU64::new(7).unwrap()),
            result_bytes: Some(NonZeroUsize::new(9).unwrap()),
            ..LimitsPatch::default()
        });
        assert_eq!(applied.evaluation_ms, 7);
        assert_eq!(applied.result_bytes, 9);
        assert_eq!(applied.input_bytes, defaults.input_bytes);
    }
}
