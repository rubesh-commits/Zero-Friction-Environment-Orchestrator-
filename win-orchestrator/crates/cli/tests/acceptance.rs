//! End-to-end acceptance tests for the deterministic Windows orchestrator.
//!
//! These tests never call an AI service. Repository fixtures under
//! `tests/fixtures` are analysed on disk, and the CLI is exercised through the
//! binary Cargo builds for this package.

use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use winorchestrator_core::{KeepPolicy, RuntimeKind};
use winorchestrator_workflow::PipelineOptions;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_winorchestrator"))
}

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "winorchestrator-test-{}-{}-{}",
            std::process::id(),
            label,
            TEMP_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&path).expect("create temp root");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn minimum_for(
    inspection: &winorchestrator_workflow::Inspection,
    runtime: RuntimeKind,
) -> winorchestrator_core::Version {
    inspection
        .plan
        .requirement_for(runtime)
        .and_then(|requirement| requirement.minimum)
        .unwrap_or_else(|| panic!("no {runtime:?} requirement was derived"))
}

fn version(text: &str) -> winorchestrator_core::Version {
    winorchestrator_core::Version::parse(text).expect("valid test version")
}

#[test]
fn python_fixture_requires_modern_python_without_ai() {
    let inspection =
        winorchestrator_workflow::inspect(&fixture("python-service").display().to_string())
            .expect("inspect python fixture");

    assert!(inspection
        .plan
        .has_language(winorchestrator_core::Language::Python));
    assert!(
        inspection
            .plan
            .requirements
            .iter()
            .any(|requirement| requirement.runtime == RuntimeKind::Python),
        "Python requirement missing"
    );
    let minimum = minimum_for(&inspection, RuntimeKind::Python);
    assert!(
        minimum >= version("3.10.0"),
        "match/case and walrus should raise the Python floor, got {minimum}"
    );
    assert!(
        inspection
            .plan
            .dependencies
            .iter()
            .any(|dependency| dependency.name.eq_ignore_ascii_case("httpx")),
        "requirements.txt dependency was not parsed"
    );
    assert!(
        inspection
            .plan
            .entrypoints
            .iter()
            .any(|entrypoint| entrypoint.path.as_deref() == Some("app.py")),
        "app.py was not discovered as an entrypoint"
    );
    assert!(
        inspection
            .plan
            .notes
            .iter()
            .chain(inspection.notes.iter())
            .any(|note| note.contains("No AI") || note.contains("no AI")),
        "analysis should state that no AI is used"
    );
}

#[test]
fn node_fixture_detects_modern_javascript_and_scripts() {
    let inspection =
        winorchestrator_workflow::inspect(&fixture("node-service").display().to_string())
            .expect("inspect node fixture");

    assert!(inspection
        .plan
        .has_language(winorchestrator_core::Language::JavaScript));
    let minimum = minimum_for(&inspection, RuntimeKind::Node);
    assert!(
        minimum >= version("18.0.0"),
        "global fetch requires Node 18+, got {minimum}"
    );
    assert!(
        inspection
            .plan
            .dependencies
            .iter()
            .any(|dependency| dependency.name == "chalk"),
        "package.json dependency was not parsed"
    );
    let kinds: Vec<_> = inspection
        .plan
        .entrypoints
        .iter()
        .map(|entrypoint| entrypoint.kind)
        .collect();
    assert!(
        kinds.contains(&winorchestrator_core::EntrypointKind::NpmScript),
        "npm script entrypoint missing: {kinds:?}"
    );
    assert!(
        kinds.contains(&winorchestrator_core::EntrypointKind::NodeScript),
        "Node script entrypoint missing: {kinds:?}"
    );
}

#[test]
fn java_fixture_detects_records_and_main_class() {
    let inspection =
        winorchestrator_workflow::inspect(&fixture("java-service").display().to_string())
            .expect("inspect java fixture");

    assert!(inspection
        .plan
        .has_language(winorchestrator_core::Language::Java));
    let minimum = minimum_for(&inspection, RuntimeKind::Java);
    assert!(
        minimum >= version("16.0.0"),
        "records require Java 16+, got {minimum}"
    );
    assert!(
        inspection.plan.entrypoints.iter().any(|entrypoint| {
            entrypoint.kind == winorchestrator_core::EntrypointKind::JavaMain
                && entrypoint.path.as_deref() == Some("com.example.demo.Main")
        }),
        "fully qualified Java main was not detected: {:?}",
        inspection.plan.entrypoints
    );
}

#[test]
fn undocumented_repository_is_still_analyzed() {
    let inspection =
        winorchestrator_workflow::inspect(&fixture("undocumented-python").display().to_string())
            .expect("inspect undocumented fixture");

    assert!(inspection.plan.manifests.is_empty());
    assert!(
        inspection
            .warnings
            .iter()
            .any(|warning| warning.contains("No known manifest")),
        "missing-manifest warning absent: {:?}",
        inspection.warnings
    );
    assert!(
        inspection.plan.entrypoints.iter().any(|entrypoint| {
            entrypoint.kind == winorchestrator_core::EntrypointKind::PythonScript
                && entrypoint.path.as_deref() == Some("tool.py")
        }),
        "main guard entrypoint was not discovered"
    );
    assert!(minimum_for(&inspection, RuntimeKind::Python) >= version("3.8.0"));
}

#[test]
fn cli_inspect_json_is_deterministic() {
    let target = fixture("python-service").display().to_string();
    let run = || {
        let output = Command::new(binary())
            .args(["inspect", &target, "--json"])
            .output()
            .expect("spawn inspect");
        assert!(
            output.status.success(),
            "inspect failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).expect("inspect JSON")
    };

    let first = run();
    let second = run();
    assert_eq!(
        first["plan"], second["plan"],
        "analysis must be deterministic"
    );
    assert_eq!(first["plan"]["tool"], "WinOrchestrator");
    assert_eq!(first["acquisition"]["source_kind"], "local");
    assert!(
        first["plan"]["notes"]
            .as_array()
            .expect("notes array")
            .iter()
            .any(|note| note.as_str().unwrap_or_default().contains("No AI/LLM")),
        "deterministic guarantee missing from notes"
    );
}

#[test]
fn cli_doctor_and_list_produce_json() {
    let output = Command::new(binary())
        .args(["doctor", "--json"])
        .output()
        .expect("spawn doctor");
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doctor: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert!(doctor.get("probe").is_some() || doctor.get("memory_total_bytes").is_some());

    let output = Command::new(binary())
        .args(["list", "--json"])
        .output()
        .expect("spawn list");
    assert!(output.status.success());
    let list: Value = serde_json::from_slice(&output.stdout).expect("list JSON");
    assert!(list.is_array(), "list --json should return an array");
}

#[test]
fn cli_rejects_a_missing_target() {
    let missing = std::env::temp_dir().join("winorchestrator-does-not-exist");
    let output = Command::new(binary())
        .args(["inspect", &missing.display().to_string()])
        .output()
        .expect("spawn inspect");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("local path does not exist"),
        "unexpected error: {stderr}"
    );
}

#[test]
fn python_fixture_runs_in_an_isolated_sandbox_and_restores() {
    let target = fixture("python-service").display().to_string();
    let inspection = winorchestrator_workflow::inspect(&target).expect("inspect fixture");
    let host_python = inspection
        .matches
        .iter()
        .any(|entry| entry.runtime == RuntimeKind::Python && entry.is_compatible());
    if !host_python {
        eprintln!("skipping sandbox execution: no compatible host Python (fresh machine mode)");
        return;
    }

    let temp = TempRoot::new("sandbox");
    let snapshot = temp.path().join("python-service.envspec.utf8");
    let options = PipelineOptions {
        target,
        id: Some("python-e2e".to_string()),
        base_dir: Some(temp.path().to_path_buf()),
        offline: true,
        force_download: false,
        setup: false,
        write_windows_sandbox: false,
        entrypoint_index: None,
        extra_args: Vec::new(),
        snapshot_out: Some(snapshot.clone()),
        memory_limit_mb: Some(512),
        update_path: false,
    };

    let mut prepared = winorchestrator_workflow::prepare(&options).expect("prepare sandbox");
    assert!(
        prepared.sandbox.layout.workspace.join("app.py").is_file(),
        "the fixture should be staged in a separate workspace"
    );
    assert_ne!(
        prepared.sandbox.layout.workspace,
        fixture("python-service"),
        "the original repository must never be the execution directory"
    );

    let outcome = prepared
        .run_captured(Some(Duration::from_secs(60)))
        .expect("run sandbox");
    assert!(outcome.success, "sandbox run failed: {outcome:?}");
    assert!(
        outcome
            .stdout
            .contains("python-service says hello to acceptance: ready"),
        "unexpected stdout: {}",
        outcome.stdout
    );

    let mut prepared = prepared;
    let report =
        winorchestrator_workflow::finalize(&mut prepared, KeepPolicy::Discard, &snapshot, false)
            .expect("finalize sandbox");
    assert!(
        report.errors.is_empty(),
        "finalize errors: {:?}",
        report.errors
    );
    assert!(report.sandbox_removed, "discard must remove the sandbox");
    assert!(snapshot.is_file(), "restore file must survive teardown");

    let spec = winorchestrator_snapshot::read_snapshot(&snapshot).expect("read snapshot");
    assert_eq!(spec.repo_name, "python-service");
    assert_eq!(spec.source_kind, winorchestrator_core::SourceKind::Local);
    assert!(
        spec.requirements
            .iter()
            .any(|requirement| requirement.runtime == RuntimeKind::Python),
        "restore file lost the Python requirement"
    );
    let restored = spec.to_plan();
    assert!(restored.has_language(winorchestrator_core::Language::Python));
}

#[test]
fn gui_serves_a_local_health_endpoint() {
    let reserved = TcpListener::bind("127.0.0.1:0").expect("reserve port");
    let address = reserved.local_addr().expect("address");
    drop(reserved);

    let child = Command::new(binary())
        .args(["gui", "--bind", &address.to_string(), "--no-open"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn gui");
    let _guard = ChildGuard(child);

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_error = String::new();
    while Instant::now() < deadline {
        match TcpStream::connect(address) {
            Ok(mut stream) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let request = format!(
                    "GET /api/health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
                );
                if stream.write_all(request.as_bytes()).is_err() {
                    last_error = "write failed".to_string();
                } else {
                    let mut response = String::new();
                    if stream.read_to_string(&mut response).is_ok() {
                        if let Some(body) = response.split("\r\n\r\n").nth(1) {
                            let value: Value = serde_json::from_str(body).expect("health JSON");
                            assert_eq!(value["ok"], true);
                            assert_eq!(value["product"], "WinOrchestrator");
                            return;
                        }
                        last_error = format!("no body in response: {response}");
                    } else {
                        last_error = "read failed".to_string();
                    }
                }
            }
            Err(err) => last_error = err.to_string(),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("dashboard did not answer /api/health: {last_error}");
}

#[test]
fn mcp_stdio_lists_tools_and_analyzes_a_repository() {
    let mut child = Command::new(binary())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp");
    let stdout = child.stdout.take().expect("stdout");
    let mut stdin = child.stdin.take().expect("stdin");
    let (sender, receiver) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let _guard = ChildGuard(child);

    let mut send = |value: Value| {
        writeln!(stdin, "{value}").expect("write MCP request");
        stdin.flush().expect("flush MCP request");
    };
    let receive = |id: i64| -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = receiver
                .recv_timeout(remaining.max(Duration::from_millis(50)))
                .unwrap_or_else(|err| panic!("no MCP response for {id}: {err}"));
            let value: Value = serde_json::from_str(&line).expect("MCP JSON");
            if value["id"] == id {
                return value;
            }
        }
    };

    send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"protocolVersion": "2024-11-05", "capabilities": {}}
    }));
    let initialize = receive(1);
    assert_eq!(
        initialize["result"]["serverInfo"]["name"],
        "winorchestrator"
    );

    send(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let tools = receive(2);
    let names: Vec<_> = tools["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for expected in [
        "analyze_repo",
        "run_sandbox",
        "inspect_net_tree",
        "restore_snapshot",
        "teardown_sandbox",
    ] {
        assert!(
            names.contains(&expected),
            "missing MCP tool {expected}: {names:?}"
        );
    }

    send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "analyze_repo",
            "arguments": {"target": fixture("python-service").display().to_string()}
        }
    }));
    let analyzed = receive(3);
    assert_eq!(analyzed["result"]["isError"], false);
    assert_eq!(
        analyzed["result"]["structuredContent"]["plan"]["repo_name"],
        "python-service"
    );
}
