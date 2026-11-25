# Sand

**Sand** is a deterministic repository orchestrator for Windows and Linux. Point it at an unfamiliar repository and it will inspect the code and manifests, reuse compatible runtimes already on the host, fetch only missing toolchains, and run the project in an isolated workspace. Sand can also write a human-readable UTF-8 restore file to recreate the environment later.

Sand uses no AI models or cloud analysis. Its decisions come from manifest parsers, tree-sitter AST scans, a compatibility table, and host checks.

## Highlights

- Analyzes `README`, `requirements.txt`, `pyproject.toml`, `package.json`, `pom.xml`, `build.gradle`, `go.mod`, `Cargo.toml`, and source files parsed with tree-sitter.
- Reuses compatible Python, Node.js, or Java runtimes from the host before downloading anything. Verifies downloaded archives with SHA-256.
- Stages repository files read-only, then runs commands against a sandbox copy with a controlled environment.
- Uses Windows Job Objects or Linux cgroups and namespaces where available to manage sandbox processes and resource limits.
- Supports optional stronger isolation with Windows Sandbox on supported Windows editions, or rootless Podman, Docker, or bubblewrap on Linux.
- Creates UTF-8 restore files that document which runtimes were reused or fetched.
- Provides a CLI, local web dashboard, and MCP stdio server using the same deterministic workflow.

## Requirements

| Task | Requirement |
|---|---|
| Build | Windows 10/11 or Linux x86_64/aarch64; Rust 1.82+ |
| Remote clone | Git available on `PATH`; local repository paths do not require Git |
| Windows isolation | Job Objects; optional Windows Sandbox on supported editions |
| Linux isolation | cgroups and namespaces where available; optional rootless Podman, Docker, or bubblewrap |

## Build

```bash
cargo build --release
```

On Linux:

```bash
./target/release/sand --version
```

On Windows PowerShell:

```powershell
.\target\release\sand.exe --version
```

## Quick start

Linux:

```bash
sand inspect ~/work/unfamiliar-repo
sand run ~/work/unfamiliar-repo --keep
sand run https://github.com/owner/repo.git --yes --timeout 300 --json
sand restore ./repo.envspec.utf8 --run --timeout 300
sand doctor
sand gui
sand mcp
```

Windows PowerShell:

```powershell
sand inspect C:\work\unfamiliar-repo
sand run C:\work\unfamiliar-repo --keep
sand run https://github.com/owner/repo.git --yes --timeout 300 --json
sand restore .\repo.envspec.utf8 --run --timeout 300
sand doctor
sand gui
sand mcp
```

## Commands

| Command | Purpose |
|---|---|
| `inspect <target>` | Analyze without executing repository code |
| `run <target>` | Acquire, analyze, provision, stage, run, snapshot, and close |
| `restore <file>` | Rebuild from `.envspec.utf8`; optionally run it |
| `list` | Show known sandboxes and their lifecycle state |
| `clean` | Remove stale sandboxes, cached clones, downloads, and logs |
| `doctor [path]` | Report RAM, disk, runtimes, caches, and stale sandboxes |
| `net-tree` | Show sandbox process trees and TCP/UDP flows |
| `gui` | Start the local dashboard |
| `mcp` | Start the newline-delimited JSON-RPC 2.0 MCP server |

## Data locations

Sand stores generated data in the user’s application-data directory:

- **Windows:** `%LOCALAPPDATA%\\Sand`
- **Linux:** `${XDG_DATA_HOME:-$HOME/.local/share}/sand`

```text
cache/workspaces/    shallow clones of remote repositories
cache/downloads/     verified runtime archives
sandboxes/<id>/      staged workspace, home, temp, caches, logs, metadata
toolchains/          runtimes retained with Keep
snapshots/           GUI/MCP restore files
logs/                diagnostics
locks/               cross-process coordination
```

Sand treats the original repository as read-only. It stages a copy in the sandbox and never runs project commands from the original checkout.

## Keep or discard

When `run` or `restore` finishes, Sand asks whether to keep or discard the environment:

- **Keep** retains the sandbox and fetched runtimes in Sand-managed directories. It does not change system-wide configuration.
- **Discard** writes the restore file, removes the sandbox, and reclaims downloads and cached clones owned by that run.
- Use `--keep` or `--discard` to choose in advance. In non-interactive mode, `--yes` defaults to discard unless `--keep` is also specified.

## Dashboard and MCP

Start the local dashboard on either operating system:

```bash
sand gui --bind 127.0.0.1:8765 --no-open
```

Example MCP configuration for Linux:

```json
{
  "mcpServers": {
    "sand": {
      "command": "/path/to/sand",
      "args": ["mcp"]
    }
  }
}
```

On Windows, set `command` to the full path of `sand.exe`. MCP tools include `analyze_repo`, `run_sandbox`, `inspect_net_tree`, `restore_snapshot`, and `teardown_sandbox`.

## Safety model

- Keep the original checkout read-only and run code from a staged copy.
- Clear the child process environment, then add only sandbox and required system paths.
- Use Windows Job Objects for process management and optional memory limits. Use Linux cgroups v2 where available.
- Use Windows Sandbox, Linux namespaces, or rootless containers for stronger isolation when available. Report the isolation level actually in effect.
- Download runtimes only from official vendor endpoints and verify archives with SHA-256 before extraction.
- Check teardown paths against the sandbox base before removing data.
- Never force-kill unrelated user applications.

## Limitations

- Sand can analyze Rust and Go projects; initial runtime fetching focuses on Python, Node.js, and Java.
- Remote cloning requires Git. Local repository paths do not.
- Isolation strength depends on the operating system, kernel features, permissions, and available sandbox tools.
- Sand reports version floors supported by manifests and syntax. It does not guess the latest version.
