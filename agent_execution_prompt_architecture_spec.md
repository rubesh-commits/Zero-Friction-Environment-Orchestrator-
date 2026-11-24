# Project Build Prompt: Zero-Friction Deterministic Environment & Sandbox Orchestrator

## 1. Role & Mission
You are a Senior Systems Engineer and Rust Architect. Your task is to design, implement, and verify a **standalone, single-binary Rust application** that eliminates the friction of running unfamiliar codebases.

**Core Problem Solved:** Students, code reviewers, and hackathon judges routinely waste hours fighting broken setups, missing lockfiles, and mismatched runtimes before they can even inspect or run someone else's code. Furthermore, developer tools themselves often require tedious setup.

**Your Mission:** Build a tool that:
1. Requires **zero setup of its own** (ships as a single statically-linked Rust binary with zero runtime dependencies).
2. Takes any Git repository URL or local path and autonomously resolves, provisions, isolates, and executes it without manual patching.
3. Operates **100% deterministically without AI/LLM calls** in its core resolution engine.

---

## 2. Non-Negotiable Architectural & Ethical Constraints

1. **Zero AI in Core Resolution:** Do NOT use LLMs or external AI APIs to parse code or guess dependencies. All dependency mapping, language version detection, and API/syntax compatibility checks MUST be performed by deterministic Rust algorithms (manifest parsers + `tree-sitter` AST analysis + embedded compatibility rule tables).
2. **Lightest-Viable Footprint:** Never pull bloated IDEs, full SDKs, or heavy base images when a minimal runtime or compiler binary suffices (e.g., fetch standalone `javac` / minimal JDK instead of Eclipse or a full enterprise JDK; use minimal/embedded Python builds).
3. **Host Injection First:** Always probe the host OS first. If compatible runtimes or cached libraries already exist locally, securely mount/symlink/inject them into the sandbox rather than re-downloading them.
4. **Ethical Resource Management:** Never forcefully terminate user applications, and never pressure or prompt the user to kill personal background apps (e.g., Spotify, Discord). If RAM or disk space is constrained, fall back to a low-memory/paging-friendly execution profile and display a **non-intrusive, read-only resource advisory** listing top memory consumers so the user retains full autonomy.
5. **Clean Host Hygiene:** Never modify the host `PATH` or persist downloaded toolchains on the host unless the user explicitly chooses **Keep** upon exit.
6. **Fresh-Machine Guarantee:** The compiled binary must work out-of-the-box on a completely clean, unconfigured OS installation with zero manual intervention.

---

## 3. Recommended Rust Workspace Architecture

Organize the repository as a Cargo workspace with clear separation of concerns:

```text
env-orchestrator/
├── Cargo.toml                  # Workspace root
├── crates/
│   ├── core/                   # Shared domain models, error types, pipeline state machine
│   ├── git-acquire/            # Autonomous repo cloning, shallow fetch, workspace staging
│   ├── analyzer/               # Deterministic parser (README/manifests + tree-sitter AST + API version rules)
│   ├── host-probe/             # OS scanner (installed runtimes, caches, disk/RAM telemetry, stale env detector)
│   ├── provisioner/            # Minimal toolchain downloader, checksum verifier, host-to-sandbox injector
│   ├── sandbox/                # Cross-platform isolated virtual environment & process runner
│   ├── snapshot/               # Custom human-readable UTF-8 config generator & one-command restorer
│   ├── net-tree/               # Live process/port/socket monitor for serving & receiving topology trees
│   ├── cli/                    # Fast terminal interface (primary binary entrypoint)
│   ├── gui/                    # Desktop UI (e.g., egui/tauri) with live tree maps & interactive controls
│   └── mcp/                    # Thin Model Context Protocol server wrapping `core` for AI client integration
└── tests/
    └── fresh_machine/          # Automated clean-container / VM end-to-end acceptance suite
```

---

## 4. Detailed Functional Pipeline (Step-by-Step Specification)

Implement the execution lifecycle as an explicit state machine in `crates/core`:

### Step 1: Autonomous Repository Acquisition (`crates/git-acquire`)
* Accept a remote Git URL (or local directory).
* Clone the repository cleanly into an isolated staging directory (`~/.cache/<tool-name>/workspaces/<repo-hash>` or OS temp equivalent).
* Detect repository structure, entrypoints, build scripts, and documentation files.

### Step 2: Deterministic Code & Spec Mapping (`crates/analyzer`)
Without invoking any AI model, build a static analysis pipeline that inspects both documentation and source code:
1. **Manifest & Doc Extraction:** Parse `README.md` (code blocks, install commands, version badges), `requirements.txt`, `pyproject.toml`, `dependencies.txt`, `package.json`, `Cargo.toml`, `pom.xml`, `build.gradle`, `go.mod`, etc.
2. **AST Source Inspection (`tree-sitter`):** Parse source files directly (start with **Python**, **JavaScript/TypeScript**, and **Java**, designed with a trait-based plugin architecture for easy expansion) to extract:
   * Every imported module/library actually used in code (catching undeclared dependencies missing from lockfiles).
   * Language syntax constructs (e.g., Python `match/case` -> requires `>= 3.10`; walrus operator `:=` -> requires `>= 3.8`; f-strings -> `>= 3.6`).
   * **Version-Sensitive Function & API Signatures:** Cross-reference extracted function calls and stdlib/library methods against an embedded version-compatibility ruleset (e.g., detecting deprecated/removed APIs or behavioral differences between `3.x` and `3.x.1`) to decide whether an existing host minor version is safe to reuse or if a specific version must be fetched.
3. **Output:** Produce a deterministic `ResolutionPlan` struct detailing exact required runtimes, version bounds, libraries, and entrypoint candidates.

### Step 3: Pre-Flight Environment & Ethical Resource Guard (`crates/host-probe`)
Before creating the virtual environment:
1. **Stale/Conflicting Environment Check:** Scan for active or orphaned virtual environments/sandboxes associated with the workspace or conflicting ports. Warn the user clearly and, upon confirmation, cleanly deactivate/terminate those stale sandbox processes.
2. **Disk & RAM Audit:**
   * Estimate required disk space (for minimal downloads + workspace) and baseline RAM.
   * If free disk space is insufficient, warn the user with exact byte requirements before proceeding.
   * If free RAM is below the safe threshold, automatically enable **Low-Footprint / Paging-Friendly Execution Mode** (limiting parallel build jobs and memory caps) and display a **read-only advisory** (e.g., *"Low free RAM detected (1.1 GB available, 1.8 GB recommended). Paging-safe mode enabled. Top memory consumers on host: Discord (680 MB), Spotify (410 MB)."*). **Never offer or execute process kills on user apps.**

### Step 4: Partial Host Injection & Minimal Provisioning (`crates/provisioner` & `crates/sandbox`)
1. **Host Probe & Match:** Check host `PATH`, standard installation directories, and package caches (e.g., `~/.cache/pip`, `~/.npm`, `~/.m2`, system JDKs/Python installs) against the `ResolutionPlan`.
2. **Host Injection:** For every requirement satisfied by a compatible host runtime or cached package (verified by AST syntax/API compatibility rules), mount or symlink it read-only into the isolated virtual environment—avoiding redundant downloads.
3. **Lightest-Viable Fetch:** For unsatisfied requirements, download only the minimal standalone binaries/archives required (e.g., standalone `javac` / minimal JRE/JDK zip, portable minimal Python build, direct wheel/tarball packages) and verify SHA-256 checksums.
4. **Isolated Execution:** Launch the target project inside the isolated sandbox with scoped environment variables (`VIRTUAL_ENV`, isolated `PATH`, isolated cache dirs) and attach the `net-tree` telemetry observer.

### Step 5: Post-Run Lifecycle & UTF-8 Snapshot Generation (`crates/snapshot`)
When the user finishes inspecting/running the code, prompt for lifecycle resolution:
* **Option A — Keep & Promote:**
  * Move newly downloaded runtimes/dependencies from temporary sandbox storage into a clean, structured user tools directory (e.g., `~/.local/share/<tool-name>/toolchains/`).
  * Cleanly register the new binary paths in the user's shell profile / OS `PATH` (idempotently, without duplicate entries).
* **Option B — Destroy & Clean:**
  * Completely tear down the virtual environment, delete all downloaded dependencies and cloned temporary files, and verify zero leftover artifacts remain on the host OS.
* **Mandatory Artifact — Custom UTF-8 Restore Configuration File:**
  * Regardless of Keep/Discard (or upon teardown), write a lightweight, **strictly UTF-8 encoded** configuration file using a customizable filename (defaulting to `<repo-name>.envspec.utf8` or user-provided `--snapshot-out <filename>`).
  * **Dual-Purpose Design:** The file must be clean, human-readable text (e.g., structured TOML/INI-style UTF-8 with clear comments explaining why each version/injection choice was made) so a human can read it as documentation, AND executable via `<tool-name> restore <custom-filename>` to deterministically rebuild the exact environment in one step.

### Step 6: Clean Exit Summary
* Print a concise, transparent summary of:
  * Host dependencies reused vs. minimal packages fetched.
  * Disk space reclaimed or promoted to `PATH`.
  * Path to the generated UTF-8 restore file.
  * A friendly sign-off message (e.g., *"Environment cleanly resolved and closed. Go grab yourself a chocolate muffin—you earned it."*).

---

## 5. Interfaces & Add-Ons

### A. CLI (`crates/cli`) — Primary Interface
Implement clean, intuitive subcommands:
* `run <repo-url-or-path> [--snapshot-out <filename>] [--keep | --discard]` — Full autonomous pipeline.
* `inspect <repo-url-or-path>` — Dry-run static analysis showing detected languages, AST version constraints, host matches, and estimated download size.
* `restore <custom-utf8-file>` — Reconstruct an environment directly from a saved UTF-8 configuration file.
* `clean` — Purge any orphaned workspaces or temporary sandboxes.

### B. GUI (`crates/gui`) — Power-User & Visual Reviewer Interface
Build a responsive desktop UI sharing `crates/core` that exposes all CLI controls plus:
* **Live Serving & Receiving Tree Map (`crates/net-tree`):** Real-time visual node tree displaying processes spawned by the sandbox, bound listening ports (`SERVING`), active outbound/inbound connections (`RECEIVING`), and inter-service traffic flows.
* **Dependency & AST Diff Panel:** Visual breakdown of what was injected from the host vs. fetched minimally, plus highlighted version/deprecation warnings found in source code.
* **One-Click Keep/Discard & Snapshot Exporter:** Visual controls for post-run promotion or teardown.

### C. MCP Server (`crates/mcp`) — AI Assistant Integration
Expose the deterministic Rust engine as an **MCP (Model Context Protocol)** server over stdio/SSE with tools (`analyze_repo`, `run_sandbox`, `inspect_net_tree`, `restore_snapshot`, `teardown_sandbox`). This allows users who prefer working inside an AI IDE/client to let their assistant spin up and test unfamiliar repositories automatically—while keeping the underlying resolution engine 100% deterministic.

---

## 6. Phased Execution Plan for the Agent

Execute this build sequentially, validating tests at the end of each phase before moving to the next:

* **Phase 1: Workspace Skeleton & Deterministic Analyzer (`core`, `git-acquire`, `analyzer`)**
  * Set up the Cargo workspace.
  * Implement manifest parsers and `tree-sitter` AST extractors for Python, JS/TS, and Java, including syntax-feature and function-version compatibility tables.
  * Write unit tests verifying that undocumented repos with version-specific syntax/APIs resolve accurately without AI.
* **Phase 2: Host Probing, Ethical Guard & Provisioner (`host-probe`, `provisioner`)**
  * Implement host runtime/cache discovery, stale environment detection, and read-only RAM/disk telemetry with paging-safe fallback.
  * Implement minimal toolchain fetchers (e.g., standalone `javac`/minimal JDK, minimal Python) and host-to-sandbox injection.
* **Phase 3: Sandbox Runner, UTF-8 Snapshot & CLI (`sandbox`, `snapshot`, `cli`)**
  * Implement isolated sandbox execution, Keep (`PATH` promotion) vs. Destroy teardown, and the UTF-8 restore file writer/reader.
  * Wire up the complete `cli` binary.
* **Phase 4: Live Serving/Receiving Tree & GUI (`net-tree`, `gui`)**
  * Implement OS socket/process tree tracking (`crates/net-tree`) and build the GUI with the live topology visualizer and full pipeline controls.
* **Phase 5: MCP Server Wrapper (`mcp`)**
  * Expose `core` workflows as MCP tools and verify integration with a standard MCP client.
* **Phase 6: Fresh-Machine Acceptance Suite (`tests/fresh_machine`)**
  * Create automated Docker/VM tests using bare base OS images (no Python, Node, Java, or dev tools pre-installed).
  * Drop *only* the compiled Rust binary into the fresh environment and run end-to-end tests across sample Python, Java, and Node repos (testing both host injection and minimal fetch + clean teardown + UTF-8 restore).

---

## 7. Definition of Done

The project is complete only when:
1. `cargo test --workspace` passes with zero errors.
2. The single compiled binary executes on a **completely fresh, unconfigured machine** against unfamiliar repositories with zero manual patching.
3. No AI calls exist anywhere in the dependency or version resolution path.
4. Teardown leaves zero unintended artifacts on the host while producing a valid, human-readable UTF-8 restore configuration file.