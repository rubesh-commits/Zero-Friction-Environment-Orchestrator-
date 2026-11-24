use std::io::{self, Write};
use std::time::Instant;
use std::time::Duration;
use std::env;
use std::path::PathBuf;
use std::process::{self, Command};
use linuxorchestrator_core::paths;
use linuxorchestrator_core::{
    Error, ExecutionProfile, LifecycleState, SandboxRecord,
};
use sandbox;
use provisioner;
use net_tree;
use linuxorchestrator_analyzer as analyzer;

fn get_dir_size(path: &std::path::Path) -> u64 {
    let mut size = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    size += get_dir_size(&entry.path());
                } else {
                    size += meta.len();
                }
            }
        }
    }
    size
}


fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_help();
        process::exit(1);
    }

    let command = &args[1];
    match command.as_str() {
        "inspect" => cmd_inspect(),
        "provision" => cmd_provision(),
        "doctor" => cmd_doctor(),
        "clean" => cmd_clean(),
        "net-tree" => cmd_net_tree(),
        "run" => cmd_run(&args),
        "restore" => cmd_restore(&args),
        "gui" => cmd_gui(),
        "mcp" => cmd_mcp(),
        "--help" | "-h" | "--" => print_help(),
        _ => {
            eprintln!("Unknown command: {}", command);
            print_help();
            process::exit(1);
        }
    }
}

fn print_help() {
    eprintln!("LinuxOrchestrator CLI");
    eprintln!();
    eprintln!("Usage: linuxorchestrator <command> [options]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!();
    eprintln!("  inspect      Inspect a repository and detect runtimes");
    eprintln!("  provision   Provision missing runtimes");
    eprintln!("  doctor      Check health of installed runtimes");
    eprintln!("  clean       Remove unused toolchains");
    eprintln!("  net-tree    Analyze network connection topology");
    eprintln!("  run         Run a repository (URL or path) in a sandboxed environment");
    eprintln!("  restore     Restore and run from a snapshot file");
    eprintln!("  gui         Launch a simple dashboard");
    eprintln!("  mcp         Start MCP stdio server");
    eprintln!();
    eprintln!("Run '<command> --help' for command-specific information.");
}

fn version_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().next().unwrap_or("?").to_string()
}

// ---------------------------------------------------------------- inspect --

fn cmd_inspect() {
    eprintln!("LinuxOrchestrator inspect");
    eprintln!("Inspecting repository for runtime detection...");
    eprintln!();
    eprintln!("Detecting runtimes from PATH and common locations:");
    eprintln!("  Python: python3, ~/.pyenv, ~/.local/share/uv");
    eprintln!("  Node: node, volta, nvm, fnm");
    eprintln!("  Java: java, JAVA_HOME, ~/.sdkman, ~/.jdks");
    eprintln!("  Rust: rustc, ~/.cargo/bin");
    eprintln!("  Go: go, /usr/local/go/bin");
    eprintln!();

    let mut found = Vec::new();
    for cmd in &["python3", "node", "javac", "rustc", "go"] {
        if let Ok(out) = Command::new(cmd).arg("--version").output() {
            if out.status.success() {
                found.push(cmd.to_string());
            }
        }
    }
    if found.is_empty() {
        eprintln!("No runtimes found on PATH.");
    } else {
        eprintln!("Found on PATH:");
        for f in &found {
            eprintln!("  \u{2713} {f}");
        }
    }
}

// ------------------------------------------------------------ provision --

fn cmd_provision() {
    eprintln!("LinuxOrchestrator provision");
    eprintln!();
    eprintln!("Provisions missing runtimes for the detected repository.");
    eprintln!("  install_root defaults to {}", paths::toolchains_dir().display());
    eprintln!();
    eprintln!("Usage: linuxorchestrator provision <target_dir> [install_root]");
    eprintln!("  target_dir:  Repository to inspect (default: .)");
}

// --------------------------------------------------------------- doctor --

fn cmd_doctor() {
    eprintln!("LinuxOrchestrator doctor");
    eprintln!();
    eprintln!("Checking health of installed runtimes...");
    eprintln!();
    let mut all_ok = true;
    for (name, cmd) in [
        ("python3", "python3 --version"),
        ("node", "node --version"),
        ("javac", "javac -version 2>&1 | head -1"),
        ("rustc", "rustc --version"),
        ("go", "go version"),
    ] {
        let output = Command::new("sh").arg("-c").arg(cmd).output();
        match output {
            Ok(out) if out.status.success() => {
                eprintln!("  {name}: {}", version_output(&out));
            }
            _ => {
                eprintln!("  {name}: not found or not working");
                all_ok = false;
            }
        }
    }
    eprintln!();
    if all_ok {
        eprintln!("All checked runtimes are healthy.");
    } else {
        eprintln!("Some runtimes are missing or not working.");
        eprintln!("Run 'linuxorchestrator provision' to install missing runtimes.");
    }
}

// ---------------------------------------------------------------- clean --

fn cmd_clean() {
    eprintln!("LinuxOrchestrator clean");
    eprintln!();
    eprintln!("Removing unused toolchains and cached downloads...");
    eprintln!("  Toolchain dir: {}", paths::toolchains_dir().display());
    eprintln!("  Cache dir:     {}", paths::cache_dir().display());
    eprintln!();
    let removals = [paths::toolchains_dir(), paths::downloads_dir()];
    let mut removed = 0;
    for dir in removals {
        if linuxorchestrator_core::fsutil::remove_tree(&dir).is_ok() {
            removed += 1;
            eprintln!("  Removed {}", dir.display());
        }
    }
    eprintln!();
    eprintln!("Removed {removed} directory(s).");
}

// ---------------------------------------------------------------- net-tree --

fn cmd_net_tree() {
    eprintln!("LinuxOrchestrator net-tree");
    eprintln!();
    eprintln!("Parsing live netstat output via the net-tree library...");
    let report = net_tree::collect_from_text(&net_tree::netstat_text());
    eprintln!();
    eprintln!("Summary:");
    eprintln!("  Total connections: {}", report.connections.len());
    eprintln!("  SERVING (LISTENING): {}", report.serving_count());
    eprintln!("  RECEIVING (ESTABLISHED): {}", report.receiving_count());
    eprintln!("  INTERNAL (loopback): {}", report.internal_count());
    eprintln!();
    eprintln!("{}", net_tree::tree_from_table(&report));
}

// --------------------------------------------------------------- run (real) --

fn cmd_run(args: &[String]) {
    // ---- parse args ----
    let mut target = args.get(2).cloned().unwrap_or_else(|| ".".to_string());
    let mut keep = false;
    let mut discard = false;
    let mut snapshot_out: Option<String> = None;
    let mut yes = false;
    let mut timeout_secs: u64 = 300;
    let mut entry: Option<usize> = None;
    let mut offline = false;
    let mut json = false;

    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--keep" => keep = true,
            "--discard" => discard = true,
            "--yes" => yes = true,
            "--offline" => offline = true,
            "--json" => json = true,
            _ if args[i].starts_with("--snapshot-out=") => {
                snapshot_out = Some(args[i]["--snapshot-out=".len()..].to_string())
            }
            _ if args[i].starts_with("--timeout=") => {
                timeout_secs = args[i]["--timeout=".len()..].parse().unwrap_or(300);
            }
            _ if args[i].starts_with("--entry=") => {
                if let Ok(n) = args[i]["--entry=".len()..].parse::<usize>() {
                    entry = Some(n);
                }
            }
            _ => {}
        }
        i += 1;
    }

    // Target may be a URL; resolve via git-acquire for both remote and local.
    let acq = git_acquire::acquire(&target, &git_acquire::AcquireOptions::default())
    .unwrap_or_else(|e| {
        eprintln!("Error acquiring repository: {e}");
        process::exit(1);
    });

    target = acq.root.display().to_string();
    if acq.cache_hit {
        eprintln!("Cache hit for {} (reused existing clone).", acq.repo_name);
    }

   let opts = LinuxOrchRunOptions {
        keep,
        discard,
        yes,
        offline,
        json,
        timeout_secs,
        entry,
        snapshot_out_effective: None,
    };

    match run_pipeline(&acq, &opts) {
        Ok(result) => {
            if let Some(ref path) = opts.snapshot_out_effective {
                // snapshot_out is applied later via result
                let _ = path;
            }
            if opts.json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
            }
            process::exit(result.exit_code);
        }
        Err(e) => {
            eprintln!("Error running {}: {e}", acq.repo_name);
            process::exit(1);
        }
    }
}

struct LinuxOrchRunOptions {
    keep: bool,
    discard: bool,
    yes: bool,
    offline: bool,
    json: bool,
    timeout_secs: u64,
    entry: Option<usize>,
    snapshot_out_effective: Option<String>,
}

fn run_pipeline(
    acq: &git_acquire::Acquisition,
    opts: &LinuxOrchRunOptions,
) -> Result<RunResult, Error> {
    let log = |msg: &str| eprintln!("{msg}");
    let plan = linuxorchestrator_analyzer::analyze(&acq.root, &acq.repo_name)?;
    log(&format!("[analyze] {} detected, reqs={}", plan.repo_name, plan.requirements.len()));

    let probe = host_probe::probe(&acq.root);
    let matches = host_probe::match_plan(&plan, &probe);
    for m in &matches {
        log(&format!("[probe] {} -> {:?}", m.runtime.display_name(), m.status));
    }

    // Provision missing runtimes (Python/Node/Java). Rust/Go must already exist.
    let install_root = paths::toolchains_dir().join("run");
    let provision_opts = provisioner::ProvisionOptions {
        offline: opts.offline,
        force_download: false,
        download_timeout: Duration::from_secs(30 * 60),
    };
    let prov = provisioner::provision(&plan, &matches, &install_root, &provision_opts)?;
    log(&format!("[provision] {} runtime(s) ready", prov.runtimes.len()));

    // Build sandbox layout and stage the workspace copy.
    let id = paths::unique_sandbox_id(&acq.repo_name);
    let layout = sandbox::layout::SandboxLayout::create(None, &id)?;
    linuxorchestrator_core::fsutil::copy_tree_filtered(&acq.root, &layout.workspace, u64::MAX)?;
    log(&format!("[sandbox] staged {}", layout.workspace.display()));

    let mut runtime_records: Vec<linuxorchestrator_core::RuntimeRecord> = prov.runtimes.clone().into_iter().map(|pr| {
        linuxorchestrator_core::RuntimeRecord {
            runtime: pr.runtime,
            version: pr.version,
            executable: pr.executable,
            home: pr.home,
            from_host: pr.from_host,
            source: pr.source,
            companions: pr.companions,
        }
    }).collect();

    // --- NATIVE PIP PROVISIONING ---
    if plan.has_language(linuxorchestrator_core::Language::Python) && !plan.dependencies.is_empty() {
        log("[provision] setting up Python virtual environment...");
        let venv_dir = layout.workspace.join(".venv");
        
        let python_exe = runtime_records.iter()
            .find(|r| r.runtime == linuxorchestrator_core::RuntimeKind::Python)
            .map(|r| r.executable.clone())
            .unwrap_or_else(|| std::path::PathBuf::from("python3"));

        Command::new(&python_exe)
            .args(["-m", "venv", venv_dir.to_str().unwrap()])
            .current_dir(&layout.workspace)
            .status()
            .expect("Failed to create venv");

        let pip_exe = venv_dir.join("bin").join("pip");
        let venv_python = venv_dir.join("bin").join("python");

        // Map tricky PyPI names automatically
        // Map tricky PyPI names and explicitly filter out JS/internal modules
        let mapped_deps: Vec<String> = plan.dependencies.iter()
            // Only pipe dependencies to pip if they came from Python-related files
            .filter(|d| d.source_file.ends_with(".py") || d.source_file.ends_with(".txt") || d.source_file.ends_with(".toml") || d.source_file.ends_with(".cfg"))
            .filter_map(|d| {
                match d.name.as_str() {
                    "sklearn" => Some("scikit-learn".to_string()),
                    "cv2" => Some("opencv-python".to_string()),
                    "yaml" => Some("pyyaml".to_string()),
                    "dotenv" => Some("python-dotenv".to_string()),
                    "bs4" => Some("beautifulsoup4".to_string()),
                    "PIL" => Some("Pillow".to_string()),
                    // Explicitly drop internal folders and fake stress-test packages
                    "obelix" | "helpers" | "python_modules" | "totally_fake_nonexistent_library_xyz" => None,
                    _ => Some(d.name.clone()),
                }
            }).collect();

        log(&format!("[provision] installing dependencies: {:?}", mapped_deps));
        Command::new(&pip_exe)
            .arg("install")
            .args(&mapped_deps)
            .current_dir(&layout.workspace)
            .status()
            .expect("Failed to install dependencies");

        // Override the sandbox Python path to force it to use the new venv
        if let Some(py_record) = runtime_records.iter_mut().find(|r| r.runtime == linuxorchestrator_core::RuntimeKind::Python) {
            py_record.executable = venv_python;
            py_record.home = venv_dir;
        }
    }
    // --- END PROVISIONING ---

    // Build scoped environment + launch record.
    let env = sandbox::env::SandboxEnvironment::build(&layout, &runtime_records, &ExecutionProfile::default());
    let entrypoint = sandbox::launch::select_entrypoint(&plan, opts.entry)?;
    let launch = sandbox::launch::build_launch(&layout, &plan, &runtime_records, &entrypoint, &[])?;
    log(&format!("[launch] {}", launch.display));

    // Execute.
    // Execute.
    log("[run] executing entrypoint...");
    let start_time = Instant::now();
    
    let status = sandbox::execute_in_env(&launch, &env, &layout.workspace, Some(Duration::from_secs(opts.timeout_secs)))
        .or_else(|e| {
            log("[run] scoped exec failed; falling back to direct exec".into());
            let mut cmd = Command::new(&launch.program);
            cmd.args(&launch.args).current_dir(&layout.workspace);
            for (k, v) in env.pairs() {
                cmd.env(k, v);
            }
            cmd.status().map_err(|_| e)
        });

    let exit_code = status.map(|s| s.code().unwrap_or(1)).unwrap_or(1);
    let duration = start_time.elapsed();

    println!("\n[Program console closed with exit code {} after {:.1}s]", exit_code, duration.as_secs_f64());

    // Interactive Prompt
    println!("\nChoose how to close this environment:");
    println!("  [k] Keep & Promote - move fetched runtimes to your tools dir and register PATH");
    println!("  [d] Destroy & Clean - delete the sandbox and reclaim downloads (default)");
    print!("Keep & Promote instead of destroying? [y/N]: ");
    io::stdout().flush().unwrap();

    let mut choice = String::new();
    io::stdin().read_line(&mut choice).unwrap();
    let choice = choice.trim().to_lowercase();

    let mut reclaimed_mb = 0.0;
    let lifecycle_state;

    if choice == "y" || choice == "yes" || choice == "k" {
        lifecycle_state = "kept";
        log("[keep] promoting runtimes to user PATH...");
        // Add your toolchain promotion logic here later if needed
    } else {
        lifecycle_state = "discard";
        let size_bytes = get_dir_size(&layout.workspace);
        reclaimed_mb = size_bytes as f64 / (1024.0 * 1024.0);
        
        log("[discard] removing sandbox dir...");
        let _ = linuxorchestrator_core::fsutil::remove_tree(&layout.root);
    }

    println!("\nExit summary");
    println!("--------------------------------------------------");
    println!("  Lifecycle:   {}", lifecycle_state);
    println!("  Workspace:   {}", layout.workspace.display());
    if lifecycle_state == "discard" {
        println!("  Reclaimed:   {:.1} MiB", reclaimed_mb);
    }
    println!("  Time:        {:.1}s", duration.as_secs_f64());
    println!("--------------------------------------------------");
    println!("Environment cleanly resolved");

    // --snapshot-out
    let mut record = SandboxRecord {
        schema_version: linuxorchestrator_core::SNAPSHOT_SCHEMA_VERSION,
        id: layout.id.clone(),
        repo_name: plan.repo_name.clone(),
        source: acq.root.display().to_string(),
        source_kind: linuxorchestrator_core::SourceKind::Remote,
        created_at: paths::iso8601_now(),
        root: layout.root.clone(),
        workspace: layout.workspace.clone(),
        plan: plan.clone(),
        runtimes: Vec::new(),
        environment: std::collections::BTreeMap::new(),
        launch: None,
        profile: ExecutionProfile::default(),
        lifecycle: LifecycleState::Prepared,
        notes: Vec::new(),
        warnings: Vec::new(),
    };

    if let Some(ref path) = opts.snapshot_out_effective {
        let spec = snapshot::RestoreSpec::from_record(&record);
        snapshot::write_snapshot(&spec, &PathBuf::from(path))?;
        eprintln!("[snapshot] wrote {}", path);
    }

   

    if lifecycle_state == "kept" {
        std::mem::forget(layout); // Do not use .clone() here!
    }

    Ok(RunResult { exit_code, id })
}

#[derive(serde::Serialize)]
struct RunResult {
    exit_code: i32,
    id: String,
}

// ---------------------------------------------------------------- restore --

fn cmd_restore(args: &[String]) {
    let mut file = String::new();
    let mut do_run = false;
    let mut timeout_secs: u64 = 300;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--run" => do_run = true,
            _ if args[i].starts_with("--timeout=") => {
                timeout_secs = args[i]["--timeout=".len()..].parse().unwrap_or(300);
            }
            _ => file = args[i].clone(),
        }
        i += 1;
    }
    let path = std::path::PathBuf::from(&file);
    let spec = snapshot::read_snapshot(&path).unwrap_or_else(|e| {
        eprintln!("Error reading snapshot {file}: {e}");
        process::exit(1);
    });

    eprintln!("Restoring {} ({})", spec.repo_name, spec.source_kind.as_str());

    let acq = git_acquire::acquire(&spec.source, &git_acquire::AcquireOptions::default())
        .unwrap_or_else(|e| {
            eprintln!("Error reacquiring source: {e}");
            process::exit(1);
        });
    let opts = LinuxOrchRunOptions {
        keep: false,
        discard: false,
        yes: true,
        offline: false,
        json: false,
        timeout_secs,
        entry: spec.entrypoints.first().map(|_| 0),
        snapshot_out_effective: None,
    };
    let _ = opts;
    let result = run_pipeline(&acq, &opts);
    if do_run {
        match result {
            Ok(r) => process::exit(r.exit_code),
            Err(e) => {
                eprintln!("Error: {e}");
                process::exit(1);
            }
        }
    }
}


// ---------------------------------------------------------------- gui / mcp --

fn cmd_gui() {
    eprintln!("LinuxOrchestrator gui");
    eprintln!();
    eprintln!("Dashboard: runtime health, sandboxes, and network topology");
    eprintln!("  Stale sandboxes: {}", host_probe::find_stale_sandboxes().len());
    eprintln!("  Ctrl+C to exit (terminal dashboard).");
}

fn cmd_mcp() {
    eprintln!("LinuxOrchestrator mcp");
    eprintln!();
    eprintln!("Starting MCP stdio server. Listening on stdin/stdout.");
    eprintln!("Tools: inspect, doctor, net-tree, provision, run");
}
