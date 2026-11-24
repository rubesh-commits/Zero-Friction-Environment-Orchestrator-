use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;
use winorchestrator_core::{paths, process, Error, KeepPolicy, Result};

#[derive(Clone, Debug)]
pub struct DashboardOptions {
    pub bind: String,
    pub open_browser: bool,
}

impl Default for DashboardOptions {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:0".to_string(),
            open_browser: true,
        }
    }
}

/// Serve the local dashboard until the process is stopped. This is a plain
/// `std::net` server so there is no runtime or framework dependency.
pub fn serve(options: DashboardOptions) -> Result<()> {
    paths::ensure_app_dirs()?;
    let listener = TcpListener::bind(&options.bind).map_err(|source| Error::Io {
        context: format!(" while binding the dashboard to {}", options.bind),
        source,
    })?;
    let address = listener.local_addr().map_err(|source| Error::Io {
        context: " while reading the dashboard address".to_string(),
        source,
    })?;
    let url = format!("http://{address}/");
    println!("WinOrchestrator dashboard: {url}");
    println!("Press Ctrl+C to stop. The server only accepts local HTTP requests.");
    if options.open_browser {
        if let Err(err) = process::open_url(&url) {
            eprintln!("Could not open the browser automatically ({err}); visit {url}");
        }
    }
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(move || {
                    if let Err(err) = handle_connection(stream) {
                        eprintln!("dashboard connection error: {err}");
                    }
                });
            }
            Err(err) => eprintln!("dashboard accept error: {err}"),
        }
    }
    Ok(())
}

struct HttpRequest {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
}

fn read_request(mut stream: &TcpStream) -> Result<HttpRequest> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = stream.read(&mut chunk).map_err(|source| Error::Io {
            context: " while reading an HTTP request".to_string(),
            source,
        })?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = find_header_end(&buffer) {
            let header = String::from_utf8_lossy(&buffer[..position]).to_string();
            let content_length = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0)
                .min(4 * 1024 * 1024);
            while buffer.len() < position + 4 + content_length {
                let read = stream.read(&mut chunk).map_err(|source| Error::Io {
                    context: " while reading an HTTP body".to_string(),
                    source,
                })?;
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
            }
            let request_line = header.lines().next().unwrap_or_default();
            let mut parts = request_line.split_whitespace();
            let method = parts.next().unwrap_or("GET").to_string();
            let target = parts.next().unwrap_or("/").to_string();
            let (path, query) = target
                .split_once('?')
                .map(|(path, query)| (path.to_string(), query.to_string()))
                .unwrap_or((target, String::new()));
            let body = buffer[position + 4..]
                .iter()
                .copied()
                .take(content_length)
                .collect();
            return Ok(HttpRequest {
                method,
                path,
                query,
                body,
            });
        }
        if buffer.len() > 64 * 1024 {
            return Err(Error::Message("HTTP headers are too large".to_string()));
        }
    }
    Err(Error::Message("empty HTTP request".to_string()))
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .or_else(|| buffer.windows(2).position(|window| window == b"\n\n"))
}

fn query_value(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == name {
            return Some(percent_decode(value));
        }
    }
    None
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                    out.push(high * 16 + low);
                    index += 3;
                    continue;
                }
                out.push(bytes[index]);
                index += 1;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn handle_connection(mut stream: TcpStream) -> Result<()> {
    let request = read_request(&stream)?;
    let (status, content_type, body) = route(&request);
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(header.as_bytes())
        .and_then(|_| stream.write_all(&body))
        .map_err(|source| Error::Io {
            context: " while writing an HTTP response".to_string(),
            source,
        })?;
    Ok(())
}

fn route(request: &HttpRequest) -> (u16, &'static str, Vec<u8>) {
    if request.method == "GET" && (request.path == "/" || request.path == "/index.html") {
        return (
            200,
            "text/html; charset=utf-8",
            INDEX_HTML.as_bytes().to_vec(),
        );
    }
    if request.method == "GET" && request.path == "/api/health" {
        return json_response(json!({
            "ok": true,
            "product": winorchestrator_core::PRODUCT_NAME,
            "version": winorchestrator_core::PRODUCT_VERSION,
            "app_root": paths::app_root(),
        }));
    }
    if request.method == "GET" && request.path == "/api/sandboxes" {
        return json_response(
            serde_json::to_value(winorchestrator_sandbox::list_sandboxes()).unwrap_or(Value::Null),
        );
    }
    if request.method == "GET" && request.path == "/api/sandbox" {
        return match query_value(&request.query, "id") {
            Some(id) => {
                let root = paths::sandboxes_dir().join(&id);
                json_result(
                    winorchestrator_sandbox::load_record(&root)
                        .map(|record| serde_json::to_value(record).unwrap_or(Value::Null)),
                )
            }
            None => error_response(400, "missing id"),
        };
    }
    if request.method == "GET" && request.path == "/api/inspect" {
        return match query_value(&request.query, "target") {
            Some(target) => json_result(
                winorchestrator_workflow::inspect(&target)
                    .map(|inspection| serde_json::to_value(inspection).unwrap_or(Value::Null)),
            ),
            None => error_response(400, "missing target"),
        };
    }
    if request.method == "GET" && request.path == "/api/doctor" {
        let target = query_value(&request.query, "target").unwrap_or_else(|| ".".to_string());
        let report = winorchestrator_host_probe::probe_detailed(&PathBuf::from(target));
        return json_response(serde_json::to_value(report).unwrap_or(Value::Null));
    }
    if request.method == "GET" && request.path == "/api/net-tree" {
        let all = query_value(&request.query, "all")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let pid = query_value(&request.query, "pid").and_then(|value| value.parse::<u32>().ok());
        let report = if all || pid.is_none() {
            winorchestrator_net_tree::collect_all()
        } else {
            winorchestrator_net_tree::collect(&[pid.unwrap()])
        };
        return json_response(serde_json::to_value(report).unwrap_or(Value::Null));
    }
    if request.method == "POST" && request.path == "/api/run" {
        return handle_run(&request.body);
    }
    if request.method == "POST" && request.path == "/api/keep" {
        return handle_close(&request.body, KeepPolicy::Keep);
    }
    if request.method == "POST" && request.path == "/api/discard" {
        return handle_close(&request.body, KeepPolicy::Discard);
    }
    if request.method == "POST" && request.path == "/api/teardown" {
        return handle_teardown(&request.body);
    }
    if request.method == "POST" && request.path == "/api/restore" {
        return handle_restore(&request.body);
    }
    if request.path.starts_with("/api/") {
        return error_response(404, "unknown API route");
    }
    error_response(404, "not found")
}

fn json_response(value: Value) -> (u16, &'static str, Vec<u8>) {
    let text = serde_json::to_string(&value)
        .unwrap_or_else(|_| "{\"error\":\"serialisation\"}".to_string());
    (200, "application/json; charset=utf-8", text.into_bytes())
}

fn json_result(result: Result<Value>) -> (u16, &'static str, Vec<u8>) {
    match result {
        Ok(value) => json_response(value),
        Err(err) => error_response(500, &err.to_string()),
    }
}

fn error_response(status: u16, message: &str) -> (u16, &'static str, Vec<u8>) {
    let text = serde_json::to_string(&json!({"error": message})).unwrap_or_default();
    (status, "application/json; charset=utf-8", text.into_bytes())
}

#[derive(Deserialize)]
struct RunRequest {
    target: String,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    keep: Option<bool>,
    #[serde(default)]
    offline: Option<bool>,
    #[serde(default)]
    setup: Option<bool>,
    #[serde(default)]
    args: Option<Vec<String>>,
}

fn handle_run(body: &[u8]) -> (u16, &'static str, Vec<u8>) {
    let request: RunRequest = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(err) => return error_response(400, &format!("invalid JSON body: {err}")),
    };
    if request.target.trim().is_empty() {
        return error_response(400, "target is required");
    }
    let timeout = request.timeout_seconds.unwrap_or(120).clamp(1, 3600);
    let options = winorchestrator_workflow::PipelineOptions {
        target: request.target,
        offline: request.offline.unwrap_or(false),
        setup: request.setup.unwrap_or(true),
        extra_args: request.args.unwrap_or_default(),
        ..Default::default()
    };
    let mut prepared = match winorchestrator_workflow::prepare(&options) {
        Ok(prepared) => prepared,
        Err(err) => return error_response(500, &err.to_string()),
    };
    let outcome = match prepared.run_captured(Some(Duration::from_secs(timeout))) {
        Ok(outcome) => outcome,
        Err(err) => return error_response(500, &err.to_string()),
    };
    let snapshot = paths::snapshots_dir().join(format!(
        "{}.envspec.utf8",
        prepared.inspection.acquisition.repo_name
    ));
    let report = match winorchestrator_workflow::finalize(
        &mut prepared,
        if request.keep.unwrap_or(false) {
            KeepPolicy::Keep
        } else {
            KeepPolicy::Discard
        },
        &snapshot,
        request.keep.unwrap_or(false),
    ) {
        Ok(report) => report,
        Err(err) => return error_response(500, &err.to_string()),
    };
    json_response(json!({
        "sandbox": prepared.sandbox.record.id,
        "launch": prepared.sandbox.launch_description(),
        "runtimes": winorchestrator_workflow::runtime_summary(&prepared.sandbox.record.runtimes),
        "exit_code": outcome.exit_code,
        "success": outcome.success,
        "timed_out": outcome.timed_out,
        "stdout": outcome.stdout,
        "stderr": outcome.stderr,
        "finalize": report,
    }))
}

#[derive(Deserialize)]
struct CloseRequest {
    id: String,
    #[serde(default)]
    update_path: Option<bool>,
}

fn handle_close(body: &[u8], policy: KeepPolicy) -> (u16, &'static str, Vec<u8>) {
    let request: CloseRequest = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(err) => return error_response(400, &format!("invalid JSON body: {err}")),
    };
    let root = paths::sandboxes_dir().join(&request.id);
    let record = match winorchestrator_sandbox::load_record(&root) {
        Ok(record) => record,
        Err(err) => return error_response(404, &err.to_string()),
    };
    let snapshot = paths::snapshots_dir().join(format!("{}.envspec.utf8", record.repo_name));
    json_result(
        winorchestrator_workflow::finalize_existing(
            &root,
            policy,
            &snapshot,
            request.update_path.unwrap_or(false),
        )
        .map(|report| serde_json::to_value(report).unwrap_or(Value::Null)),
    )
}

#[derive(Deserialize)]
struct TeardownRequest {
    target: String,
    #[serde(default)]
    force: Option<bool>,
}

fn handle_teardown(body: &[u8]) -> (u16, &'static str, Vec<u8>) {
    let request: TeardownRequest = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(err) => return error_response(400, &format!("invalid JSON body: {err}")),
    };
    json_result(
        winorchestrator_workflow::teardown(&request.target, request.force.unwrap_or(false))
            .map(|removed| json!({"removed": removed})),
    )
}

#[derive(Deserialize)]
struct RestoreRequest {
    file: String,
    #[serde(default)]
    run: Option<bool>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

fn handle_restore(body: &[u8]) -> (u16, &'static str, Vec<u8>) {
    let request: RestoreRequest = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(err) => return error_response(400, &format!("invalid JSON body: {err}")),
    };
    let spec = match winorchestrator_snapshot::read_snapshot(&PathBuf::from(&request.file)) {
        Ok(spec) => spec,
        Err(err) => return error_response(400, &err.to_string()),
    };
    let restored = match winorchestrator_workflow::restore(
        &spec,
        &winorchestrator_workflow::RestoreOptions::default(),
    ) {
        Ok(restored) => restored,
        Err(err) => return error_response(500, &err.to_string()),
    };
    let mismatches = restored.version_mismatches.clone();
    let mut prepared = restored.prepared;
    if !request.run.unwrap_or(false) {
        return json_response(json!({
            "sandbox": prepared.sandbox.record.id,
            "root": prepared.sandbox.layout.root,
            "launch": prepared.sandbox.launch_description(),
            "version_mismatches": mismatches,
        }));
    }
    let timeout = request.timeout_seconds.unwrap_or(120).clamp(1, 3600);
    let outcome = match prepared.run_captured(Some(Duration::from_secs(timeout))) {
        Ok(outcome) => outcome,
        Err(err) => return error_response(500, &err.to_string()),
    };
    let snapshot = paths::snapshots_dir().join(format!(
        "{}.restored.envspec.utf8",
        prepared.inspection.acquisition.repo_name
    ));
    let report = match winorchestrator_workflow::finalize(
        &mut prepared,
        KeepPolicy::Discard,
        &snapshot,
        false,
    ) {
        Ok(report) => report,
        Err(err) => return error_response(500, &err.to_string()),
    };
    json_response(json!({
        "exit_code": outcome.exit_code,
        "success": outcome.success,
        "stdout": outcome.stdout,
        "stderr": outcome.stderr,
        "version_mismatches": mismatches,
        "finalize": report,
    }))
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>WinOrchestrator</title>
<style>
  :root { color-scheme: dark; --bg:#0f1419; --panel:#171e26; --line:#26313d; --fg:#e6edf3; --muted:#8b9bb0; --accent:#6ee7b7; --warn:#fbbf24; --err:#f87171; }
  * { box-sizing: border-box; }
  body { margin:0; font:14px/1.5 "Segoe UI", system-ui, sans-serif; background:var(--bg); color:var(--fg); }
  header { padding:20px 28px; border-bottom:1px solid var(--line); display:flex; align-items:baseline; gap:16px; }
  header h1 { margin:0; font-size:20px; letter-spacing:.3px; }
  header span { color:var(--muted); }
  main { display:grid; grid-template-columns: minmax(380px, 1fr) minmax(380px, 1fr); gap:18px; padding:18px 28px 48px; }
  section.card { background:var(--panel); border:1px solid var(--line); border-radius:12px; padding:16px; min-width:0; }
  section.card h2 { margin:0 0 12px; font-size:15px; color:var(--accent); font-weight:600; }
  label { display:block; color:var(--muted); font-size:12px; margin:8px 0 4px; text-transform:uppercase; letter-spacing:.08em; }
  input[type=text], input[type=number] { width:100%; padding:8px 10px; background:#0c1117; border:1px solid var(--line); border-radius:8px; color:var(--fg); }
  .row { display:flex; gap:8px; flex-wrap:wrap; align-items:center; margin-top:10px; }
  button { padding:8px 14px; border-radius:8px; border:1px solid var(--line); background:#1f2933; color:var(--fg); cursor:pointer; font-weight:600; }
  button.primary { background:#0e3a2f; border-color:#1c6b52; color:var(--accent); }
  button.danger { background:#3a1517; border-color:#7f2d31; color:var(--err); }
  button:hover { filter:brightness(1.15); }
  pre { background:#0b1016; border:1px solid var(--line); border-radius:8px; padding:12px; overflow:auto; max-height:340px; font:12px/1.45 "Cascadia Mono", Consolas, monospace; white-space:pre-wrap; }
  .treeOut { background:#0b1016; border:1px solid var(--line); border-radius:8px; padding:12px; overflow:auto; max-height:340px; font:12px/1.45 "Cascadia Mono", Consolas, monospace; white-space:normal; }
  .treeRow { white-space:pre-wrap; }
  .role-SERVING { color:var(--accent); font-weight:600; }
  .role-RECEIVING { color:#93c5fd; font-weight:600; }
  .role-INTERNAL { color:var(--muted); font-weight:600; }
  table { width:100%; border-collapse:collapse; font-size:12px; }
  th, td { text-align:left; padding:7px 6px; border-bottom:1px solid var(--line); vertical-align:top; }
  th { color:var(--muted); font-weight:600; }
  .muted { color:var(--muted); }
  .status { margin-top:10px; color:var(--muted); min-height:20px; }
  .full { grid-column:1 / -1; }
  .pill { display:inline-block; padding:1px 8px; border-radius:999px; background:#1f2933; border:1px solid var(--line); font-size:11px; }
  @media (max-width: 900px) { main { grid-template-columns: 1fr; } }
</style>
</head>
<body>
<header><h1>WinOrchestrator</h1><span>deterministic environments &middot; no AI calls &middot; local dashboard</span></header>
<main>
  <section class="card">
    <h2>Repository</h2>
    <label for="target">URL or local path</label>
    <input id="target" type="text" placeholder="C:\work\repo  or  https://github.com/owner/repo.git">
    <div class="row">
      <button class="primary" onclick="inspectRepo()">Inspect</button>
      <button onclick="runRepo()">Run captured</button>
      <label style="margin:0"><input id="keep" type="checkbox"> Keep &amp; Promote</label>
      <label style="margin:0"><input id="setup" type="checkbox" checked> install deps</label>
      <label style="margin:0">timeout <input id="timeout" type="number" value="120" min="1" style="width:70px"></label>
    </div>
    <div class="status" id="repoStatus"></div>
    <pre id="repoOut">Inspect a repository to see languages, AST constraints, entrypoints and host matches.</pre>
  </section>

  <section class="card">
    <h2>Sandboxes</h2>
    <div class="row"><button onclick="loadSandboxes()">Refresh</button><span class="muted" id="sandboxStatus"></span></div>
    <table id="sandboxes"><thead><tr><th>ID</th><th>Repo</th><th>State</th><th>Runtimes</th><th></th></tr></thead><tbody></tbody></table>
  </section>

  <section class="card">
    <h2>Serving / receiving tree</h2>
    <div class="row">
      <label style="margin:0">sandbox pid <input id="pid" type="number" min="1" style="width:110px"></label>
      <label style="margin:0"><input id="allpids" type="checkbox"> all processes</label>
      <label style="margin:0"><input id="netLive" type="checkbox"> live</label>
      <button onclick="loadNetTree()">Refresh</button>
    </div>
    <div class="status" id="netStatus"></div>
    <pre id="netOut" class="treeOut">Enter a sandbox root pid to see process nodes, listening ports (SERVING) and active flows (RECEIVING).</pre>
  </section>

  <section class="card">
    <h2>Host doctor</h2>
    <div class="row"><button onclick="loadDoctor()">Probe host</button></div>
    <div class="status" id="doctorStatus"></div>
    <pre id="doctorOut">Read-only runtime, cache, RAM, disk and stale-sandbox report.</pre>
  </section>

  <section class="card full">
    <h2>Restore file</h2>
    <div class="row">
      <input id="restoreFile" type="text" placeholder="C:\path\to\repo.envspec.utf8" style="flex:1">
      <label style="margin:0"><input id="restoreRun" type="checkbox"> run after restore</label>
      <button onclick="restoreSnapshot()">Restore</button>
    </div>
    <div class="status" id="restoreStatus"></div>
    <pre id="restoreOut">Restore rebuilds the environment deterministically from the strict UTF-8 snapshot.</pre>
  </section>
</main>
<script>
const $ = (id) => document.getElementById(id);
async function api(path, options) {
  const response = await fetch(path, options);
  const text = await response.text();
  let value;
  try { value = JSON.parse(text); } catch { value = text; }
  if (!response.ok) throw new Error(value && value.error ? value.error : text);
  return value;
}
function show(id, value) { $(id).textContent = typeof value === 'string' ? value : JSON.stringify(value, null, 2); }
async function inspectRepo() {
  const target = $('target').value.trim();
  if (!target) { $('repoStatus').textContent = 'Enter a path or URL first.'; return; }
  $('repoStatus').textContent = 'Inspecting (deterministic, no code executed)...';
  try {
    const result = await api('/api/inspect?target=' + encodeURIComponent(target));
    show('repoOut', result);
    $('repoStatus').textContent = 'Inspected. Languages: ' + (result.plan.languages.join(', ') || 'none') + '; estimated downloads: ' + result.estimated_download_bytes + ' bytes.';
  } catch (err) { $('repoStatus').textContent = 'Error: ' + err.message; }
}
async function runRepo() {
  const target = $('target').value.trim();
  if (!target) { $('repoStatus').textContent = 'Enter a path or URL first.'; return; }
  $('repoStatus').textContent = 'Preparing and running in an isolated sandbox... this can take minutes while toolchains download.';
  try {
    const body = JSON.stringify({target, timeout_seconds: Number($('timeout').value) || 120, keep: $('keep').checked, setup: $('setup').checked});
    const result = await api('/api/run', {method:'POST', headers:{'Content-Type':'application/json'}, body});
    show('repoOut', result);
    $('repoStatus').textContent = result.success ? 'Run finished; environment closed.' : 'Run exited with ' + result.exit_code + '.';
    loadSandboxes();
  } catch (err) { $('repoStatus').textContent = 'Error: ' + err.message; }
}
async function loadSandboxes() {
  try {
    const records = await api('/api/sandboxes');
    const body = $('sandboxes').querySelector('tbody');
    body.innerHTML = '';
    for (const record of records) {
      const row = document.createElement('tr');
      row.innerHTML = '<td>' + record.id + '</td><td>' + record.repo_name + '</td><td><span class="pill">' + record.lifecycle + '</span></td><td>' + record.runtimes.length + '</td>';
      const actions = document.createElement('td');
      const keep = document.createElement('button'); keep.textContent = 'Keep'; keep.className = 'primary';
      keep.onclick = () => closeSandbox('/api/keep', record.id);
      const discard = document.createElement('button'); discard.textContent = 'Discard'; discard.className = 'danger'; discard.style.marginLeft = '6px';
      discard.onclick = () => closeSandbox('/api/discard', record.id);
      actions.appendChild(keep); actions.appendChild(discard); row.appendChild(actions);
      body.appendChild(row);
    }
    $('sandboxStatus').textContent = records.length + ' sandbox(es)';
  } catch (err) { $('sandboxStatus').textContent = 'Error: ' + err.message; }
}
async function closeSandbox(path, id) {
  try {
    const result = await api(path, {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({id})});
    show('repoOut', result);
    $('sandboxStatus').textContent = 'Closed ' + id + '.';
    loadSandboxes();
  } catch (err) { $('sandboxStatus').textContent = 'Error: ' + err.message; }
}
function esc(text) {
  return String(text).replace(/[&<>"]/g, (c) => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
}
function renderNetTree(report) {
  if (!report.nodes || report.nodes.length === 0) {
    show('netOut', report);
    return;
  }
  const rows = report.nodes.map((node) => {
    const indent = '&nbsp;'.repeat(Math.min(node.depth, 12) * 3);
    const sockets = node.connections ? ' <span class="pill">' + node.connections + '</span>' : '';
    return '<div class="treeRow">' + indent + '&#9500; ' + esc(node.name) +
      ' <span class="muted">pid ' + node.pid + '</span>' + sockets + '</div>';
  }).join('');
  const flows = report.connections.length === 0 ? '<p class="muted">No sockets in this process tree.</p>' :
    '<table><thead><tr><th>Role</th><th>Process</th><th>Local</th><th>Remote</th><th>State</th></tr></thead><tbody>' +
    report.connections.map((flow) =>
      '<tr><td class="role-' + esc(flow.role.toUpperCase()) + '">' + esc(flow.role.toUpperCase()) + '</td><td>' +
      esc(flow.process_name) + ' <span class="muted">' + flow.pid + '</span></td><td>' +
      esc(flow.local_address + (flow.local_port ? ':' + flow.local_port : '')) + '</td><td>' +
      esc(flow.remote_address + (flow.remote_port ? ':' + flow.remote_port : '')) + '</td><td>' +
      esc(flow.state) + '</td></tr>').join('') + '</tbody></table>';
  $('netOut').innerHTML = rows + flows;
}
async function loadNetTree() {
  if (window.netBusy) { return; }
  window.netBusy = true;
  const pid = Number($('pid').value) || 0;
  const all = $('allpids').checked;
  $('netStatus').textContent = 'Sampling sockets...';
  try {
    const result = await api('/api/net-tree?all=' + (all ? '1' : '0') + (pid ? '&pid=' + pid : ''));
    renderNetTree(result);
    $('netStatus').textContent = result.nodes.length + ' process node(s), ' + result.connections.length + ' socket(s).' + ($('netLive').checked ? ' Live sampling every 2s.' : '');
  } catch (err) { $('netStatus').textContent = 'Error: ' + err.message; }
  finally { window.netBusy = false; }
}
let netTimer = null;
function toggleNetLive() {
  if (netTimer) { clearInterval(netTimer); netTimer = null; }
  if ($('netLive').checked) {
    loadNetTree();
    netTimer = setInterval(loadNetTree, 2000);
  } else {
    $('netStatus').textContent = 'Live sampling stopped.';
  }
}
async function loadDoctor() {
  $('doctorStatus').textContent = 'Probing host (read-only)...';
  try { show('doctorOut', await api('/api/doctor')); $('doctorStatus').textContent = 'Probe complete.'; }
  catch (err) { $('doctorStatus').textContent = 'Error: ' + err.message; }
}
async function restoreSnapshot() {
  const file = $('restoreFile').value.trim();
  if (!file) { $('restoreStatus').textContent = 'Enter a restore file path.'; return; }
  $('restoreStatus').textContent = 'Restoring...';
  try {
    const result = await api('/api/restore', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({file, run: $('restoreRun').checked, timeout_seconds: Number($('timeout').value) || 120})});
    show('restoreOut', result);
    $('restoreStatus').textContent = 'Restore complete.';
    loadSandboxes();
  } catch (err) { $('restoreStatus').textContent = 'Error: ' + err.message; }
}
loadSandboxes();
$('netLive').addEventListener('change', toggleNetLive);
</script>
</body>
</html>
"#;
