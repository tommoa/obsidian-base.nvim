//! Vault scanning, metadata extraction, and link resolution for the query index.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

use crate::{
    error::{Result, WorkerError},
    limits::Limits,
    markdown::markdown_metadata,
    path::lexical_contained,
    protocol::MetadataOverrideParams,
    value::{DataValue, natural_cmp, parse_date},
    yaml::parse_mapping,
};

pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "md", "base", "canvas", "avif", "bmp", "gif", "jpeg", "jpg", "png", "svg", "webp", "flac",
    "m4a", "mp3", "ogg", "wav", "webm", "3gp", "mkv", "mov", "mp4", "ogv", "pdf",
];
const DIAGNOSTIC_SAMPLE: usize = 8;

#[derive(Clone, Debug)]
/// Indexed metadata and relationship data for one supported vault file.
pub struct FileRecord {
    /// Vault-relative path using forward slashes.
    pub path: String,
    /// Basename without its extension.
    pub name: String,
    /// Filename including its extension.
    pub basename: String,
    /// Vault-relative parent folder, or empty for the vault root.
    pub folder: String,
    /// Lowercase filename extension.
    pub extension: String,
    /// Creation time in milliseconds since the Unix epoch.
    pub ctime: i64,
    /// Modification time in milliseconds since the Unix epoch.
    pub mtime: i64,
    /// Frontmatter properties keyed by their original names.
    pub properties: BTreeMap<String, DataValue>,
    /// Normalized tags from frontmatter and Markdown prose.
    pub tags: Vec<String>,
    /// Wikilink targets declared by this file.
    pub links: Vec<String>,
    /// Indexed files that resolve a wikilink to this file.
    pub backlinks: Vec<String>,
}

#[derive(Clone, Debug)]
/// Unsaved editor contents that temporarily replace a vault file in the index.
pub struct Overlay {
    /// Unsaved text supplied by the editor.
    pub contents: String,
    /// Time assigned to an overlay-only file without filesystem metadata.
    pub created_at: i64,
}

#[derive(Clone, Debug, Default)]
/// Non-fatal conditions collected while building an index.
pub struct Diagnostics {
    /// Number of filesystem entries skipped because their names are not UTF-8.
    pub skipped_non_utf8: usize,
    /// Bounded sample of skipped non-UTF-8 entry locations.
    pub skipped_non_utf8_examples: Vec<String>,
    /// Recent watcher failures retained across index snapshots.
    pub watcher_errors: Vec<String>,
}

#[derive(Clone, Debug)]
/// Immutable snapshot of the vault used to evaluate a query.
pub struct Index {
    /// Canonical vault root used to build this snapshot.
    pub root: PathBuf,
    /// File records keyed by vault-relative path.
    pub records: BTreeMap<String, FileRecord>,
    /// Non-fatal observations made while scanning the vault.
    pub diagnostics: Diagnostics,
}

impl Index {
    pub fn empty(root: PathBuf) -> Self {
        Self {
            root,
            records: BTreeMap::new(),
            diagnostics: Diagnostics::default(),
        }
    }

    pub fn build(
        root: PathBuf,
        overlays: &BTreeMap<String, Overlay>,
        metadata_overrides: &HashMap<String, MetadataOverride>,
        limits: Limits,
        watcher_errors: Vec<String>,
    ) -> Result<Self> {
        let property_types = load_property_types(&root, limits)?;
        let mut builder = Builder {
            root: root.clone(),
            overlays,
            metadata_overrides,
            property_types,
            limits,
            records: BTreeMap::new(),
            diagnostics: Diagnostics {
                watcher_errors,
                ..Diagnostics::default()
            },
        };
        builder.scan(Path::new(""))?;
        // An unsaved buffer may not exist on disk yet, but still needs to participate in queries.
        for path in overlays.keys() {
            if !builder.records.contains_key(path) && supported(path) {
                builder.index(path)?;
            }
        }
        builder.build_backlinks();
        Ok(Self {
            root,
            records: builder.records,
            diagnostics: builder.diagnostics,
        })
    }

    pub fn resolve_link(&self, link: &str, from: &str) -> Option<String> {
        resolve_link(link, from, &self.records)
    }
}

#[derive(Clone, Debug, Default)]
/// Caller-supplied file times used when filesystem metadata is unavailable or unsuitable.
pub struct MetadataOverride {
    /// Optional creation time overriding filesystem metadata.
    pub ctime: Option<i64>,
    /// Optional modification time overriding filesystem metadata.
    pub mtime: Option<i64>,
}

pub fn metadata_overrides(
    values: BTreeMap<String, MetadataOverrideParams>,
) -> Result<HashMap<String, MetadataOverride>> {
    let mut result = HashMap::new();
    for (path, metadata) in values {
        let parse = |value: Option<String>| -> Result<Option<i64>> {
            value
                .map(|value| {
                    parse_date(&value)
                        .ok_or_else(|| WorkerError::new("invalid_params", "invalid metadata date"))
                })
                .transpose()
        };
        result.insert(
            path,
            MetadataOverride {
                ctime: parse(metadata.ctime)?,
                mtime: parse(metadata.mtime)?,
            },
        );
    }
    Ok(result)
}

/// Mutable scanner used to construct one complete index snapshot.
struct Builder<'a> {
    /// Canonical vault root being scanned.
    root: PathBuf,
    /// Unsaved contents applied while reading indexed paths.
    overlays: &'a BTreeMap<String, Overlay>,
    /// Caller-provided filesystem-time overrides by path.
    metadata_overrides: &'a HashMap<String, MetadataOverride>,
    /// Property names declared as dates by Obsidian.
    property_types: HashMap<String, String>,
    /// Resource limits applied to parsed file contents.
    limits: Limits,
    /// Records collected during the current traversal.
    records: BTreeMap<String, FileRecord>,
    /// Diagnostics accumulated without failing the complete scan.
    diagnostics: Diagnostics,
}

impl Builder<'_> {
    fn scan(&mut self, relative: &Path) -> Result<()> {
        let absolute = self.root.join(relative);
        let entries = match fs::read_dir(&absolute) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut entries = entries.collect::<std::result::Result<Vec<_>, _>>()?;
        // Stable traversal gives deterministic link tie-breaking and query results across runs.
        entries.sort_by(|left, right| {
            match (left.file_name().to_str(), right.file_name().to_str()) {
                (Some(left), Some(right)) => natural_cmp(left, right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => left.file_name().cmp(&right.file_name()),
            }
        });
        for entry in entries {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                self.diagnostics.skipped_non_utf8 += 1;
                if self.diagnostics.skipped_non_utf8_examples.len() < DIAGNOSTIC_SAMPLE {
                    self.diagnostics.skipped_non_utf8_examples.push(
                        if relative.as_os_str().is_empty() {
                            "<non-UTF-8 vault entry>".into()
                        } else {
                            format!("{}/<non-UTF-8 entry>", relative.to_string_lossy())
                        },
                    );
                }
                continue;
            };
            if [".git", ".obsidian", ".trash"].contains(&name) {
                continue;
            }
            let child = relative.join(name);
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if file_type.is_dir() {
                self.scan(&child)?;
            } else if file_type.is_file() {
                let path = child
                    .to_str()
                    .expect("UTF-8 parent and entry produce UTF-8 path")
                    .replace('\\', "/");
                if supported(&path) {
                    match self.index(&path) {
                        Err(error) if error.code == "ENOENT" => {}
                        result => result?,
                    }
                }
            }
        }
        Ok(())
    }

    fn index(&mut self, relative: &str) -> Result<()> {
        let absolute = lexical_contained(&self.root, relative)?;
        let overlay = self.overlays.get(relative);
        let metadata = match fs::metadata(&absolute) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && overlay.is_some() => None,
            Err(error) => return Err(error.into()),
        };
        let extension = extension(relative);
        let contents = if extension == "md" {
            match overlay {
                Some(overlay) => self.limits.checked_text(&overlay.contents)?.to_owned(),
                None => self.limits.read_text(&absolute)?,
            }
        } else {
            String::new()
        };
        let (mut properties, body) = if extension == "md" {
            frontmatter(&contents, self.limits)
        } else {
            (BTreeMap::new(), "")
        };
        for (key, value) in &mut properties {
            if self
                .property_types
                .get(key)
                .is_some_and(|kind| kind == "date")
            {
                if let DataValue::String(source) = value {
                    if let Some(date) = parse_date(source) {
                        *value = DataValue::Date(date);
                    }
                }
            }
        }
        let markdown = markdown_metadata(body);
        let mut tags = BTreeSet::new();
        if let Some(value) = properties.get("tags") {
            let values = match value {
                DataValue::List(values) => values.iter().collect::<Vec<_>>(),
                value => vec![value],
            };
            for value in values {
                if let DataValue::String(tag) = value {
                    tags.insert(if tag.starts_with('#') {
                        tag.clone()
                    } else {
                        format!("#{tag}")
                    });
                }
            }
        }
        tags.extend(markdown.tags);
        let fallback = overlay.map_or(0, |overlay| overlay.created_at);
        let override_value = self.metadata_overrides.get(relative);
        let ctime = override_value
            .and_then(|value| value.ctime)
            .or_else(|| {
                metadata
                    .as_ref()
                    .and_then(|value| value.created().ok())
                    .map(system_time)
            })
            .unwrap_or(fallback);
        let mtime = override_value
            .and_then(|value| value.mtime)
            .or_else(|| {
                metadata
                    .as_ref()
                    .and_then(|value| value.modified().ok())
                    .map(system_time)
            })
            .unwrap_or(fallback);
        let basename = relative.rsplit('/').next().unwrap_or(relative).to_owned();
        let name = basename
            .strip_suffix(&format!(".{extension}"))
            .unwrap_or(&basename)
            .to_owned();
        let folder = relative
            .rsplit_once('/')
            .map_or("", |(folder, _)| folder)
            .to_owned();
        self.records.insert(
            relative.to_owned(),
            FileRecord {
                path: relative.to_owned(),
                name,
                basename,
                folder,
                extension,
                ctime,
                mtime,
                properties,
                tags: tags.into_iter().collect(),
                links: markdown.links,
                backlinks: Vec::new(),
            },
        );
        Ok(())
    }

    fn build_backlinks(&mut self) {
        let links = self
            .records
            .values()
            .flat_map(|record| {
                record
                    .links
                    .iter()
                    .map(|link| (record.path.clone(), link.clone()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for (from, link) in links {
            if let Some(target) = resolve_link(&link, &from, &self.records) {
                if let Some(record) = self.records.get_mut(&target) {
                    record.backlinks.push(from.clone());
                }
            }
        }
    }
}

fn frontmatter(contents: &str, limits: Limits) -> (BTreeMap<String, DataValue>, &str) {
    let Some(rest) = contents
        .strip_prefix("---\n")
        .or_else(|| contents.strip_prefix("---\r\n"))
    else {
        return (BTreeMap::new(), contents);
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let marker = line.trim_end_matches(['\r', '\n']);
        if marker == "---" {
            let source = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return (parse_mapping(source, limits).unwrap_or_default(), body);
        }
        offset += line.len();
    }
    (BTreeMap::new(), contents)
}

fn load_property_types(root: &Path, limits: Limits) -> Result<HashMap<String, String>> {
    let path = root.join(".obsidian/types.json");
    let source = match limits.read_text(&path) {
        Ok(source) => source,
        Err(error) if error.code == "ENOENT" => return Ok(HashMap::new()),
        Err(error) => return Err(error),
    };
    let value: Value = serde_json::from_str(&source)?;
    Ok(value
        .get("types")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_owned())))
        .collect())
}

fn resolve_link(link: &str, from: &str, records: &BTreeMap<String, FileRecord>) -> Option<String> {
    let clean = link.replace('\\', "/");
    let clean = clean.strip_suffix(".md").unwrap_or(&clean);
    let folder = from.rsplit_once('/').map_or("", |(folder, _)| folder);
    let relative = normalize_join(folder, clean);
    for candidate in [format!("{relative}.md"), relative] {
        if records.contains_key(&candidate) {
            return Some(candidate);
        }
    }
    // Match Obsidian's basename fallback; natural ordering makes ambiguous links deterministic.
    let basename = clean.rsplit('/').next().unwrap_or(clean);
    records
        .values()
        .filter(|record| record.name == basename)
        .min_by(|left, right| natural_cmp(&left.path, &right.path))
        .map(|record| record.path.clone())
}

fn normalize_join(folder: &str, target: &str) -> String {
    let mut parts = folder
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

fn supported(path: &str) -> bool {
    let extension = extension(path);
    SUPPORTED_EXTENSIONS.contains(&extension.as_str())
}

fn extension(path: &str) -> String {
    path.rsplit_once('.')
        .map_or("", |(_, extension)| extension)
        .to_ascii_lowercase()
}

fn system_time(value: SystemTime) -> i64 {
    match value.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX),
    }
}
