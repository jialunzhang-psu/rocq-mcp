//! Select the production server and configure disposable fixture wrappers.
use rocq_e2e::ServerConfig;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Build the server configuration for one isolated trace fixture.
pub fn build_config(
    fixture: &str,
    trace: &Path,
    lab: &Path,
    production_server: &Path,
) -> rocq_e2e::Result<ServerConfig> {
    let executable = if matches!(
        fixture,
        "source_change" | "project_timeout" | "toolchain_change" | "fault"
    ) {
        lab.join("bin/rocq-mcp-wrapper")
    } else {
        production_server.to_path_buf()
    };
    let mut config = ServerConfig::new(executable, lab.join("state")).with_working_dir(lab);
    if fixture == "fault" {
        let point = trace
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.split("__").next())
            .ok_or_else(|| rocq_e2e::TraceError::InvalidConfiguration {
                path: trace.to_path_buf(),
                message: "fault trace lacks point".into(),
            })?;
        config = config
            .with_request_timeout(std::time::Duration::from_secs(10))
            .with_env("ROCQ_E2E_REAL_MCP", production_server)
            .with_env("ROCQ_E2E_FAULT_POINT", point)
            .with_env("ROCQ_E2E_FAULT_START_COUNT", lab.join("fault-start.count"));
    } else if fixture == "declare_race" {
        config = config.with_env(
            "ROCQ_ENGINE_DECLARE_RACE_SOCKET",
            lab.join("declare-race.sock"),
        );
    } else if matches!(fixture, "timeout" | "check_timeout") {
        let real_rocq =
            find_on_path("rocq").ok_or_else(|| rocq_e2e::TraceError::InvalidConfiguration {
                path: PathBuf::from("rocq"),
                message: "installed rocq executable was not found".into(),
            })?;
        let path = std::env::join_paths(std::iter::once(lab.join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .map_err(|error| rocq_e2e::TraceError::InvalidConfiguration {
            path: lab.join("bin"),
            message: error.to_string(),
        })?;
        config = config
            .with_env("ROCQ_E2E_REAL_ROCQ", real_rocq)
            .with_env("ROCQ_E2E_TIMEOUT_MARKER", lab.join("timeout.once"))
            .with_env("PATH", path);
    } else if matches!(fixture, "pet_recovery" | "pet_fault") {
        let real_pet =
            find_on_path("pet").ok_or_else(|| rocq_e2e::TraceError::InvalidConfiguration {
                path: PathBuf::from("pet"),
                message: "installed pet executable was not found".into(),
            })?;
        let path = std::env::join_paths(std::iter::once(lab.join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .map_err(|error| rocq_e2e::TraceError::InvalidConfiguration {
            path: lab.join("bin"),
            message: error.to_string(),
        })?;
        // Design note: the disposable PATH wrapper kills only the first PET
        // spawn. Subsequent commands prove recovery through the public MCP
        // connection without exposing a test-only server tool.
        config = config
            .with_env("ROCQ_E2E_REAL_PET", real_pet)
            .with_env(
                if fixture == "pet_fault" {
                    "ROCQ_E2E_PET_SPAWN_COUNT"
                } else {
                    "ROCQ_E2E_PET_FAILURE_MARKER"
                },
                if fixture == "pet_fault" {
                    lab.join("pet-spawn.count")
                } else {
                    lab.join("pet-failed.once")
                },
            )
            .with_env("PATH", path);
    } else if fixture == "pet_eviction" {
        config = config.with_env("ROCQ_MAX_PET_PROCESSES", "1");
    } else if fixture == "pet_timeout" {
        let real_pet =
            find_on_path("pet").ok_or_else(|| rocq_e2e::TraceError::InvalidConfiguration {
                path: PathBuf::from("pet"),
                message: "installed pet executable was not found".into(),
            })?;
        let path = std::env::join_paths(std::iter::once(lab.join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .map_err(|error| rocq_e2e::TraceError::InvalidConfiguration {
            path: lab.join("bin"),
            message: error.to_string(),
        })?;
        let trace_name = trace.to_string_lossy();
        // Parameter and lifecycle cases in this family intentionally share the
        // timeout fixture, but many of them are expected to reject before PET
        // is touched.  Do not make those traces fail during a later server
        // restart merely because the wrapper's default budget is exhausted.
        // Design note: only a trace carrying the public proof-timeout oracle
        // gets the bounded spawn budget; all other traces leave ample room for
        // ordinary startup/reconnect activity.
        let has_timeout_oracle = fs::read(trace)
            .map(|bytes| {
                bytes
                    .windows(b"\"kind\":\"proof_timeout\"".len())
                    .any(|window| window == b"\"kind\":\"proof_timeout\"")
            })
            .unwrap_or(false);
        let allowed_spawns = if !has_timeout_oracle {
            "100"
        } else if trace_name.contains("open_restart/") || trace_name.contains("open_restart_") {
            "3"
        } else if trace_name.contains("/open_") {
            "2"
        } else {
            "0"
        };
        config = config
            .with_env("ROCQ_E2E_REAL_PET", real_pet)
            .with_env("ROCQ_E2E_PET_SPAWN_COUNT", lab.join("pet-spawn.count"))
            .with_env("ROCQ_E2E_PET_ALLOWED_SPAWNS", allowed_spawns)
            .with_env("PATH", path);
    } else if fixture == "axiom_injection" {
        let real_rocq =
            find_on_path("rocq").ok_or_else(|| rocq_e2e::TraceError::InvalidConfiguration {
                path: PathBuf::from("rocq"),
                message: "installed rocq executable was not found".into(),
            })?;
        let path = std::env::join_paths(std::iter::once(lab.join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .map_err(|error| rocq_e2e::TraceError::InvalidConfiguration {
            path: lab.join("bin"),
            message: error.to_string(),
        })?;
        config = config
            .with_env("ROCQ_E2E_REAL_ROCQ", real_rocq)
            .with_env("ROCQ_E2E_AX_INJECT_MARKER", lab.join("axiom-inject.armed"))
            .with_env("PATH", path);
    } else if fixture == "status_runtime" {
        let real_rocq =
            find_on_path("rocq").ok_or_else(|| rocq_e2e::TraceError::InvalidConfiguration {
                path: PathBuf::from("rocq"),
                message: "installed rocq executable was not found".into(),
            })?;
        let real_pet =
            find_on_path("pet").ok_or_else(|| rocq_e2e::TraceError::InvalidConfiguration {
                path: PathBuf::from("pet"),
                message: "installed pet executable was not found".into(),
            })?;
        let path = std::env::join_paths(std::iter::once(lab.join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .map_err(|error| rocq_e2e::TraceError::InvalidConfiguration {
            path: lab.join("bin"),
            message: error.to_string(),
        })?;
        let trace_name = trace.to_string_lossy();
        config = config
            .with_env("ROCQ_E2E_REAL_ROCQ", real_rocq)
            .with_env("ROCQ_E2E_REAL_PET", real_pet)
            .with_env("ROCQ_E2E_TIMEOUT_MARKER", lab.join("timeout.once"))
            .with_env("PATH", path);
        if trace_name.contains("prove_pet_killed") {
            config = config.with_env("ROCQ_E2E_PET_SPAWN_COUNT", lab.join("pet-spawn.count"));
        } else if trace_name.contains("prove_pet_evicted") {
            config = config.with_env("ROCQ_MAX_PET_PROCESSES", "1");
        }
    } else if fixture == "source_change" {
        // Design note: the wrapper mutates only the disposable fixture on the
        // second server_start, keeping the public trace language at five events.
        let change = trace
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.split("__").next())
            .unwrap_or("");
        config = config
            .with_env("ROCQ_E2E_REAL_MCP", production_server)
            .with_env("ROCQ_E2E_CHANGE", change)
            .with_env("ROCQ_E2E_SERVER_COUNT", lab.join("server-start.count"));
        if trace.to_string_lossy().contains("__solved_pending__") {
            let real_rocq =
                find_on_path("rocq").ok_or_else(|| rocq_e2e::TraceError::InvalidConfiguration {
                    path: PathBuf::from("rocq"),
                    message: "installed rocq executable was not found".into(),
                })?;
            let path = std::env::join_paths(std::iter::once(lab.join("bin")).chain(
                std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
            ))
            .map_err(|error| rocq_e2e::TraceError::InvalidConfiguration {
                path: lab.join("bin"),
                message: error.to_string(),
            })?;
            config = config
                .with_env("ROCQ_E2E_REAL_ROCQ", real_rocq)
                .with_env("ROCQ_E2E_ENABLE_BUILD_TIMEOUT", "1")
                .with_env("ROCQ_E2E_TIMEOUT_MARKER", lab.join("timeout.once"))
                .with_env("PATH", path);
            if change == "dune_modules" {
                let real_dune = find_on_path("dune").ok_or_else(|| {
                    rocq_e2e::TraceError::InvalidConfiguration {
                        path: PathBuf::from("dune"),
                        message: "installed dune executable was not found".into(),
                    }
                })?;
                config = config
                    .with_env("ROCQ_E2E_REAL_DUNE", real_dune)
                    .with_env("ROCQ_E2E_DUNE_ARM_MARKER", lab.join("dune-timeout.arm"));
            }
        }
    } else if fixture == "toolchain_change" {
        let real_rocq =
            find_on_path("rocq").ok_or_else(|| rocq_e2e::TraceError::InvalidConfiguration {
                path: PathBuf::from("rocq"),
                message: "installed rocq executable was not found".into(),
            })?;
        let real_pet =
            find_on_path("pet").ok_or_else(|| rocq_e2e::TraceError::InvalidConfiguration {
                path: PathBuf::from("pet"),
                message: "installed pet executable was not found".into(),
            })?;
        let path = std::env::join_paths(std::iter::once(lab.join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .map_err(|error| rocq_e2e::TraceError::InvalidConfiguration {
            path: lab.join("bin"),
            message: error.to_string(),
        })?;
        let change = trace
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.split("__").next())
            .unwrap_or("");
        config = config
            .with_env("ROCQ_E2E_REAL_MCP", production_server)
            .with_env("ROCQ_E2E_REAL_ROCQ", real_rocq)
            .with_env("ROCQ_E2E_REAL_PET", real_pet)
            .with_env("ROCQ_E2E_CHANGE", change)
            .with_env("ROCQ_E2E_SERVER_COUNT", lab.join("server-start.count"))
            .with_env("ROCQ_E2E_TOOLCHAIN_READY", lab.join("toolchain.ready"))
            .with_env("ROCQ_E2E_TOOLCHAIN_CHANGED", lab.join("toolchain.changed"))
            .with_env("PATH", path);
        if trace.to_string_lossy().contains("__solved_pending__") {
            config = config
                .with_env("ROCQ_E2E_ENABLE_BUILD_TIMEOUT", "1")
                .with_env("ROCQ_E2E_TIMEOUT_MARKER", lab.join("timeout.once"));
        }
    } else if fixture == "project_timeout" {
        config = config
            .with_env("ROCQ_E2E_REAL_MCP", production_server)
            .with_env("ROCQ_E2E_SERVER_COUNT", lab.join("server-start.count"))
            .with_env("ROCQ_E2E_LOCK_PID", lab.join("lock.pid"))
            .with_env("ROCQ_E2E_LOCK_READY", lab.join("lock.ready"));
    }
    Ok(config)
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}
