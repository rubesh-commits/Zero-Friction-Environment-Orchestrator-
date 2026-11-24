# WinOrchestrator

A deterministic, Windows-native repository orchestrator. Point it at an
unfamiliar repository; it inspects the code and manifests, injects compatible
host runtimes or fetches only the missing minimal toolchains, runs the project
inside an isolated sandbox, and writes a human-readable UTF-8 restore file that
rebuilds the same environment later.

**No AI models, no cloud analysis, no guessing.** Every decision comes from
manifest parsers, tree-sitter ASTs, an embedded compatibility table, and host
probing.

This directory is a standalone Windows port. It does not modify or depend on the
original project at the repository root.

---

## Highlights

- **Deterministic analysis** of `README`, `requirements.txt`, `pyproject.toml`,
  `package.json`, `pom.xml`, `build.gradle`, `go.mod`, `Cargo.toml`, and source
  files parsed with tree-sitter.
- **Host-first provisioning**: reuse a compatible installed Python, Node.js or
  Java; download only what is missing, and verify every archive with SHA-256.
- **Isolated execution** in a staged workspace with a scrubbed environment, a
  Windows Job Object (`KILL_ON_JOB_CLOSE`, optional memory cap), and an optional
  Windows Sandbox `.wsb` file.
- **UTF-8 restore files** with comments explaining why each runtime was injected
  or fetched. The file is both documentation and executable config.
- **Three front ends** over the same `workflow` crate: CLI, local web dashboard,
  and an MCP stdio server.
- **Fresh-machine tests** that prove the compiled binary works on a clean
  Windows image with no Python, Node.js, Java, or Git installed.

## Requirements

| Task | Requirement |
| --- | --- |
| Build | Windows 10/11 x64, Rust 1.82+ (tested with 1.96), `x86_64-pc-windows-gnu` or MSVC |
| Run | Windows 10/11 x64. No Python, Node.js, Java, Git or Visual Studio required |
| Remote clone | `git.exe` on `PATH` (local paths need no Git) |
| Optional Windows Sandbox | Windows Pro/Enterprise or Windows 11 with the feature enabled |

## Build

```powershell
cd win-orchestrator
cargo build --release
.\target\release\winorchestrator.exe --version
```

Add the release directory to `PATH` if you want to call `winorchestrator` from
anywhere. The workspace is dependency-light and produces one primary executable
(`winorchestrator.exe`, plus an optional tiny `winorchestrator-mcp.exe` shim).

## Quick start

```powershell
# Dry run: languages, version floors, entrypoints, host matches, download size
winorchestrator inspect C:\work\unfamiliar-repo

# Run in a sandbox, then keep the environment (promote fetched runtimes to PATH)
winorchestrator run C:\work\unfamiliar-repo --keep

# Run, write a restore file, and destroy everything afterwards
winorchestrator run C:\work\unfamiliar-repo --discard `
    --snapshot-out .\repo.envspec.utf8

# Non-interactive CI-style run with captured output
winorchestrator run https://github.com/owner/repo.git --yes --timeout 300 --json

# Rebuild exactly what was recorded
winorchestrator restore .\repo.envspec.utf8 --run --timeout 300

# Read-only host diagnostics
winorchestrator doctor

# Sandbox process tree with SERVING / RECEIVING socket classification
winorchestrator net-tree --pid 12345 --watch 2

# Local dashboard
winorchestrator gui

# MCP server over stdio
winorchestrator mcp
```

### Commands

| Command | Purpose |
| --- | --- |
| `inspect <target>` | Dry-run analysis only. Never executes repository code. |
| `run <target>` | Acquire, analyze, provision, stage, run, snapshot, close. |
| `restore <file>` | Rebuild from `.envspec.utf8`; optionally run it. |
| `list` | List known sandboxes and their lifecycle state. |
| `clean` | Purge stale sandboxes, cached clones, downloads and logs. |
| `doctor [path]` | RAM, disk, runtimes, caches, paging advice, stale sandboxes. |
| `net-tree` | Process tree plus TCP/UDP flows for a sandbox or all processes. |
| `gui` | Local web dashboard (`127.0.0.1`, ephemeral port by default). |
| `mcp` | Newline-delimited JSON-RPC 2.0 MCP server on stdio. |

`run` and `restore` ask how to close the environment:

- **Keep & Promote** moves fetched runtimes into
  `%LOCALAPPDATA%\WinOrchestrator\toolchains`, updates the user `PATH`
  idempotently (with a backup), writes the restore file, then removes the
  sandbox.
- **Discard** writes the restore file first, removes the sandbox, and reclaims
  run-owned downloads and cached remote clones.

Use `--keep` / `--discard` to answer up front, or `--yes` for non-interactive
defaults (`--yes` implies discard unless `--keep` is also given).

## Restore file

The restore file is strict UTF-8 without a BOM, deterministic in ordering, and
contains comments. A shortened example:

```toml
# WinOrchestrator restore specification
# Strict UTF-8, no BOM. Human-readable documentation and executable config.
# Rebuild with: winorchestrator restore "C:\work\repo.envspec.utf8"

schema_version = 1
generated_at = "2026-11-24T12:00:00Z"
tool = "WinOrchestrator"
tool_version = "0.1.0"
sandbox_id = "repo-20261124120000-4242"
repo_name = "repo"
source = "C:\work\repo"
source_kind = "local"
target = "C:\work\repo"

[plan]
languages = ["python"]

[[requirements]]
runtime = "python"  # Python
constraint = ">=3.10.0"
minimum = "3.10.0"
reason = "Structural pattern matching (match/case) was added in Python 3.10."
source = "app.py:7"

[[runtimes]]
runtime = "python"
version = "3.12.16"
from_host = false
# Fetched from the official vendor endpoint and SHA-256 verified.
source = "python-build-standalone 20261123"

[launch]
program = "python.exe"
args = ["app.py"]
cwd = "C:\Users\you\AppData\Local\WinOrchestrator\sandboxes\repo-...\workspace"
display = "python.exe app.py"

[environment]
# Scoped environment. Host PATH is intentionally absent.
PATH = "C:\Users\you\AppData\Local\WinOrchestrator\..."
PYTHONUTF8 = "1"
```

`restore` treats the recorded runtime versions as the desired state. If the host
can only satisfy a nearby compatible version, the rebuild proceeds and reports
a warning instead of failing hard.

## Layout and state

All generated state stays under `%LOCALAPPDATA%\WinOrchestrator`:

```text
cache\workspaces\      shallow clones of remote repositories
cache\downloads\       verified runtime archives
sandboxes\<id>\         staged workspace, home, temp, caches, logs, sandbox.json
toolchains\             promoted runtimes from Keep & Promote
snapshots\              GUI/MCP restore files
logs\                   diagnostics
locks\                  cross-process coordination
```

Original repositories are treated as read-only. Execution copies them into
`sandboxes\<id>\workspace` and never launches a process with the original
directory as the working directory.

## Dashboard

```powershell
winorchestrator gui --bind 127.0.0.1:8765 --no-open
```

The dashboard is served with `std::net` and embedded HTML; there is no Node.js
build step and no web framework. It supports inspect, captured run, sandbox
list, Keep/Discard, live socket sampling, host doctor, restore, and teardown.
All endpoints are local JSON.

## MCP integration

`winorchestrator mcp` speaks newline-delimited JSON-RPC 2.0 over stdio. Example
client configuration:

```json
{
  "mcpServers": {
    "winorchestrator": {
      "command": "C:\\path\\to\\winorchestrator.exe",
      "args": ["mcp"]
    }
  }
}
```

Tools: `analyze_repo`, `run_sandbox`, `inspect_net_tree`, `restore_snapshot`,
`teardown_sandbox`. The server itself makes no AI calls; it only exposes the
deterministic engine.

## Safety model

- The original checkout is read-only. Staging copies it into a sandbox.
- The child environment is cleared and rebuilt with sandbox paths plus Windows
  system directories only. The user `PATH` is never inherited.
- Processes are spawned suspended, assigned to a Job Object with
  `KILL_ON_JOB_CLOSE`, optionally capped with `JOB_OBJECT_LIMIT_PROCESS_MEMORY`,
  then resumed. Descendants die with the sandbox.
- Downloads are restricted to official vendor endpoints and verified with
  SHA-256 before extraction.
- Sandbox teardown is lexical and refuses paths outside the sandbox base.
- `Keep & Promote` backs up the user `PATH` before the single idempotent update.

## Testing

```powershell
cargo test --workspace
powershell -NoProfile -ExecutionPolicy Bypass -File .\tests\fresh_machine\run-acceptance.ps1
```

The workspace tests include fixtures for modern Python, Node.js, Java, and an
undocumented repository; CLI JSON determinism; a real isolated Python run with
snapshot/restore; dashboard health; and an MCP handshake/tool call.

For the true clean-OS guarantee:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\tests\fresh_machine\enter-sandbox.ps1
```

See [`WINDOWS.md`](WINDOWS.md) for the Windows-specific design notes and
[`tests/fresh_machine/README.md`](tests/fresh_machine/README.md) for the
acceptance suite.

## Limitations

- Windows x64 is the supported execution target.
- Rust and Go projects are analyzed and reported, but this port's fetch matrix
  focuses on Python, Node.js and Java.
- Remote clones require Git. Without Git, pass a local path.
- Windows Sandbox execution is an optional outer layer; the default sandbox is
  process-level isolation (Job Objects + scoped environment + staged copy).
- Version detection is deliberately conservative: it reports the floor it can
  prove from manifests and syntax, not a guessed "latest".
