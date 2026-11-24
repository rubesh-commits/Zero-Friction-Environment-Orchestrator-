# Windows implementation notes

This document explains how the original Linux-oriented architecture was mapped
onto Windows, and where the port intentionally deviates. The engine remains
deterministic: no network call is made during analysis, and no AI/LLM is used
anywhere.

## Architecture map

| Original concept | Windows implementation |
| --- | --- |
| `core` domain engine | Same crate role; Win32 calls isolated in `crates/core/src/winapi.rs` |
| Manifest + tree-sitter analysis | Unchanged; Python/JS/TS/Java grammars compiled in |
| Host probing | `GlobalMemoryStatusEx`, `GetDiskFreeSpaceEx`, `where.exe`, `py` launcher registry, Toolhelp process memory |
| Minimal runtime fetch | python-build-standalone, nodejs.org `SHASUMS256.txt`, Adoptium API checksum |
| Isolated execution | Staged workspace + cleared environment + Job Object + optional Windows Sandbox `.wsb` |
| Socket/process tree | `netstat -ano` + Toolhelp parent map; SERVING/RECEIVING/INTERNAL |
| Desktop GUI | Local `std::net` HTTP dashboard with embedded HTML (no egui/Tauri/Node build) |
| MCP wrapper | Newline-delimited JSON-RPC 2.0 on stdio |
| Fresh-machine containers | Windows Sandbox acceptance suite plus a host acceptance script |

The new `workflow` crate owns the shared pipeline so CLI, GUI and MCP cannot
drift apart. Front ends only translate arguments and render results.

## Paths and filesystem

Everything generated lives under `%LOCALAPPDATA%\WinOrchestrator`:

| Path | Contents |
| --- | --- |
| `cache\workspaces` | Shallow clones keyed by URL hash |
| `cache\downloads` | Verified archives and Adoptium metadata |
| `sandboxes\<id>` | `workspace`, `home`, `temp`, `cache`, `logs`, `runtimes`, `sandbox.json` |
| `toolchains` | Promoted runtimes (`python-3.12.16`, `node-24.22.3`, ...) |
| `snapshots` | Restore files created by GUI/MCP |
| `logs`, `locks` | Diagnostics and cross-process coordination |

Paths are normalized and compared lexically before any recursive delete.
`safe_join` rejects absolute components and `..` traversal from repository
content. A sandbox is only destroyed if it is inside the sandbox base and carries
a compatible `sandbox.json`.

## Process isolation

`crates/core/src/winapi.rs` provides:

- `CreateJobObjectW` with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
- Optional `JOB_OBJECT_LIMIT_PROCESS_MEMORY` cap (minimum 256 MiB).
- Processes are created suspended, assigned to the job, then resumed with
  `NtResumeProcess` so short-lived children cannot escape assignment.
- `TerminateJobObject` plus best-effort descendant termination on teardown.

The sandbox environment is built from scratch. `Command::env_clear()` removes
the user environment, then only these are restored:

- Sandbox `HOME`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `TEMP`, `TMP`.
- Per-tool caches (`PIP_CACHE_DIR`, `npm_config_cache`, `CARGO_HOME`, ...).
- `PYTHONUTF8=1`, `PYTHONNOUSERSITE=1`, UTF-8 locale variables.
- A `PATH` containing the venv, sandbox tools, provisioned runtime directories,
  and Windows system directories. The host `PATH` is never appended.

Interactive CLI runs inherit the console so breakpoints and servers work.
GUI/MCP runs use captured output with a hard timeout.

## Runtime provisioning

### Python

Pinned python-build-standalone release `20261123`. The chosen build is the
lowest embedded version satisfying the analyzer's floor:

| Floor | Build |
| --- | --- |
| <= 3.11 | CPython 3.11.18 |
| <= 3.12 | CPython 3.12.16 |
| <= 3.13 | CPython 3.13.18 |
| <= 3.14 | CPython 3.14.10 |

The archive URL is
`.../releases/download/20261123/cpython-<version>+20261123-x86_64-pc-windows-msvc-install_only.tar.gz`.
The companion `SHA256SUMS` file from the same release is used for verification.

### Node.js

Default LTS `24.22.3`. The floor is raised by `engines.node`, AST rules
(optional chaining, nullish coalescing, global `fetch`, top-level await) and
manifest dependencies. Archives come from
`https://nodejs.org/dist/v<version>/node-v<version>-win-x64.zip` and are verified
against `SHASUMS256.txt` for that exact version.

### Java

Minimum feature release drives the Adoptium v3 API:

```text
https://api.adoptium.net/v3/assets/latest/<feature>/hotspot
    ?architecture=x64&image_type=jdk&os=windows&vendor=eclipse
```

The response's package link and SHA-256 checksum are used directly. A JDK is
fetched because `javac` is required for plain Java sources and Maven/Gradle
fallbacks. `JAVA_HOME` is set inside the sandbox to the provisioned home.

### Host injection vs. fetch

Host probing prefers a compatible existing runtime. `--force-download` ignores
host runtimes and fetches minimal vendor builds. `--offline` forbids downloads
and fails if a compatible runtime is missing. Extracted archives are cached by
filename and re-verified before reuse.

## Keep & Promote

Promotion copies fetched runtimes from the sandbox into
`%LOCALAPPDATA%\WinOrchestrator\toolchains\<runtime>-<version>` and prepends the
required `bin` directories to the **user** `PATH` (`HKCU\Environment`), not the
system `PATH`.

- The previous `PATH` value is backed up under the app root before the first
  change.
- Entries are compared case-insensitively and de-duplicated, so repeated
  promotions are idempotent.
- A promotion failure leaves the sandbox in place and reports the error instead
  of destroying evidence.
- The restore file is written before promotion, so it survives either policy.
  Promoted runtime locations are rewritten in the record before the snapshot is
  written.

## Restore files

`crates/snapshot` writes strict UTF-8 without a BOM, LF line endings, and a
deterministic ordering with explanatory comments. The format is TOML, but it is
written by hand (not serialized) so comments and human-readable decisions stay
stable.

`restore` validates the schema, re-acquires the source (remote URL or local
path), reconstructs the plan from the recorded requirements, provisions to the
recorded floors, restages the workspace, and optionally runs the recorded launch
command. Exact-version mismatches are warnings, not hard failures, because a
fresh machine may only offer a nearby compatible build.

## Networking view

`net-tree` parses `netstat -ano` for TCP and UDP endpoints, builds a parent map
from Toolhelp snapshots, and classifies sockets relative to sandbox PIDs:

- **SERVING** — listening socket owned by a sandbox process.
- **RECEIVING** — established/active flow owned by a sandbox process.
- **INTERNAL** — loopback or inter-sandbox flow.

The GUI can poll this endpoint every two seconds for a live view; the CLI
supports `--watch <seconds>`.

## Windows Sandbox

`run --windows-sandbox` also emits a `.wsb` file describing the staged
workspace. This is an *outer* isolation layer for users who want a disposable
VM in addition to the process-level sandbox. The default execution path does not
require the Windows Sandbox feature.

The fresh-machine suite uses Windows Sandbox differently: it maps only the
compiled binary, fixtures and test scripts read-only, then runs the acceptance
script in a clean OS with no developer runtimes. See
[`tests/fresh_machine`](tests/fresh_machine/README.md).

## Deliberate deviations from the Linux design

1. **Web dashboard instead of egui/Tauri.** It is dependency-light, accessible
   from any local browser, and needs no GUI toolkit or Node build pipeline.
2. **Newline-delimited MCP JSON-RPC.** Simpler to implement and debug with
   stdio; standard MCP clients accept line-delimited messages. No SSE server is
   included.
3. **JDK fetch instead of a bare `javac`.** Adoptium does not publish a
   standalone `javac`-only archive, and Maven/Gradle need a full JDK anyway.
4. **Rust/Go are analyzed but not fetched.** The port's minimal-fetch matrix is
   focused on the three ecosystems in the acceptance suite. Host Rust/Go are
   still detected and injected when compatible.
5. **Process-level sandbox by default.** Windows has no portable `bwrap`; Job
   Objects plus a staged copy and scrubbed environment are the reliable default.
   Windows Sandbox is opt-in and optional.

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| `git.exe was not found` | Install Git for Windows, or pass a local repository path |
| `not enough free disk space` | Free space on the target volume; the estimate includes the staged copy |
| `Keep & Promote failed` | The sandbox is deliberately left in place; inspect the reported error, then `discard` |
| A sandbox is stuck | `winorchestrator doctor` lists stale records; `winorchestrator clean --stop-stale` terminates and removes them |
| Dashboard does not open | Re-run with `--no-open` and visit the printed URL |
| Antivirus blocks extracted tools | Allow `%LOCALAPPDATA%\WinOrchestrator`; every archive is SHA-256 verified first |
| Windows Sandbox unavailable | Enable `Containers-DisposableClientVM` or use the host acceptance script |
