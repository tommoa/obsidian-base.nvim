//! Stateful worker operations over an explicitly initialized vault.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    error::{Result, WorkerError},
    index::{Index, MetadataOverride, Overlay, metadata_overrides},
    limits::Limits,
    path::{canonical_contained, relative_vault_path},
    protocol::{
        FetchRowsParams, FetchRowsResult, GenerationResult, InitializeParams, InitializeResult,
        InspectResult, OverlayPathParams, OverlayUpsertParams, QueryParams, QuerySource, Success,
    },
    query::{QueryInput, ResultRow, execute},
};

/// One published index generation, retained internally until the actor emits it.
pub struct IndexChange {
    pub generation: u64,
    pub paths: Vec<String>,
}

/// A decoded request result with its optional index transition.
pub struct HandledRequest {
    pub success: Success,
    pub index_change: Option<IndexChange>,
}

/// All state that is valid only after a vault has been initialized.
struct InitializedService {
    root: PathBuf,
    index: Index,
    overlays: BTreeMap<String, Overlay>,
    metadata: HashMap<String, MetadataOverride>,
    limits: Limits,
    generation: u64,
    result_sequence: u64,
    results: HashMap<String, Vec<ResultRow>>,
    watcher_errors: Vec<String>,
}

impl InitializedService {
    fn build(&self, overlays: &BTreeMap<String, Overlay>) -> Result<Index> {
        Index::build(
            self.root.clone(),
            overlays,
            &self.metadata,
            self.limits,
            self.watcher_errors.clone(),
        )
    }

    fn publish(&mut self, index: Index) {
        self.index = index;
        self.generation += 1;
        self.results.clear();
    }

    fn read_source(&self, path: &str) -> Result<String> {
        if let Some(overlay) = self.overlays.get(path) {
            return Ok(overlay.contents.clone());
        }
        let path = canonical_contained(&self.root, path)?;
        self.limits.read_text(&path)
    }
}

/// Owns an optional initialized vault and its result cache.
pub struct WorkerService {
    state: Option<InitializedService>,
}

impl Default for WorkerService {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkerService {
    /// Create an uninitialized worker.
    pub fn new() -> Self {
        Self { state: None }
    }

    /// Dispatch a decoded command. Only this boundary can observe an uninitialized service.
    /// Apply one typed request and return its typed success payload.
    ///
    /// `Shutdown` returns its acknowledgement; the actor owns process termination.
    pub fn handle(&mut self, request: crate::protocol::Request) -> Result<HandledRequest> {
        let (success, index_change) = match request {
            crate::protocol::Request::Initialize(params) => {
                (Success::Initialize(self.initialize(params)?), None)
            }
            crate::protocol::Request::Query(params) => (Success::Query(self.query(params)?), None),
            crate::protocol::Request::FetchRows(params) => {
                (Success::FetchRows(self.fetch_rows(params)?), None)
            }
            crate::protocol::Request::OverlayUpsert(params) => {
                let change = self.overlay_upsert(params)?;
                (
                    Success::OverlayUpsert(GenerationResult {
                        generation: change.generation,
                    }),
                    Some(change),
                )
            }
            crate::protocol::Request::OverlayCommit(params) => {
                let change = self.overlay_remove(params)?;
                (
                    Success::OverlayCommit(GenerationResult {
                        generation: change.generation,
                    }),
                    Some(change),
                )
            }
            crate::protocol::Request::OverlayRemove(params) => {
                let change = self.overlay_remove(params)?;
                (
                    Success::OverlayRemove(GenerationResult {
                        generation: change.generation,
                    }),
                    Some(change),
                )
            }
            crate::protocol::Request::Inspect(_) => (Success::Inspect(self.inspect()), None),
            crate::protocol::Request::Shutdown(_) => (Success::Shutdown(Default::default()), None),
        };
        Ok(HandledRequest {
            success,
            index_change,
        })
    }

    /// Build and publish a new vault snapshot from typed initialization inputs.
    pub fn initialize(&mut self, params: InitializeParams) -> Result<InitializeResult> {
        let root = Path::new(&params.vault_root)
            .canonicalize()
            .map_err(WorkerError::io)?;
        let limits = Limits::default().apply(params.limits);
        let metadata = metadata_overrides(params.metadata_overrides)?;
        let (overlays, watcher_errors, generation) = match &self.state {
            None => (BTreeMap::new(), Vec::new(), 0),
            Some(state) if state.root == root => (
                state.overlays.clone(),
                state.watcher_errors.clone(),
                state.generation,
            ),
            Some(state) => (BTreeMap::new(), Vec::new(), state.generation),
        };
        let index = Index::build(
            root.clone(),
            &overlays,
            &metadata,
            limits,
            watcher_errors.clone(),
        )?;
        let files = index.records.len();
        self.state = Some(InitializedService {
            root,
            index,
            overlays,
            metadata,
            limits,
            generation: generation + 1,
            result_sequence: 0,
            results: HashMap::new(),
            watcher_errors,
        });
        Ok(InitializeResult {
            generation: generation + 1,
            files,
        })
    }

    /// Evaluate a typed Base query against the initialized vault.
    pub fn query(&mut self, params: QueryParams) -> Result<crate::protocol::QueryResult> {
        let state = self.initialized_mut()?;
        let host_path = relative_vault_path(&params.host_path)?;
        let (text, source_id) = match params.source {
            QuerySource::Inline { text, source_id } => {
                state.limits.checked_text(&text)?;
                (text, source_id.unwrap_or_else(|| "inline".to_owned()))
            }
            QuerySource::File { path, source_id } => {
                let path = relative_vault_path(&path)?;
                let text = state.read_source(&path)?;
                (text, source_id.unwrap_or(path))
            }
        };
        let sequence = state.result_sequence + 1;
        let mut result_id_allocated = false;
        let output = execute(
            &state.index,
            state.limits,
            state.generation,
            sequence,
            &mut result_id_allocated,
            QueryInput {
                text,
                source_id,
                host_path,
                view_name: params.view_name,
                preview_rows: params.preview_rows,
            },
        );
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                if result_id_allocated {
                    state.result_sequence = sequence;
                }
                return Err(error);
            }
        };
        state.result_sequence = sequence;
        state
            .results
            .insert(output.result.result_id.clone(), output.rows);
        Ok(output.result)
    }

    /// Return the complete cached rows for a successful query.
    pub fn fetch_rows(&self, params: FetchRowsParams) -> Result<FetchRowsResult> {
        let state = self.initialized_ref()?;
        let rows = state
            .results
            .get(&params.result_id)
            .ok_or_else(|| WorkerError::new("unknown_result", "result is no longer available"))?;
        Ok(FetchRowsResult {
            result_id: params.result_id,
            rows: rows.clone(),
        })
    }

    /// Replace one unsaved buffer overlay and publish a new index generation.
    pub fn overlay_upsert(&mut self, params: OverlayUpsertParams) -> Result<IndexChange> {
        let path = relative_vault_path(&params.path)?;
        let generation = {
            let state = self.initialized_mut()?;
            state.limits.checked_text(&params.contents)?;
            let mut overlays = state.overlays.clone();
            let created_at = overlays
                .get(&path)
                .map_or_else(now_milliseconds, |overlay| overlay.created_at);
            overlays.insert(
                path.clone(),
                Overlay {
                    contents: params.contents,
                    created_at,
                },
            );
            let index = state.build(&overlays)?;
            state.overlays = overlays;
            state.publish(index);
            state.generation
        };
        Ok(IndexChange {
            generation,
            paths: vec![path],
        })
    }

    /// Remove one overlay and publish a new index generation.
    pub fn overlay_remove(&mut self, params: OverlayPathParams) -> Result<IndexChange> {
        let path = relative_vault_path(&params.path)?;
        let generation = {
            let state = self.initialized_mut()?;
            let mut overlays = state.overlays.clone();
            overlays.remove(&path);
            let index = state.build(&overlays)?;
            state.overlays = overlays;
            state.publish(index);
            state.generation
        };
        Ok(IndexChange {
            generation,
            paths: vec![path],
        })
    }

    /// Rebuild after coalesced external filesystem notifications.
    pub fn reindex_external(&mut self, paths: Vec<String>) -> Result<IndexChange> {
        let state = self.initialized_mut()?;
        let index = state.build(&state.overlays)?;
        state.publish(index);
        Ok(IndexChange {
            generation: state.generation,
            paths,
        })
    }

    /// Return a serializable worker diagnostic snapshot.
    pub fn inspect(&self) -> InspectResult {
        match &self.state {
            None => InspectResult {
                generation: 0,
                files: 0,
                overlays: Vec::new(),
                skipped_non_utf8: 0,
                skipped_non_utf8_examples: Vec::new(),
                watcher_errors: Vec::new(),
            },
            Some(state) => InspectResult {
                generation: state.generation,
                files: state.index.records.len(),
                overlays: state.overlays.keys().cloned().collect(),
                skipped_non_utf8: state.index.diagnostics.skipped_non_utf8,
                skipped_non_utf8_examples: state
                    .index
                    .diagnostics
                    .skipped_non_utf8_examples
                    .clone(),
                watcher_errors: state.watcher_errors.clone(),
            },
        }
    }

    /// Return the canonical root only while a vault is initialized.
    pub fn root(&self) -> Option<&Path> {
        self.state.as_ref().map(|state| state.root.as_path())
    }

    /// Retain a bounded watcher diagnostic without making the index unavailable.
    pub fn record_watcher_error(&mut self, message: String) {
        let Some(state) = &mut self.state else {
            return;
        };
        const MAX_ERRORS: usize = 8;
        if state.watcher_errors.len() == MAX_ERRORS {
            state.watcher_errors.remove(0);
        }
        state.watcher_errors.push(message);
    }

    fn initialized_mut(&mut self) -> Result<&mut InitializedService> {
        self.state
            .as_mut()
            .ok_or_else(|| WorkerError::new("not_initialized", "worker is not initialized"))
    }

    fn initialized_ref(&self) -> Result<&InitializedService> {
        self.state
            .as_ref()
            .ok_or_else(|| WorkerError::new("not_initialized", "worker is not initialized"))
    }
}

fn now_milliseconds() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(value) => i64::try_from(value.as_millis()).unwrap_or(i64::MAX),
        Err(value) => -i64::try_from(value.duration().as_millis()).unwrap_or(i64::MAX),
    }
}
