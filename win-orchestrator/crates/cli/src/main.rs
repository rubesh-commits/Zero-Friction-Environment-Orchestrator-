use clap::{Args, Parser, Subcommand};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;
use winorchestrator_core::{paths, process, KeepPolicy, LifecycleState, SourceKind};
use winorchestrator_workflow::{
    CleanOptions, NetObserver, PipelineOptions, PreparedRun, RestoreOptions,
};

#[derive(Parser, Debug)]
#[command(
    name = "winorchestrator",
    version,
    about = "Deterministic Windows environment and sandbox orchestrator (no AI calls)",
    long_about = "Inspects an unfamiliar repository, injects compatible host runtimes, fetches only \
                  missing toolchains with SHA-256 verification, runs the project in an isolated \
                  Windows sandbox, and writes a strict UTF-8 restore file."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Build an isolated sandbox, run the project and write a restore file.
    Run(RunArgs),
    /// Dry-run analysis: languages, version constraints, host matches, download estimate.
    Inspect(InspectArgs),
    /// Rebuild an environment from a saved `.envspec.utf8` file.
    Restore(RestoreArgs),
    /// Purge cached clones, sandboxes, downloads and logs.
    Clean(CleanArgs),
    /// Read-only host diagnostics and paging-safe advice.
    Doctor(DoctorArgs),
    /// Show the sandbox process tree with SERVING/RECEIVING sockets.
    NetTree(NetTreeArgs),
    /// Start the local web dashboard.
    Gui(GuiArgs),
    /// Speak the Model Context Protocol over stdio.
    Mcp,
    /// List known sandboxes.
    List(ListArgs),
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Repository URL or local path.
    target: String,
    /// Where to write the UTF-8 restore file (default `<repo>.envspec.utf8`).
    #[arg(long, value_name = "FILE")]
    snapshot_out: Option<PathBuf>,
    /// Promote fetched runtimes into the user toolchain directory and update PATH.
    #[arg(long, conflicts_with = "discard")]
    keep: bool,
    /// Tear down the sandbox and reclaim downloaded artifacts.
    #[arg(long, conflicts_with = "keep")]
    discard: bool,
    /// Do not download anything; fail if a runtime is missing.
    #[arg(long)]
    offline: bool,
    /// Ignore compatible host runtimes and fetch fresh minimal builds.
    #[arg(long)]
    force_download: bool,
    /// Skip venv/npm/javac setup.
    #[arg(long = "no-setup")]
    no_setup: bool,
    /// 1-based entrypoint index from `inspect`.
    #[arg(long, value_name = "N")]
    entrypoint: Option<usize>,
    /// Extra argument appended to the launch command.
    #[arg(long = "arg", value_name = "ARG", action = clap::ArgAction::Append)]
    args: Vec<String>,
    /// Hard memory cap for the sandbox process tree, in MiB.
    #[arg(long, value_name = "MIB")]
    memory_limit: Option<u64>,
    /// Also emit an optional Windows Sandbox `.wsb` file.
    #[arg(long)]
    windows_sandbox: bool,
    /// Non-interactive: accept defaults (does not modify PATH unless --keep).
    #[arg(long)]
    yes: bool,
    /// Keep PATH unchanged even when Keep & Promote is selected.
    #[arg(long)]
    no_update_path: bool,
    /// Run non-interactively with captured output and this timeout.
    #[arg(long, value_name = "SECONDS")]
    timeout: Option<u64>,
    /// Machine-readable summary.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct InspectArgs {
    target: String,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct RestoreArgs {
    /// The `.envspec.utf8` file to execute.
    file: PathBuf,
    /// Override the recorded source (useful when the original path moved).
    #[arg(long)]
    target: Option<String>,
    #[arg(long, conflicts_with = "discard")]
    keep: bool,
    #[arg(long, conflicts_with = "keep")]
    discard: bool,
    /// Run the restored project immediately.
    #[arg(long)]
    run: bool,
    #[arg(long, value_name = "SECONDS")]
    timeout: Option<u64>,
    #[arg(long = "arg", value_name = "ARG", action = clap::ArgAction::Append)]
    args: Vec<String>,
    #[arg(long)]
    offline: bool,
    #[arg(long = "no-setup")]
    no_setup: bool,
    #[arg(long, value_name = "FILE")]
    snapshot_out: Option<PathBuf>,
    #[arg(long)]
    yes: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct CleanArgs {
    #[arg(long)]
    sandboxes_only: bool,
    #[arg(long)]
    no_workspaces: bool,
    #[arg(long)]
    no_downloads: bool,
    #[arg(long)]
    no_logs: bool,
    /// Remove directories even if they are missing a sandbox record.
    #[arg(long)]
    force: bool,
    /// Terminate processes attached to stale sandbox records first.
    #[arg(long)]
    stop_stale: bool,
    #[arg(long)]
    yes: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct DoctorArgs {
    #[arg(default_value = ".")]
    target: PathBuf,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct NetTreeArgs {
    /// Sandbox root pid (repeatable). Empty means every process.
    #[arg(long, value_name = "PID")]
    pid: Vec<u32>,
    #[arg(long)]
    all: bool,
    #[arg(long)]
    json: bool,
    /// Refresh every N seconds until Ctrl+C.
    #[arg(long, value_name = "SECONDS")]
    watch: Option<u64>,
}

#[derive(Args, Debug)]
struct GuiArgs {
    #[arg(long, default_value = "127.0.0.1:0")]
    bind: String,
    #[arg(long)]
    no_open: bool,
}

#[derive(Args, Debug)]
struct ListArgs {
    #[arg(long)]
    json: bool,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Run(args) => run(args),
        Command::Inspect(args) => inspect(args),
        Command::Restore(args) => restore(args),
        Command::Clean(args) => clean(args),
        Command::Doctor(args) => doctor(args),
        Command::NetTree(args) => net_tree(args),
        Command::Gui(args) => gui(args),
        Command::Mcp => winorchestrator_mcp::run_stdio(),
        Command::List(args) => list(args),
    };
    if let Err(err) = result {
        eprintln!("\nError: {err}");
        std::process::exit(1);
    }
}

fn section(title: &str) {
    println!("\n{title}");
    println!("{}", "-".repeat(title.len().max(8)));
}

fn print_inspection(inspection: &winorchestrator_workflow::Inspection) {
    println!(
        "{} {} — deterministic inspection",
        winorchestrator_core::PRODUCT_NAME,
        winorchestrator_core::PRODUCT_VERSION
    );
    println!("Repository: {}", inspection.acquisition.repo_name);
    println!("Source:     {}", inspection.target);
    println!("Checkout:   {}", inspection.acquisition.root.display());
    if let Some(commit) = &inspection.acquisition.commit {
        println!("Commit:     {commit}");
    }
    let languages = if inspection.plan.languages.is_empty() {
        "none detected".to_string()
    } else {
        inspection
            .plan
            .languages
            .iter()
            .map(|language| language.display_name())
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!("Languages:  {languages}");

    section("Runtime requirements");
    if inspection.plan.requirements.is_empty() {
        println!("  (no version constraints were derived)");
    }
    for requirement in &inspection.plan.requirements {
        let host = inspection
            .matches
            .iter()
            .find(|entry| entry.runtime == requirement.runtime);
        let status = host
            .map(|entry| {
                if entry.is_compatible() {
                    format!("HOST OK ({})", entry.detail)
                } else {
                    format!("NEEDS FETCH ({})", entry.detail)
                }
            })
            .unwrap_or_else(|| "NO HOST RUNTIME".to_string());
        println!(
            "  {:<8} {:<28} {}  [{}]",
            requirement.runtime.display_name(),
            requirement.constraint.to_string(),
            requirement.reason,
            status
        );
        println!("           source: {}", requirement.source);
    }

    section("Entrypoint candidates");
    if inspection.plan.entrypoints.is_empty() {
        println!("  (none found — pass an explicit command or add a conventional entry file)");
    }
    for (index, entrypoint) in inspection.plan.entrypoints.iter().enumerate() {
        println!(
            "  {}. {:<14} {}  — {}",
            index + 1,
            format!("{:?}", entrypoint.kind),
            entrypoint.describe(),
            entrypoint.reason
        );
    }

    section("Host & download plan");
    println!(
        "  RAM:      {} total, {} available ({}% used)",
        winorchestrator_core::fsutil::human_bytes(inspection.probe.memory_total_bytes),
        winorchestrator_core::fsutil::human_bytes(inspection.probe.memory_available_bytes),
        inspection.probe.memory_load_percent
    );
    println!(
        "  Disk:     {} free on {}",
        winorchestrator_core::fsutil::human_bytes(inspection.probe.disk_free_bytes),
        paths::volume_root(&inspection.acquisition.root).display()
    );
    println!(
        "  Downloads: about {} minimal bytes",
        winorchestrator_core::fsutil::human_bytes(inspection.estimated_download_bytes)
    );
    println!(
        "  Profile:   {} parallel job(s){}{}",
        inspection.probe.profile.parallel_jobs,
        if inspection.probe.profile.low_footprint {
            ", low-footprint / paging-safe"
        } else {
            ""
        },
        inspection
            .probe
            .profile
            .memory_limit_mb
            .map(|mb| format!(", {mb} MiB sandbox cap"))
            .unwrap_or_default()
    );
    for advisory in &inspection.probe.profile.advisory {
        println!("  Advisory:  {advisory}");
    }
    println!(
        "  Dependencies: {} declared, {} found only by AST",
        inspection
            .plan
            .dependencies
            .iter()
            .filter(|dependency| {
                dependency.origin == winorchestrator_core::DependencyOrigin::Manifest
            })
            .count(),
        inspection
            .plan
            .dependencies
            .iter()
            .filter(|dependency| dependency.origin == winorchestrator_core::DependencyOrigin::Ast)
            .count()
    );
    if !inspection.warnings.is_empty() {
        section("Warnings");
        for warning in &inspection.warnings {
            println!("  ! {warning}");
        }
    }
    if !inspection.notes.is_empty() {
        section("Notes");
        for note in &inspection.notes {
            println!("  - {note}");
        }
    }
}

fn prompt_yes(question: &str, default: bool) -> bool {
    print!("{question} [{}] ", if default { "Y/n" } else { "y/N" });
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return default;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "" => default,
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default,
    }
}

fn resolve_policy(keep: bool, discard: bool, yes: bool) -> KeepPolicy {
    if keep {
        return KeepPolicy::Keep;
    }
    if discard {
        return KeepPolicy::Discard;
    }
    if yes {
        return KeepPolicy::Discard;
    }
    println!();
    println!("Choose how to close this environment:");
    println!("  [k] Keep & Promote — move fetched runtimes to your tools dir and register PATH");
    println!("  [d] Destroy & Clean — delete the sandbox and reclaim downloads (default)");
    if prompt_yes("Keep & Promote instead of destroying?", false) {
        KeepPolicy::Keep
    } else {
        KeepPolicy::Discard
    }
}

fn maybe_clean_stale(probe: &winorchestrator_host_probe::ProbeReport, yes: bool) {
    if probe.stale_sandboxes.is_empty() || yes {
        return;
    }
    println!();
    println!("Stale sandbox records were found:");
    for stale in &probe.stale_sandboxes {
        println!(
            "  - {} at {} ({})",
            stale.id,
            stale.path.display(),
            stale.reason
        );
    }
    if !prompt_yes(
        "Stop their processes and remove these stale sandboxes before continuing?",
        false,
    ) {
        return;
    }
    for stale in &probe.stale_sandboxes {
        if let Some(pid) = stale.pid {
            if winorchestrator_core::winapi::is_pid_alive(pid) {
                let outcome = process::terminate_pid_tree(pid);
                if outcome.success {
                    println!("  stopped stale pid {pid}");
                } else {
                    println!("  could not stop pid {pid}: {}", outcome.combined());
                }
            }
        }
        match winorchestrator_sandbox::destroy(&stale.path, true) {
            Ok(()) => println!("  removed {}", stale.path.display()),
            Err(err) => println!("  could not remove {}: {err}", stale.path.display()),
        }
    }
}

fn print_prepared(prepared: &PreparedRun) {
    section("Sandbox");
    println!("  id:        {}", prepared.sandbox.record.id);
    println!("  root:      {}", prepared.sandbox.layout.root.display());
    println!(
        "  workspace: {}",
        prepared.sandbox.layout.workspace.display()
    );
    println!(
        "  runtimes:  {}",
        winorchestrator_workflow::runtime_summary(&prepared.sandbox.record.runtimes)
    );
    println!("  launch:    {}", prepared.sandbox.launch_description());
    if !prepared.provision.notes.is_empty() {
        for note in &prepared.provision.notes {
            println!("  - {note}");
        }
    }
    for warning in &prepared.sandbox.record.warnings {
        println!("  ! {warning}");
    }
}

fn print_outcome(outcome: &winorchestrator_sandbox::RunOutcome) {
    if !outcome.stdout.trim().is_empty() {
        println!("{}", outcome.stdout.trim_end());
    }
    if !outcome.stderr.trim().is_empty() {
        eprintln!("{}", outcome.stderr.trim_end());
    }
    println!(
        "\nProcess exited with {:?} after {:.1}s{}.",
        outcome.exit_code,
        outcome.duration.as_secs_f64(),
        if outcome.timed_out {
            " (timed out)"
        } else {
            ""
        }
    );
}

fn run_interactive(
    prepared: &mut PreparedRun,
) -> winorchestrator_core::Result<winorchestrator_sandbox::RunOutcome> {
    let running = prepared.start()?;
    println!("\nSandbox pid {}: {}", running.pid, running.command);
    println!("Stop the run with Ctrl+C at any time; the Job Object cleans up descendants.");
    let observer = NetObserver::start(running.pid, Duration::from_secs(2));
    let outcome = running.wait(None)?;
    prepared.finish(&outcome)?;
    if let Some(report) = observer.latest() {
        section("Sandbox sockets (final sample)");
        println!("{}", report.render_tree());
    }
    Ok(outcome)
}

fn run_pipeline(
    mut prepared: PreparedRun,
    policy: KeepPolicy,
    snapshot_out: Option<PathBuf>,
    update_path: bool,
    json: bool,
) -> winorchestrator_core::Result<()> {
    print_prepared(&prepared);
    let outcome = run_interactive(&mut prepared)?;
    print_outcome(&outcome);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let snapshot_path = snapshot_out.unwrap_or_else(|| prepared.snapshot_path(&cwd));
    let report =
        winorchestrator_workflow::finalize(&mut prepared, policy, &snapshot_path, update_path)?;
    if json {
        let value = serde_json::json!({
            "sandbox": prepared.sandbox.record.id,
            "repo": prepared.sandbox.record.repo_name,
            "exit_code": outcome.exit_code,
            "success": outcome.success,
            "timed_out": outcome.timed_out,
            "finalize": report,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    section("Exit summary");
    println!("  lifecycle:     {}", report.lifecycle);
    if let Some(path) = &report.snapshot_path {
        println!("  restore file:  {}", path.display());
    }
    if !report.promoted.is_empty() {
        for path in &report.promoted {
            println!("  promoted:      {}", path.display());
        }
    }
    if report.path_updated {
        println!("  PATH:          user PATH updated (open a new terminal to use it)");
        if let Some(backup) = &report.path_backup {
            println!("  PATH backup:   {}", backup.display());
        }
    }
    if report.sandbox_removed {
        println!(
            "  reclaimed:     {}",
            winorchestrator_core::fsutil::human_bytes(report.disk_reclaimed_bytes)
        );
    }
    for artifact in &report.removed_artifacts {
        println!("  removed:       {}", artifact.display());
    }
    for note in &report.notes {
        println!("  - {note}");
    }
    for warning in &report.warnings {
        println!("  ! {warning}");
    }
    for error in &report.errors {
        println!("  ! {error}");
    }
    if report.errors.is_empty() {
        println!("\nEnvironment cleanly resolved and closed. Go grab yourself a chocolate muffin—you earned it.");
    }
    Ok(())
}

fn run(args: RunArgs) -> winorchestrator_core::Result<()> {
    paths::ensure_app_dirs()?;
    let inspection = winorchestrator_workflow::inspect(&args.target)?;
    print_inspection(&inspection);
    maybe_clean_stale(&inspection.probe, args.yes);

    let policy = resolve_policy(args.keep, args.discard, args.yes);
    let options = PipelineOptions {
        target: args.target,
        snapshot_out: args.snapshot_out.clone(),
        offline: args.offline,
        force_download: args.force_download,
        setup: !args.no_setup,
        write_windows_sandbox: args.windows_sandbox,
        entrypoint_index: args.entrypoint.map(|value| value.saturating_sub(1)),
        extra_args: args.args.clone(),
        memory_limit_mb: args.memory_limit,
        update_path: !args.no_update_path,
        ..Default::default()
    };
    println!("\nPreparing the sandbox (host injection first, minimal fetch when needed)...");
    let mut prepared = winorchestrator_workflow::prepare(&options)?;
    if let Some(seconds) = args.timeout {
        print_prepared(&prepared);
        let outcome = prepared.run_captured(Some(Duration::from_secs(seconds)))?;
        print_outcome(&outcome);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let snapshot_path = options
            .snapshot_out
            .clone()
            .unwrap_or_else(|| prepared.snapshot_path(&cwd));
        let report = winorchestrator_workflow::finalize(
            &mut prepared,
            policy,
            &snapshot_path,
            options.update_path,
        )?;
        if args.json {
            let value = serde_json::json!({
                "sandbox": prepared.sandbox.record.id,
                "exit_code": outcome.exit_code,
                "success": outcome.success,
                "timed_out": outcome.timed_out,
                "stdout": outcome.stdout,
                "stderr": outcome.stderr,
                "finalize": report,
            });
            println!("{}", serde_json::to_string_pretty(&value)?);
        } else {
            section("Exit summary");
            if let Some(path) = report.snapshot_path {
                println!("  restore file: {}", path.display());
            }
            println!("  lifecycle:    {}", report.lifecycle);
            if report.sandbox_removed {
                println!(
                    "  reclaimed:    {}",
                    winorchestrator_core::fsutil::human_bytes(report.disk_reclaimed_bytes)
                );
            }
            for note in report.notes {
                println!("  - {note}");
            }
            for error in report.errors {
                println!("  ! {error}");
            }
            println!("\nEnvironment cleanly resolved and closed. Go grab yourself a chocolate muffin—you earned it.");
        }
        return Ok(());
    }
    run_pipeline(
        prepared,
        policy,
        options.snapshot_out,
        options.update_path,
        args.json,
    )
}

fn inspect(args: InspectArgs) -> winorchestrator_core::Result<()> {
    let inspection = winorchestrator_workflow::inspect(&args.target)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&inspection)?);
    } else {
        print_inspection(&inspection);
    }
    Ok(())
}

fn restore(args: RestoreArgs) -> winorchestrator_core::Result<()> {
    let spec = winorchestrator_snapshot::read_snapshot(&args.file)?;
    println!(
        "Restore file: {} (schema {}, generated {})",
        args.file.display(),
        spec.schema_version,
        spec.generated_at
    );
    println!("Repository:   {} ({})", spec.repo_name, spec.source);
    println!(
        "Runtimes:     {}",
        winorchestrator_workflow::runtime_summary(&spec.runtimes)
    );
    if let Some(launch) = &spec.launch {
        println!("Launch:       {}", launch.display);
    }
    let policy = resolve_policy(args.keep, args.discard, args.yes);
    let options = RestoreOptions {
        target_override: args.target.clone(),
        offline: args.offline,
        force_download: false,
        setup: !args.no_setup,
        snapshot_out: args.snapshot_out.clone(),
        entrypoint_index: None,
        extra_args: args.args.clone(),
    };
    let restored = winorchestrator_workflow::restore(&spec, &options)?;
    for mismatch in &restored.version_mismatches {
        println!("  ! {mismatch}");
    }
    let mut prepared = restored.prepared;
    print_prepared(&prepared);
    if !args.run {
        let path = args.snapshot_out.clone().unwrap_or_else(|| {
            prepared.snapshot_path(&std::env::current_dir().unwrap_or_default())
        });
        let spec = winorchestrator_snapshot::RestoreSpec::from_record(&prepared.sandbox.record);
        winorchestrator_snapshot::write_snapshot(&spec, &path)?;
        println!(
            "\nSandbox prepared. Run it later with:\n  winorchestrator net-tree --pid <pid>\n  (or re-run this restore with --run)\nRestore file refreshed at {}.",
            path.display()
        );
        return Ok(());
    }
    if let Some(seconds) = args.timeout {
        let outcome = prepared.run_captured(Some(Duration::from_secs(seconds)))?;
        print_outcome(&outcome);
    } else {
        let outcome = run_interactive(&mut prepared)?;
        print_outcome(&outcome);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let snapshot_path = args
        .snapshot_out
        .clone()
        .unwrap_or_else(|| prepared.snapshot_path(&cwd));
    let report = winorchestrator_workflow::finalize(&mut prepared, policy, &snapshot_path, true)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        section("Exit summary");
        println!("  lifecycle:    {}", report.lifecycle);
        if let Some(path) = report.snapshot_path {
            println!("  restore file: {}", path.display());
        }
        if report.sandbox_removed {
            println!(
                "  reclaimed:    {}",
                winorchestrator_core::fsutil::human_bytes(report.disk_reclaimed_bytes)
            );
        }
        for error in report.errors {
            println!("  ! {error}");
        }
    }
    Ok(())
}

fn clean(args: CleanArgs) -> winorchestrator_core::Result<()> {
    if !args.yes {
        let what = if args.sandboxes_only {
            "all sandboxes"
        } else {
            "all sandboxes, cached clones, downloads and logs"
        };
        if !prompt_yes(&format!("Remove {what}?"), false) {
            println!("Cancelled.");
            return Ok(());
        }
    }
    let options = CleanOptions {
        sandboxes: true,
        workspaces: !args.sandboxes_only && !args.no_workspaces,
        downloads: !args.sandboxes_only && !args.no_downloads,
        logs: !args.sandboxes_only && !args.no_logs,
        force: args.force,
        stop_stale: args.stop_stale,
    };
    let report = winorchestrator_workflow::clean(&options)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    section("Clean summary");
    println!(
        "  reclaimed: {}",
        winorchestrator_core::fsutil::human_bytes(report.bytes_reclaimed)
    );
    println!(
        "  sandboxes: {}  workspaces: {}  downloads: {}  logs: {}",
        report.removed_sandboxes.len(),
        report.removed_workspaces.len(),
        report.removed_downloads.len(),
        report.removed_logs.len()
    );
    if !report.stopped_pids.is_empty() {
        println!("  stopped pids: {:?}", report.stopped_pids);
    }
    for skipped in &report.skipped {
        println!("  skipped (no sandbox record): {}", skipped.display());
    }
    for warning in &report.warnings {
        println!("  ! {warning}");
    }
    Ok(())
}

fn doctor(args: DoctorArgs) -> winorchestrator_core::Result<()> {
    let report = winorchestrator_host_probe::probe_detailed(&args.target);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    section("Host runtimes");
    if report.runtimes.is_empty() {
        println!("  (no Python, Node.js, Java, Rust or Go found)");
    }
    for runtime in &report.runtimes {
        println!(
            "  {:<8} {:<12} {:<40} [{}]",
            runtime.runtime.display_name(),
            runtime.version.to_string(),
            runtime.executable.display().to_string(),
            runtime.source
        );
        for (name, path) in &runtime.companions {
            println!("           {name}: {}", path.display());
        }
    }
    section("Package caches (read-only)");
    for cache in &report.caches {
        println!(
            "  {:<8} {} {}",
            cache.name,
            if cache.present { "present" } else { "absent " },
            cache.path.display()
        );
    }
    section("Resources");
    println!(
        "  RAM:  {} total, {} available ({}%)",
        winorchestrator_core::fsutil::human_bytes(report.memory_total_bytes),
        winorchestrator_core::fsutil::human_bytes(report.memory_available_bytes),
        report.memory_load_percent
    );
    println!(
        "  Disk: {} free of {}",
        winorchestrator_core::fsutil::human_bytes(report.disk_free_bytes),
        winorchestrator_core::fsutil::human_bytes(report.disk_total_bytes)
    );
    println!(
        "  Profile: {} job(s){}{}",
        report.profile.parallel_jobs,
        if report.profile.low_footprint {
            ", low-footprint / paging-safe"
        } else {
            ""
        },
        report
            .profile
            .memory_limit_mb
            .map(|mb| format!(", {mb} MiB cap"))
            .unwrap_or_default()
    );
    for process in &report.top_memory_consumers {
        println!(
            "  memory consumer (read-only): {} ({})",
            process.name,
            winorchestrator_host_probe::human_mb(process.working_set_bytes)
        );
    }
    for stale in &report.stale_sandboxes {
        println!(
            "  stale sandbox: {} at {} ({})",
            stale.id,
            stale.path.display(),
            stale.reason
        );
    }
    for advisory in &report.profile.advisory {
        println!("  advisory: {advisory}");
    }
    for note in &report.notes {
        println!("  note: {note}");
    }
    for warning in &report.warnings {
        println!("  warning: {warning}");
    }
    Ok(())
}

fn net_tree(args: NetTreeArgs) -> winorchestrator_core::Result<()> {
    loop {
        let report = if args.pid.is_empty() || args.all {
            winorchestrator_net_tree::collect_all()
        } else {
            winorchestrator_net_tree::collect(&args.pid)
        };
        if args.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!("{}", report.render_tree());
            println!();
            println!("{}", report.render_connections());
            for warning in &report.warnings {
                println!("! {warning}");
            }
        }
        let Some(seconds) = args.watch else {
            return Ok(());
        };
        std::thread::sleep(Duration::from_secs(seconds.max(1)));
        println!("\n{}", "-".repeat(60));
    }
}

fn gui(args: GuiArgs) -> winorchestrator_core::Result<()> {
    winorchestrator_gui::serve(winorchestrator_gui::DashboardOptions {
        bind: args.bind,
        open_browser: !args.no_open,
    })
}

fn list(args: ListArgs) -> winorchestrator_core::Result<()> {
    let records = winorchestrator_sandbox::list_sandboxes();
    if args.json {
        println!("{}", serde_json::to_string_pretty(&records)?);
        return Ok(());
    }
    if records.is_empty() {
        println!("No sandboxes under {}.", paths::sandboxes_dir().display());
        return Ok(());
    }
    println!(
        "{:<42} {:<18} {:<10} {:<12} {}",
        "ID", "REPO", "STATE", "RUNTIMES", "WORKSPACE"
    );
    for record in records {
        println!(
            "{:<42} {:<18} {:<10} {:<12} {}",
            record.id,
            record.repo_name,
            record.lifecycle.as_str(),
            record.runtimes.len(),
            record.workspace.display()
        );
        if record.lifecycle == LifecycleState::Prepared
            || record.lifecycle == LifecycleState::Finished
            || record.lifecycle == LifecycleState::Restored
        {
            println!(
                "  tip: winorchestrator net-tree --pid <sandbox pid>  (source kind: {})",
                match record.source_kind {
                    SourceKind::Remote => "remote",
                    SourceKind::Local => "local",
                }
            );
        }
    }
    Ok(())
}
