//! Human IPC activation with explicit advanced policy; no persisted settings.
#![cfg_attr(not(unix), allow(dead_code))]
use serde::{Deserialize, Serialize};
use std::num::{NonZeroU64, NonZeroUsize};
#[cfg(unix)]
use tauri::Manager;

/// Every resource limit must be supplied by the human-facing caller.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdvancedPolicy {
    transport: TransportPolicy,
    graph: GraphPolicy,
    coverage: CoveragePolicy,
    scope: super::scope::ScopeLimits,
    dispatch: DispatchPolicy,
    evidence: EvidencePolicy,
    aggregate: AggregatePolicy,
    worker_reserved_bytes: NonZeroUsize,
    export_mode: spec::ExportMode,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransportPolicy {
    max_connections: NonZeroUsize,
    read_frame_bytes: NonZeroUsize,
    write_frame_bytes: NonZeroUsize,
    frame_deadline_ms: NonZeroU64,
    session_lifetime_ms: NonZeroU64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphPolicy {
    max_bytes: NonZeroUsize,
    max_rows: NonZeroUsize,
    max_depth: NonZeroUsize,
    max_values: NonZeroUsize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoveragePolicy {
    max_records: NonZeroUsize,
    max_bytes: NonZeroUsize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchPolicy {
    max_response_bytes: NonZeroUsize,
    max_request_ids: NonZeroUsize,
    trace_max_flows: NonZeroUsize,
    trace_max_hops: NonZeroUsize,
    trace_max_output_bytes: NonZeroUsize,
    compile_max_bytes: NonZeroUsize,
    adr_max_rows: NonZeroUsize,
    adr_max_output_bytes: NonZeroUsize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidencePolicy {
    max_file_validation_bytes: NonZeroU64,
    max_span_bytes: NonZeroU64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AggregatePolicy {
    max_workers: NonZeroUsize,
    max_reserved_bytes: NonZeroUsize,
    max_cumulative_read_bytes: NonZeroU64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandError {
    InvalidPolicy,
    Unavailable,
    #[cfg(not(unix))]
    UnsupportedPlatform,
}

#[cfg(unix)]
impl AdvancedPolicy {
    fn into_policy(self) -> Result<super::service::Policy, CommandError> {
        let policy = self.materialize();
        policy.validate().map_err(|_| CommandError::InvalidPolicy)?;
        Ok(policy)
    }

    // Pure field conversion; every activation path uses into_policy validation.
    fn materialize(self) -> super::service::Policy {
        use core_graph::{SnapshotReadLimits, coverage::CoverageReadLimits};
        use std::time::Duration;
        let t = self.transport;
        let g = self.graph;
        let c = self.coverage;
        let d = self.dispatch;
        let e = self.evidence;
        let a = self.aggregate;
        let deadline = Duration::from_millis(t.frame_deadline_ms.get());
        let lifetime = Duration::from_millis(t.session_lifetime_ms.get());
        super::service::Policy {
            transport: mcp_local::activation::ActivationLimits {
                max_connections: t.max_connections,
                read_frame_bytes: t.read_frame_bytes,
                write_frame_bytes: t.write_frame_bytes,
                frame_deadline: deadline,
                session_lifetime: lifetime,
            },
            worker: super::worker::WorkerLimits {
                max_request_frame_bytes: t.read_frame_bytes,
                graph: SnapshotReadLimits {
                    max_bytes: g.max_bytes.get(),
                    max_rows: g.max_rows.get(),
                    max_depth: g.max_depth.get(),
                    max_values: g.max_values.get(),
                },
                coverage: CoverageReadLimits {
                    max_records: c.max_records.get(),
                    max_bytes: c.max_bytes.get(),
                },
                scope: self.scope,
                dispatch: super::dispatch::DispatchLimits {
                    max_response_bytes: d.max_response_bytes,
                    max_request_ids: d.max_request_ids,
                    trace: flowtracer::TraceLimits {
                        max_flows: d.trace_max_flows.get(),
                        max_hops: d.trace_max_hops.get(),
                        max_output_bytes: d.trace_max_output_bytes.get(),
                    },
                    compile: spec::SelectedCompileLimits {
                        max_bytes: d.compile_max_bytes.get(),
                        adr: spec::AdrLimits {
                            max_rows: d.adr_max_rows.get(),
                            max_output_bytes: d.adr_max_output_bytes.get(),
                        },
                    },
                },
                evidence: super::dispatch::EvidenceWorkLimits {
                    max_file_validation_bytes: e.max_file_validation_bytes,
                    max_span_bytes: e.max_span_bytes,
                },
                reserved_bytes: self.worker_reserved_bytes,
            },
            aggregate: mcp_local::work_budget::WorkLimits {
                max_workers: a.max_workers,
                max_reserved_bytes: a.max_reserved_bytes,
                max_cumulative_read_bytes: a.max_cumulative_read_bytes.get(),
            },
            export_mode: self.export_mode,
        }
    }
}

#[tauri::command]
pub(crate) async fn mcp_reads_status(
    app: tauri::AppHandle,
) -> Result<serde_json::Value, CommandError> {
    #[cfg(unix)]
    {
        serde_json::to_value(
            app.state::<super::service::Service>()
                .status()
                .map_err(|_| CommandError::Unavailable)?,
        )
        .map_err(|_| CommandError::Unavailable)
    }
    #[cfg(not(unix))]
    {
        let _ = app;
        Err(CommandError::UnsupportedPlatform)
    }
}

#[tauri::command]
pub(crate) async fn mcp_reads_enable(
    app: tauri::AppHandle,
    policy: AdvancedPolicy,
) -> Result<(), CommandError> {
    #[cfg(unix)]
    {
        let policy = policy.into_policy()?;
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| CommandError::Unavailable)?;
        tauri::async_runtime::spawn_blocking(move || {
            let parent = runtime_parent(&app)?;
            // Endpoint binding is blocking, but Activation also spawns Tokio I/O.
            let _runtime = runtime.enter();
            app.state::<super::service::Service>()
                .enable(app.clone(), &parent, policy)
                .map(|_| ())
                .map_err(|_| CommandError::Unavailable)
        })
        .await
        .map_err(|_| CommandError::Unavailable)?
    }
    #[cfg(not(unix))]
    {
        let _ = (app, policy);
        Err(CommandError::UnsupportedPlatform)
    }
}

#[tauri::command]
pub(crate) async fn mcp_reads_disable(app: tauri::AppHandle) -> Result<(), CommandError> {
    #[cfg(unix)]
    {
        app.state::<super::service::Service>()
            .disable()
            .await
            .map_err(|_| CommandError::Unavailable)
    }
    #[cfg(not(unix))]
    {
        let _ = app;
        Err(CommandError::UnsupportedPlatform)
    }
}

#[tauri::command]
pub(crate) async fn mcp_reads_client_configuration(
    app: tauri::AppHandle,
) -> Result<serde_json::Value, CommandError> {
    #[cfg(unix)]
    {
        tauri::async_runtime::spawn_blocking(move || {
            let executable = std::env::current_exe().map_err(|_| CommandError::Unavailable)?;
            let relay = executable
                .parent()
                .ok_or(CommandError::Unavailable)?
                .join("cartograph-mcp");
            app.state::<super::service::Service>()
                .client_configuration(&relay)
                .map_err(|_| CommandError::Unavailable)
        })
        .await
        .map_err(|_| CommandError::Unavailable)?
    }
    #[cfg(not(unix))]
    {
        let _ = app;
        Err(CommandError::UnsupportedPlatform)
    }
}

#[cfg(unix)]
fn runtime_parent(app: &tauri::AppHandle) -> Result<std::path::PathBuf, CommandError> {
    use cap_fs_ext::DirExt;
    use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, MetadataExt};
    let data = app
        .path()
        .app_data_dir()
        .map_err(|_| CommandError::Unavailable)?;
    let data = crate::paths::canonicalize(data).map_err(|_| CommandError::Unavailable)?;
    let root = Dir::open_ambient_dir(&data, cap_std::ambient_authority())
        .map_err(|_| CommandError::Unavailable)?;
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    match root.create_dir_with("mcp-runtime", &builder) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(CommandError::Unavailable),
    }
    let child = root
        .open_dir_nofollow("mcp-runtime")
        .map_err(|_| CommandError::Unavailable)?;
    let named = root
        .symlink_metadata("mcp-runtime")
        .map_err(|_| CommandError::Unavailable)?;
    let opened = child
        .dir_metadata()
        .map_err(|_| CommandError::Unavailable)?;
    if !named.is_dir()
        || named.dev() != opened.dev()
        || named.ino() != opened.ino()
        || opened.mode() & 0o7777 != 0o700
    {
        return Err(CommandError::Unavailable);
    }
    // Endpoint::bind additionally checks current-account ownership and brackets
    // the path identity before publishing its fresh private child.
    Ok(data.join("mcp-runtime"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn input() -> serde_json::Value {
        let mut value = serde_json::json!({
            "transport":{"max_connections":1,"read_frame_bytes":1,"write_frame_bytes":1,"frame_deadline_ms":1,"session_lifetime_ms":1},
            "graph":{"max_bytes":1,"max_rows":1,"max_depth":1,"max_values":1},
            "coverage":{"max_records":1,"max_bytes":1},
            "scope":{"max_sources":1,"max_source_registry_bytes":1,"max_event_registry_bytes":1,"max_directory_entries":1,"max_artifact_bytes":1,"max_metadata_bytes":1,"max_corpus_bytes":1},
            "dispatch":{"max_response_bytes":1,"max_request_ids":1,"trace_max_flows":1,"trace_max_hops":1,"trace_max_output_bytes":1,"compile_max_bytes":1,"adr_max_rows":1,"adr_max_output_bytes":1},
            "evidence":{"max_file_validation_bytes":1,"max_span_bytes":1},
            "aggregate":{"max_workers":1,"max_reserved_bytes":1,"max_cumulative_read_bytes":1},
            "worker_reserved_bytes":1,"export_mode":"verified-only"
        });
        // Derive the exact logical floor from these explicit fixture inputs.
        let policy = serde_json::from_value::<AdvancedPolicy>(value.clone())
            .unwrap()
            .materialize();
        let worker =
            super::super::reservation::worker_bound::WorkerReservation::for_limits(policy.worker)
                .unwrap()
                .required_bytes;
        let baseline =
            super::super::reservation::TransportReservation::for_limits(policy.transport)
                .unwrap()
                .activation_bytes;
        value["worker_reserved_bytes"] = serde_json::json!(worker);
        value["aggregate"]["max_reserved_bytes"] =
            serde_json::json!(baseline.checked_add(worker).unwrap());
        value
    }
    #[test]
    fn explicit_policy_rejects_missing_zero_unknown_and_crossfield_limits() {
        // AC-0288: human activation supplies policy; no implicit convenience profile.
        assert!(
            serde_json::from_value::<AdvancedPolicy>(input())
                .unwrap()
                .into_policy()
                .is_ok()
        );
        let mut below_floor = input();
        let floor = below_floor["worker_reserved_bytes"].as_u64().unwrap();
        below_floor["worker_reserved_bytes"] = serde_json::json!(floor - 1);
        assert!(matches!(
            serde_json::from_value::<AdvancedPolicy>(below_floor)
                .unwrap()
                .into_policy(),
            Err(CommandError::InvalidPolicy)
        ));
        let mut missing = input();
        missing["transport"]
            .as_object_mut()
            .unwrap()
            .remove("read_frame_bytes");
        assert!(serde_json::from_value::<AdvancedPolicy>(missing).is_err());
        let mut missing_scope = input();
        missing_scope.as_object_mut().unwrap().remove("scope");
        assert!(serde_json::from_value::<AdvancedPolicy>(missing_scope).is_err());
        for key in [
            "max_sources",
            "max_source_registry_bytes",
            "max_event_registry_bytes",
            "max_directory_entries",
            "max_artifact_bytes",
            "max_metadata_bytes",
            "max_corpus_bytes",
        ] {
            let mut invalid = input();
            invalid["scope"][key] = serde_json::json!(0);
            assert!(
                serde_json::from_value::<AdvancedPolicy>(invalid).is_err(),
                "{key}"
            );
        }
        let mut zero = input();
        zero["graph"]["max_rows"] = serde_json::json!(0);
        assert!(serde_json::from_value::<AdvancedPolicy>(zero).is_err());
        let mut unknown = input();
        unknown["transport"]["path"] = serde_json::json!("/untrusted");
        assert!(serde_json::from_value::<AdvancedPolicy>(unknown).is_err());
        for (section, key) in [
            ("scope", "max_sources"),
            ("graph", "max_bytes"),
            ("coverage", "max_records"),
            ("dispatch", "max_response_bytes"),
            ("evidence", "max_span_bytes"),
            ("aggregate", "max_workers"),
        ] {
            let mut value = input();
            value[section][key] = serde_json::json!(usize::MAX);
            assert!(matches!(
                serde_json::from_value::<AdvancedPolicy>(value)
                    .unwrap()
                    .into_policy(),
                Err(CommandError::InvalidPolicy)
            ));
        }
    }
}
