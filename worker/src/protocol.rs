//! Strict, typed JSON-lines protocol definitions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::query::ResultRow;

/// Preview row count used when a query does not specify one.
pub const DEFAULT_PREVIEW_ROWS: usize = 50;

/// A protocol request decoded before it reaches mutable worker state.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    /// Client-selected request identifier used to correlate the response.
    pub id: RequestId,
    /// Closed request operation and its typed parameters.
    pub request: Request,
}

/// The closed set of worker operations.
#[derive(Debug, Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Request {
    /// Create or replace the active vault snapshot.
    Initialize(InitializeParams),
    /// Evaluate a Base source against the active vault.
    Query(QueryParams),
    /// Retrieve all rows retained for a prior query.
    FetchRows(FetchRowsParams),
    /// Install an unsaved buffer overlay and publish a fresh index.
    OverlayUpsert(OverlayUpsertParams),
    /// Remove an overlay after its contents have been written to disk.
    OverlayCommit(OverlayPathParams),
    /// Remove an overlay for a detached or reverted buffer.
    OverlayRemove(OverlayPathParams),
    /// Return worker diagnostics without changing state.
    Inspect(EmptyParams),
    /// Emit an acknowledgement and stop the worker.
    Shutdown(EmptyParams),
}

/// Request IDs fit exactly in Lua numbers.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct RequestId(u64);

impl RequestId {
    /// ID used when malformed input cannot supply a valid request ID.
    pub const INVALID: Self = Self(0);

    /// Return the validated numeric ID.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for RequestId {
    type Error = &'static str;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value <= 9_007_199_254_740_991 {
            Ok(Self(value))
        } else {
            Err("request id exceeds Lua integer precision")
        }
    }
}

impl From<RequestId> for u64 {
    fn from(value: RequestId) -> Self {
        value.0
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitializeParams {
    /// Filesystem path of the vault to scan.
    pub vault_root: String,
    /// Optional filesystem-time overrides keyed by vault-relative path.
    #[serde(default)]
    pub metadata_overrides: BTreeMap<String, MetadataOverrideParams>,
    /// Optional resource-limit overrides applied to this vault.
    #[serde(default)]
    pub limits: LimitsPatch,
}

/// Typed timestamp override supplied during initialization.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataOverrideParams {
    /// Optional RFC 3339 creation time.
    pub ctime: Option<String>,
    /// Optional RFC 3339 modification time.
    pub mtime: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsPatch {
    /// Maximum text bytes accepted from vault files and overlays.
    pub source_bytes: Option<usize>,
    /// Maximum bytes accepted by one expression source.
    pub expression_bytes: Option<usize>,
    /// Maximum wall-clock evaluation time in milliseconds.
    pub query_ms: Option<u64>,
    /// Maximum evaluator operations per query.
    pub evaluation_steps: Option<u64>,
    /// Maximum rows retained for one result set.
    pub result_rows: Option<usize>,
    /// Maximum serialized bytes for a result payload.
    pub result_bytes: Option<usize>,
}

/// Typed query inputs accepted by the evaluator.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryParams {
    /// Inline or file-backed Base source to evaluate.
    pub source: QuerySource,
    /// Vault-relative path of the note hosting the Base.
    pub host_path: String,
    /// Optional named view to select instead of the first view.
    pub view_name: Option<String>,
    /// Number of rows included inline in the result.
    #[serde(default = "default_preview_rows")]
    pub preview_rows: usize,
}

fn default_preview_rows() -> usize {
    DEFAULT_PREVIEW_ROWS
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuerySource {
    /// Base text sent directly by the client.
    Inline {
        /// YAML Base source text.
        text: String,
        /// Stable caller-selected source identity.
        source_id: Option<String>,
    },
    /// Base text read from a vault-relative file.
    File {
        /// Vault-relative Base file path.
        path: String,
        /// Stable caller-selected source identity.
        source_id: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchRowsParams {
    /// ID returned by a prior successful query.
    pub result_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayUpsertParams {
    /// Vault-relative file path represented by the unsaved buffer.
    pub path: String,
    /// Complete unsaved buffer contents.
    pub contents: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayPathParams {
    /// Vault-relative path of the overlay to remove.
    pub path: String,
}

/// Empty parameter object required by operations without inputs.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyParams {}

/// A typed worker response.
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseEnvelope {
    /// ID of the request this response completes.
    pub id: RequestId,
    /// Typed success or failure payload.
    pub response: Response,
}

impl ResponseEnvelope {
    /// Construct a successful response for one request.
    pub fn success(id: RequestId, result: Success) -> Self {
        Self {
            id,
            response: Response::Success { result },
        }
    }

    /// Construct a failure response for one request.
    pub fn error(id: RequestId, error: ErrorPayload) -> Self {
        Self {
            id,
            response: Response::Error { error },
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// Operation completed with typed result data.
    Success { result: Success },
    /// Operation failed with a stable code and diagnostic message.
    Error { error: ErrorPayload },
}

#[derive(Debug, Serialize)]
#[serde(tag = "method", content = "data", rename_all = "snake_case")]
pub enum Success {
    /// Initialization result.
    Initialize(InitializeResult),
    /// Query summary and preview rows.
    Query(QueryResult),
    /// Complete rows retained for a prior query.
    FetchRows(FetchRowsResult),
    /// Generation after an overlay upsert.
    OverlayUpsert(GenerationResult),
    /// Generation after an overlay commit.
    OverlayCommit(GenerationResult),
    /// Generation after overlay removal.
    OverlayRemove(GenerationResult),
    /// Worker diagnostic snapshot.
    Inspect(InspectResult),
    /// Shutdown acknowledgement.
    Shutdown(EmptyResult),
}

#[derive(Debug, Serialize)]
pub struct ErrorPayload {
    /// Stable machine-readable failure class.
    pub code: String,
    /// Human-readable diagnostic for the caller.
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct InitializeResult {
    /// New index generation.
    pub generation: u64,
    /// Number of indexed records.
    pub files: usize,
}

#[derive(Debug, Serialize)]
pub struct GenerationResult {
    /// New index generation after an overlay mutation.
    pub generation: u64,
}

#[derive(Debug, Serialize)]
pub struct FetchRowsResult {
    /// ID supplied by the request.
    pub result_id: String,
    /// Complete rendered rows cached for that ID.
    pub rows: Vec<ResultRow>,
}

#[derive(Debug, Serialize)]
pub struct QueryResult {
    /// Generation-scoped ID used by `fetch_rows`.
    pub result_id: String,
    /// Caller-supplied source identity.
    pub source_id: String,
    /// Selected table view.
    pub view: View,
    /// Rendered table columns.
    pub columns: Vec<Column>,
    /// Initial visible rows.
    pub preview_rows: Vec<ResultRow>,
    /// Number of records matching filters before view limit.
    pub matched_count: usize,
    /// Number of rows retained by the view limit.
    pub view_count: usize,
    /// Number of rows included in `preview_rows`.
    pub preview_count: usize,
    /// Whether the view limit omitted matching rows.
    pub truncated: bool,
    /// Non-fatal worker warnings.
    pub warnings: Vec<String>,
    /// Named timing measurements in milliseconds.
    pub timings: BTreeMap<String, u64>,
    /// Index generation used for evaluation.
    pub index_generation: u64,
}

#[derive(Debug, Serialize)]
pub struct Column {
    /// Stable expression key.
    pub key: String,
    /// User-facing column label.
    pub label: String,
}

#[derive(Debug, Serialize)]
pub struct View {
    /// Selected view name.
    pub name: String,
    /// View kind, currently always `table`.
    #[serde(rename = "type")]
    pub kind: &'static str,
}

#[derive(Debug, Serialize)]
pub struct InspectResult {
    /// Active index generation, or zero before initialization.
    pub generation: u64,
    /// Number of indexed records.
    pub files: usize,
    /// Active overlay paths.
    pub overlays: Vec<String>,
    /// Count of skipped non-UTF-8 paths.
    pub skipped_non_utf8: usize,
    /// Bounded redacted sample of skipped paths.
    pub skipped_non_utf8_examples: Vec<String>,
    /// Recent watcher errors.
    pub watcher_errors: Vec<String>,
}

/// Empty successful payload used by shutdown.
#[derive(Debug, Default, Serialize)]
pub struct EmptyResult {}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    /// Asynchronous state-change event.
    pub event: Event,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A new index snapshot was published.
    IndexChanged { generation: u64, paths: Vec<String> },
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum WorkerEnvelope {
    /// Request-correlated worker response.
    Response(ResponseEnvelope),
    /// Asynchronous worker event.
    Event(EventEnvelope),
}

/// Decode one strict request line before stateful dispatch.
pub fn decode(line: &str) -> Result<RequestEnvelope, String> {
    serde_json::from_str(line).map_err(|error| error.to_string())
}
