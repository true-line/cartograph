//! Explicit preadmission for registration and filesystem-derived coverage basis.
//! Limits model bounded input work; they are not a total allocation/RSS model.
use super::ReadError;
use adapters_plugin_host::discovery::{DiscoveryError, DiscoveryLimits, discover_bounded};
use context_hub::coverage::ScopedRepo;
use serde::Deserialize;
use std::{
    io::{self, Read},
    num::{NonZeroU64, NonZeroUsize},
    path::Path,
};
use tauri::Manager;

pub(crate) const READ_BUFFER_BYTES: usize = 8192;

/// No implicit numeric profile. Source rows/bytes and event-file bytes are
/// aggregate per scope observation. Filesystem entry/metadata caps apply to each
/// scan (event directory, or user+project plugin discovery for one repo).
/// Artifact/corpus caps apply independently to each repo, including repeated
/// user-level discovery; max_sources bounds the number of repo scans.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScopeLimits {
    pub max_sources: NonZeroUsize,
    pub max_source_registry_bytes: NonZeroUsize,
    pub max_event_registry_bytes: NonZeroUsize,
    pub max_directory_entries: NonZeroUsize,
    pub max_artifact_bytes: NonZeroU64,
    pub max_metadata_bytes: NonZeroUsize,
    pub max_corpus_bytes: NonZeroU64,
}

pub(crate) fn coverage_scope<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &crate::AppState,
    limits: ScopeLimits,
) -> Result<Vec<ScopedRepo>, ReadError> {
    let data = app.path().app_data_dir().map_err(|_| ReadError::Storage)?;
    coverage_scope_in(
        state,
        &data.join("adapters"),
        &data.join("registries"),
        limits,
    )
}

fn coverage_scope_in(
    state: &crate::AppState,
    user_dir: &Path,
    event_dir: &Path,
    limits: ScopeLimits,
) -> Result<Vec<ScopedRepo>, ReadError> {
    use crate::sources::{SourceListError, SourceListLimits};
    let sources = state
        .sources
        .lock()
        .map_err(|_| ReadError::Storage)?
        .list_bounded(SourceListLimits {
            max_rows: limits.max_sources.get(),
            max_bytes: limits.max_source_registry_bytes.get(),
        })
        .map_err(|error| match error {
            SourceListError::RowLimit | SourceListError::ByteLimit => ReadError::Limit,
            _ => ReadError::Storage,
        })?;
    let registries = event_registries(event_dir, limits)?;
    sources
        .iter()
        .map(|source| {
            let plugins = active_plugins(state, source.root(), user_dir, limits)?;
            Ok(ScopedRepo {
                repo_key: source.repo_key.clone(),
                registered_source_id: Some(source.source_id.clone()),
                config_hash: crate::coverage_config_hash(&plugins, &registries),
                layer_hints: None,
            })
        })
        .collect()
}

fn discovery_error(error: DiscoveryError) -> ReadError {
    match error {
        DiscoveryError::EntryLimit
        | DiscoveryError::FileByteLimit
        | DiscoveryError::MetadataLimit => ReadError::Limit,
        _ => ReadError::Storage,
    }
}

fn active_plugins(
    state: &crate::AppState,
    root: &Path,
    user_dir: &Path,
    limits: ScopeLimits,
) -> Result<Vec<crate::ActivePlugin>, ReadError> {
    if root.as_os_str().as_encoded_bytes().len() >= limits.max_metadata_bytes.get() {
        return Err(ReadError::Limit);
    }
    let discovered = discover_bounded(
        &[root.to_path_buf()],
        user_dir,
        DiscoveryLimits {
            max_entries: limits.max_directory_entries.get(),
            max_file_bytes: limits.max_artifact_bytes.get(),
            max_metadata_bytes: limits.max_metadata_bytes.get(),
        },
    )
    .map_err(discovery_error)?;
    let settings = state.settings.lock().map_err(|_| ReadError::Storage)?;
    // The same sandbox bounds as recovery (AC-1803), so the coverage basis
    // this scope compares against matches the one recovery recorded.
    let sandbox = crate::plugin_limits(&settings).map_err(|_| ReadError::Storage)?;
    let mut active = Vec::new();
    let mut corpus_remaining = limits.max_corpus_bytes.get();
    let mut metadata_remaining = limits.max_metadata_bytes.get();
    for plugin in discovered {
        let settings_root = crate::plugin_settings_root(&plugin);
        if !settings
            .plugin_enabled_exact(&settings_root, &plugin.id, &plugin.content_hash)
            .map_err(|_| ReadError::Storage)?
        {
            continue;
        }
        // Admit the derived corpus path before allocating it. Reading/parsing
        // never starts until the remaining aggregate byte cap is established.
        charge(
            &mut metadata_remaining,
            plugin
                .path
                .as_os_str()
                .as_encoded_bytes()
                .len()
                .checked_add("golden.json".len())
                .ok_or(ReadError::Limit)?,
        )?;
        let path = plugin.path.with_extension("golden.json");
        let bytes = read_file(&path, &mut corpus_remaining, None)?;
        let corpus = crate::PluginCorpus::from_bytes(&plugin, bytes);
        if !settings
            .plugin_gate_passed_exact(&plugin.id, &corpus.gate_key)
            .map_err(|_| ReadError::Storage)?
        {
            continue;
        }
        let cells = corpus.cells();
        let gate_key = corpus.gate_key;
        let Some((extensions, languages)) = corpus
            .corpus
            .map(|corpus| (corpus.extensions, corpus.languages))
            .filter(|(extensions, _)| !extensions.is_empty())
        else {
            continue;
        };
        active.push(crate::ActivePlugin {
            plugin_id: plugin.id,
            path: plugin.path,
            content_hash: plugin.content_hash,
            extensions,
            cells,
            languages,
            gate_key,
            limits: sandbox,
        });
    }
    Ok(active)
}

fn charge(remaining: &mut usize, bytes: usize) -> Result<(), ReadError> {
    *remaining = bytes
        .checked_add(1)
        .and_then(|bytes| remaining.checked_sub(bytes))
        .ok_or(ReadError::Limit)?;
    Ok(())
}

/// Optional semantic ceiling keeps official registry oversized-file rejection
/// identical without reading the complete oversized file. Host aggregate
/// admission always fails the entire observation, rather than omitting a file.
fn read_file(
    path: &Path,
    remaining: &mut u64,
    semantic_max: Option<usize>,
) -> Result<Option<Vec<u8>>, ReadError> {
    let entry = match std::fs::symlink_metadata(path) {
        Ok(entry) => entry,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ReadError::Storage),
    };
    if !entry.file_type().is_file() {
        return Err(ReadError::Storage);
    }
    let mut file = std::fs::File::open(path).map_err(|_| ReadError::Storage)?;
    if !file.metadata().map_err(|_| ReadError::Storage)?.is_file() {
        return Err(ReadError::Storage);
    }
    let semantic_bytes = semantic_max
        .and_then(|max| max.checked_add(1))
        .unwrap_or(usize::MAX);
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; READ_BUFFER_BYTES];
    while bytes.len() < semantic_bytes {
        let take = buffer
            .len()
            .min(semantic_bytes - bytes.len())
            .min(usize::try_from(remaining.saturating_add(1)).unwrap_or(usize::MAX));
        let count = file
            .read(&mut buffer[..take])
            .map_err(|_| ReadError::Storage)?;
        if count == 0 {
            break;
        }
        *remaining = remaining
            .checked_sub(count as u64)
            .ok_or(ReadError::Limit)?;
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(Some(bytes))
}

fn event_registries(dir: &Path, limits: ScopeLimits) -> Result<crate::Registries, ReadError> {
    use adapters_fw::events::{MAX_EXTENSION_FILES, MAX_REGISTRY_BYTES};
    let mut entries_remaining = limits.max_directory_entries.get();
    let mut metadata_remaining = limits.max_metadata_bytes.get();
    charge(
        &mut metadata_remaining,
        dir.as_os_str().as_encoded_bytes().len(),
    )?;
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(crate::Registries::builtin().clone());
        }
        Err(_) => return Err(ReadError::Storage),
    };
    let mut names = Vec::new();
    for entry in entries {
        entries_remaining = entries_remaining.checked_sub(1).ok_or(ReadError::Limit)?;
        let entry = entry.map_err(|_| ReadError::Storage)?;
        let name = entry.file_name();
        charge(&mut metadata_remaining, name.as_encoded_bytes().len())?;
        let name = name.into_string().map_err(|_| ReadError::Storage)?;
        if !name.ends_with(".json") {
            continue;
        }
        charge(
            &mut metadata_remaining,
            dir.as_os_str()
                .as_encoded_bytes()
                .len()
                .checked_add(name.len())
                .and_then(|n| n.checked_add(1))
                .ok_or(ReadError::Limit)?,
        )?;
        charge(
            &mut metadata_remaining,
            name.len()
                .checked_add("extension:".len())
                .ok_or(ReadError::Limit)?,
        )?;
        if !entry.file_type().map_err(|_| ReadError::Storage)?.is_file() {
            return Err(ReadError::Storage);
        }
        names.push((name, entry.path()));
    }
    names.sort();
    let mut files = Vec::new();
    let mut remaining = limits.max_event_registry_bytes.get() as u64;
    for (index, (name, path)) in names.into_iter().enumerate() {
        let bytes = if index >= MAX_EXTENSION_FILES {
            Vec::new()
        } else {
            read_file(&path, &mut remaining, Some(MAX_REGISTRY_BYTES))?.ok_or(ReadError::Storage)?
        };
        files.push((name, bytes));
    }
    Ok(crate::Registries::layered(&files))
}

#[cfg(test)]
#[path = "scope_tests.rs"]
pub(crate) mod tests;
