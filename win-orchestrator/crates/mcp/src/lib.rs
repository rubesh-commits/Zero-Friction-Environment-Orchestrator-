use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;
use winorchestrator_core::{paths, KeepPolicy};
use winorchestrator_workflow::{PipelineOptions, RestoreOptions};

pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Run the MCP server over stdio. Requests are newline-delimited JSON-RPC 2.0;
/// responses are one line each. Logs go to stderr so clients never see noise.
pub fn run_stdio() -> winorchestrator_core::Result<()> {
    paths::ensure_app_dirs()?;
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                eprintln!("winorchestrator-mcp: stdin error: {err}");
                break;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(err) => {
                write_message(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": {"code": -32700, "message": format!("parse error: {err}")}
                    }),
                )?;
                continue;
            }
        };
        if request.get("id").is_none() {
            // Notification: no response is allowed.
            continue;
        }
        let response = handle_request(&request);
        write_message(&mut stdout, &response)?;
    }
    Ok(())
}

fn write_message(stdout: &mut io::Stdout, value: &Value) -> winorchestrator_core::Result<()> {
    writeln!(stdout, "{}", serde_json::to_string(value)?)?;
    stdout
        .flush()
        .map_err(|source| winorchestrator_core::Error::Io {
            context: " while writing an MCP response".to_string(),
            source,
        })?;
    Ok(())
}

fn handle_request(request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {
                    "name": "winorchestrator",
                    "version": winorchestrator_core::PRODUCT_VERSION
                },
                "instructions": "Deterministic Windows repository analysis, sandboxing and restore. No AI calls are made by this server."
            }
        }),
        "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"tools": tool_definitions()}
        }),
        "resources/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"resources": []}
        }),
        "prompts/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"prompts": []}
        }),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call_tool(name, &arguments) {
                Ok(value) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default()}],
                        "structuredContent": value,
                        "isError": false
                    }
                }),
                Err(err) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": format!("{err}")}],
                        "isError": true
                    }
                }),
            }
        }
        other => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": format!("unknown method: {other}")}
        }),
    }
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "analyze_repo",
            "description": "Deterministically inspect a repository: manifests, AST requirements, entrypoints, host runtime matches and download estimate. No code is executed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": {"type": "string", "description": "Repository URL or local path"}
                },
                "required": ["target"]
            }
        }),
        json!({
            "name": "run_sandbox",
            "description": "Acquire, analyze, provision, run in an isolated Windows sandbox with a Job Object memory cap, then write a UTF-8 restore file and tear down unless keep=true.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": {"type": "string"},
                    "offline": {"type": "boolean"},
                    "setup": {"type": "boolean", "description": "Install declared dependencies (default true)"},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 3600},
                    "keep": {"type": "boolean", "description": "Keep & Promote runtimes instead of destroying"},
                    "snapshot_out": {"type": "string"},
                    "args": {"type": "array", "items": {"type": "string"}},
                    "entrypoint": {"type": "integer", "description": "1-based entrypoint index"},
                    "memory_limit_mb": {"type": "integer"}
                },
                "required": ["target"]
            }
        }),
        json!({
            "name": "inspect_net_tree",
            "description": "Show the sandbox process tree and its TCP/UDP sockets classified as SERVING, RECEIVING or INTERNAL.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pid": {"type": "integer", "description": "Sandbox root process id"},
                    "all": {"type": "boolean", "description": "Include every host process (read-only)"}
                }
            }
        }),
        json!({
            "name": "restore_snapshot",
            "description": "Deterministically rebuild an environment from a .envspec.utf8 file, optionally running it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {"type": "string"},
                    "run": {"type": "boolean"},
                    "timeout_seconds": {"type": "integer"},
                    "offline": {"type": "boolean"}
                },
                "required": ["file"]
            }
        }),
        json!({
            "name": "teardown_sandbox",
            "description": "Destroy one sandbox by id or path. Only sandbox directories are removed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": {"type": "string"},
                    "force": {"type": "boolean"}
                },
                "required": ["target"]
            }
        }),
    ]
}

fn call_tool(name: &str, arguments: &Value) -> winorchestrator_core::Result<Value> {
    match name {
        "analyze_repo" => {
            let target = required_string(arguments, "target")?;
            let inspection = winorchestrator_workflow::inspect(&target)?;
            Ok(serde_json::to_value(&inspection)?)
        }
        "run_sandbox" => run_sandbox(arguments),
        "inspect_net_tree" => {
            let all = arguments
                .get("all")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let pid = arguments
                .get("pid")
                .and_then(Value::as_u64)
                .map(|v| v as u32);
            let report = if all || pid.is_none() {
                winorchestrator_net_tree::collect_all()
            } else {
                winorchestrator_net_tree::collect(&[pid.unwrap()])
            };
            Ok(serde_json::to_value(&report)?)
        }
        "restore_snapshot" => restore_snapshot(arguments),
        "teardown_sandbox" => {
            let target = required_string(arguments, "target")?;
            let force = arguments
                .get("force")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let removed = winorchestrator_workflow::teardown(&target, force)?;
            Ok(json!({"removed": removed}))
        }
        other => Err(winorchestrator_core::Error::Message(format!(
            "unknown tool: {other}"
        ))),
    }
}

fn required_string(arguments: &Value, key: &str) -> winorchestrator_core::Result<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(|value| value.to_string())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            winorchestrator_core::Error::Message(format!("missing required argument `{key}`"))
        })
}

fn optional_path(arguments: &Value, key: &str) -> Option<PathBuf> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(PathBuf::from)
}

fn run_sandbox(arguments: &Value) -> winorchestrator_core::Result<Value> {
    let target = required_string(arguments, "target")?;
    let offline = arguments
        .get("offline")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let setup = arguments
        .get("setup")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let keep = arguments
        .get("keep")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let timeout = arguments
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(120)
        .clamp(1, 3600);
    let extra_args: Vec<String> = arguments
        .get("args")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(|value| value.to_string())
                .collect()
        })
        .unwrap_or_default();
    let entrypoint_index = arguments
        .get("entrypoint")
        .and_then(Value::as_u64)
        .map(|value| (value as usize).saturating_sub(1));
    let memory_limit_mb = arguments.get("memory_limit_mb").and_then(Value::as_u64);
    let snapshot_out = optional_path(arguments, "snapshot_out");
    let options = PipelineOptions {
        target: target.clone(),
        offline,
        setup,
        entrypoint_index,
        extra_args,
        memory_limit_mb,
        snapshot_out: snapshot_out.clone(),
        ..Default::default()
    };
    let mut prepared = winorchestrator_workflow::prepare(&options)?;
    let outcome = prepared.run_captured(Some(Duration::from_secs(timeout)))?;
    let snapshot_path = snapshot_out.unwrap_or_else(|| {
        paths::snapshots_dir().join(format!(
            "{}.envspec.utf8",
            prepared.inspection.acquisition.repo_name
        ))
    });
    let report = winorchestrator_workflow::finalize(
        &mut prepared,
        if keep {
            KeepPolicy::Keep
        } else {
            KeepPolicy::Discard
        },
        &snapshot_path,
        true,
    )?;
    Ok(json!({
        "sandbox": prepared.sandbox.record.id,
        "repo": prepared.inspection.acquisition.repo_name,
        "launch": prepared.sandbox.launch_description(),
        "runtimes": winorchestrator_workflow::runtime_summary(&prepared.sandbox.record.runtimes),
        "exit_code": outcome.exit_code,
        "success": outcome.success,
        "timed_out": outcome.timed_out,
        "duration_seconds": outcome.duration.as_secs_f64(),
        "stdout": outcome.stdout,
        "stderr": outcome.stderr,
        "snapshot_path": report.snapshot_path,
        "promoted": report.promoted,
        "sandbox_removed": report.sandbox_removed,
        "warnings": report.warnings,
        "errors": report.errors,
    }))
}

fn restore_snapshot(arguments: &Value) -> winorchestrator_core::Result<Value> {
    let file = required_string(arguments, "file")?;
    let spec = winorchestrator_snapshot::read_snapshot(&PathBuf::from(&file))?;
    let run = arguments
        .get("run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let offline = arguments
        .get("offline")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let timeout = arguments
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(120)
        .clamp(1, 3600);
    let restored = winorchestrator_workflow::restore(
        &spec,
        &RestoreOptions {
            offline,
            ..Default::default()
        },
    )?;
    let mismatches = restored.version_mismatches.clone();
    let mut prepared = restored.prepared;
    if !run {
        return Ok(json!({
            "sandbox": prepared.sandbox.record.id,
            "root": prepared.sandbox.layout.root,
            "launch": prepared.sandbox.launch_description(),
            "version_mismatches": mismatches,
            "snapshot_path": file,
        }));
    }
    let outcome = prepared.run_captured(Some(Duration::from_secs(timeout)))?;
    let snapshot_path = paths::snapshots_dir().join(format!(
        "{}.restored.envspec.utf8",
        prepared.inspection.acquisition.repo_name
    ));
    let report = winorchestrator_workflow::finalize(
        &mut prepared,
        KeepPolicy::Discard,
        &snapshot_path,
        true,
    )?;
    Ok(json!({
        "sandbox": prepared.sandbox.record.id,
        "exit_code": outcome.exit_code,
        "success": outcome.success,
        "timed_out": outcome.timed_out,
        "stdout": outcome.stdout,
        "stderr": outcome.stderr,
        "version_mismatches": mismatches,
        "snapshot_path": report.snapshot_path,
        "sandbox_removed": report.sandbox_removed,
    }))
}
