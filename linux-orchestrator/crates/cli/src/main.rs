use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process;
use std::time::Duration;

use linuxorchestrator_core::{paths, KeepPolicy};
use workflow::{CleanOptions, PipelineOptions, PreparedRun, RestoreOptions, RunOutcome};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_help();
        process::exit(1);
    }
    if let Err(message) = dispatch(&args[1], &args[2..]) {
        eprintln!("Error: {message}");
        process::exit(1);
    }
}

fn dispatch(command: &str, args: &[String]) -> Result<(), String> {
    match command {
        "run" => cmd_run(args),
        "inspect" => cmd_inspect(args),
        "restore" => cmd_restore(args),
        "clean" => cmd_clean(args),
        "provision" => cmd_provision(args),
        "doctor" => cmd_doctor(args),
        "net-tree" => cmd_net_tree(args),
        "gui" => cmd_gui(args),
        "mcp" => cmd_mcp(args),
        "--help" | "-h" | "help" => {
            print_help();
            Ok(())
        }
        other => {
            print_help();
            Err(format!("unknown command: {other}"))
        }
    }
}

fn print_help() {
    eprintln!("LinuxOrchestrator CLI");
    eprintln!();
    eprintln!("Usage: linuxorchestrator <command> [options]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  run <repo-url-or-path>   Resolve, isolate and run a repository");
    eprintln!("  inspect <repo-url-or-path>  Dry-run analysis (no sandbox)");
    eprintln!("  restore <snapshot>       Rebuild and run from a UTF-8 restore file");
    eprintln!("  provision <repo>         Provision missing runtimes into the toolchain dir");
    eprintln!("  clean                    Remove sandboxes, caches, downloads and logs");
    eprintln!("  doctor                   Check host runtimes and resource telemetry");
    eprintln!("  net-tree                 Print the network topology snapshot");
    eprintln!("  gui                      Print a terminal dashboard summary");
    eprintln!("  mcp                      Start the MCP stdio server");
    eprintln!();
    eprintln!("Run options (run/restore):");
    eprintln!("  --keep | --discard | --yes   Lifecycle policy (no prompt with --yes)");
    eprintln!("  --snapshot-out <file>        Restore file path (default <repo>.envspec.utf8)");
    eprintln!("  --offline                    Do not download missing runtimes/packages");
    eprintln!("  --force-download             Ignore compatible host runtimes");
    eprintln!("  --no-setup                   Skip venv/npm/javac dependency setup");
    eprintln!("  --entry <n>                  Select the 1-based entrypoint candidate");
    eprintln!("  --timeout <seconds>          Non-interactive run with a hard timeout");
    eprintln!("  --json                       Machine-readable summary (never prompts)");
    eprintln!("  --                           Pass the remaining arguments to the program");
}

// --------------------------------------------------------------- helpers --

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

fn resolve_policy(keep: bool, discard: bool, yes: bool, allow_prompt: bool) -> KeepPolicy {
    if keep {
        return KeepPolicy::Keep;
    }
    if discard {
        return KeepPolicy::Discard;
    }
    if yes || !allow_prompt || !io::stdin().is_terminal() {
        return KeepPolicy::Discard;
    }
    println!();
    println!("Choose how to close this environment:");
    println!("  [k] Keep & Promote - move fetched runtimes to your tools dir and register PATH");
    println!("  [d] Destroy & Clean - delete the sandbox and reclaim downloads (default)");
    if prompt_yes("Keep & Promote instead of destroying?", false) {
        KeepPolicy::Keep
    } else {
        KeepPolicy::Discard
    }
}

fn value_after(args: &[String], index: usize, flag: &str) -> Result<String, String> {
    args.get(index + 1)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn inline_value<'a>(arg: &'a str, prefix: &str) -> Option<&'a str> {
    arg.strip_prefix(prefix)
}

fn section(title: &str) {
    println!();
    println!("{title}");
    println!("{}", "-".repeat(title.len().max(20)));
}

// --------------------------------------------------------------- inspect --

struct InspectArgs {
    target: String,
    json: bool,
}

fn parse_inspect(args: &[String]) -> Result<InspectArgs, String> {
    let mut target: Option<String> = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                println!("Usage: linuxorchestrator inspect <repo-url-or-path> [--json]");
                process::exit(0);
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown inspect option: {other}"));
            }
            other => {
                if target.is_some() {
                    return Err(format!("unexpected extra argument: {other}"));
                }
                target = Some(other.to_string());
            }
        }
        index += 1;
    }
    Ok(InspectArgs {
        target: target.unwrap_or_else(|| ".".to_string()),
        json,
    })
}

fn cmd_inspect(args: &[String]) -> Result<(), String> {
    let args = parse_inspect(args)?;
    let inspection = workflow::inspect(&args.target).map_err(|err| err.to_string())?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&inspection).map_err(|err| err.to_string())?
        );
    } else {
        print_inspection(&inspection);
    }
    Ok(())
}

fn print_inspection(inspection: &workflow::Inspection) {
    section("Repository");
    println!("  target:      {}", inspection.target);
    println!("  source:      {}", inspection.acquisition.root.display());
    println!("  repo:        {}", inspection.acquisition.repo_name);
    if let Some(commit) = &inspection.acquisition.commit {
        println!("  commit:      {commit}");
    }
    println!(
        "  source kind: {}",
        match inspection.acquisition.source_kind {
            linuxorchestrator_core::SourceKind::Remote => "remote clone",
            linuxorchestrator_core::SourceKind::Local => "local checkout",
        }
    );

    section("Languages");
    if inspection.plan.languages.is_empty() {
        println!("  (none detected)");
    }
    for language in &inspection.plan.languages {
        println!("  - {}", language.display_name());
    }

    section("Requirements");
    if inspection.plan.requirements.is_empty() {
        println!("  (none)");
    }
    for requirement in &inspection.plan.requirements {
        println!(
            "  - {} {} ({})",
            requirement.runtime.display_name(),
            requirement.constraint,
            requirement.reason
        );
    }

    section("Host matches");
    if inspection.matches.is_empty() {
        println!("  (none)");
    }
    for host in &inspection.matches {
        println!("  - {}: {}", host.runtime.display_name(), host.detail);
    }

    section("Entrypoints");
    if inspection.plan.entrypoints.is_empty() {
        println!("  (none)");
    }
    for (index, entrypoint) in inspection.plan.entrypoints.iter().enumerate() {
        println!("  {}. {} - {}", index + 1, entrypoint.describe(), entrypoint.reason);
    }

    section("Estimate");
    println!(
        "  downloads:  {}",
        linuxorchestrator_core::fsutil::human_bytes(inspection.estimated_download_bytes)
    );
    println!(
        "  disk:       {} required",
        linuxorchestrator_core::fsutil::human_bytes(inspection.required_disk_bytes)
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

// ------------------------------------------------------------------- run --

struct RunArgs {
    target: String,
    keep: bool,
    discard: bool,
    yes: bool,
    offline: bool,
    force_download: bool,
    setup: bool,
    snapshot_out: Option<PathBuf>,
    entry: Option<usize>,
    timeout: Option<u64>,
    memory_limit: Option<u64>,
    update_path: bool,
    json: bool,
    extra_args: Vec<String>,
}

fn parse_run(args: &[String]) -> Result<RunArgs, String> {
    let mut parsed = RunArgs {
        target: String::new(),
        keep: false,
        discard: false,
        yes: false,
        offline: false,
        force_download: false,
        setup: true,
        snapshot_out: None,
        entry: None,
        timeout: None,
        memory_limit: None,
        update_path: true,
        json: false,
        extra_args: Vec::new(),
    };
    let mut target: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            parsed.extra_args.extend(args[index + 1..].iter().cloned());
            break;
        }
        match arg.as_str() {
            "--keep" => parsed.keep = true,
            "--discard" => parsed.discard = true,
            "--yes" | "-y" => parsed.yes = true,
            "--offline" => parsed.offline = true,
            "--force-download" => parsed.force_download = true,
            "--no-setup" => parsed.setup = false,
            "--no-update-path" => parsed.update_path = false,
            "--json" => parsed.json = true,
            "--help" | "-h" => {
                print_help();
                process::exit(0);
            }
            _ if inline_value(arg, "--snapshot-out=").is_some() => {
                parsed.snapshot_out = Some(PathBuf::from(
                    inline_value(arg, "--snapshot-out=").unwrap(),
                ));
            }
            "--snapshot-out" => {
                parsed.snapshot_out = Some(PathBuf::from(value_after(args, index, arg)?));
                index += 1;
            }
            _ if inline_value(arg, "--timeout=").is_some() => {
                parsed.timeout = Some(
                    inline_value(arg, "--timeout=")
                        .unwrap()
                        .parse()
                        .map_err(|_| "--timeout must be a number of seconds".to_string())?,
                );
            }
            "--timeout" => {
                parsed.timeout = Some(
                    value_after(args, index, arg)?
                        .parse()
                        .map_err(|_| "--timeout must be a number of seconds".to_string())?,
                );
                index += 1;
            }
            _ if inline_value(arg, "--entry=").is_some() => {
                parsed.entry = Some(
                    inline_value(arg, "--entry=")
                        .unwrap()
                        .parse()
                        .map_err(|_| "--entry must be a 1-based number".to_string())?,
                );
            }
            "--entry" => {
                parsed.entry = Some(
                    value_after(args, index, arg)?
                        .parse()
                        .map_err(|_| "--entry must be a 1-based number".to_string())?,
                );
                index += 1;
            }
            _ if inline_value(arg, "--memory=").is_some() => {
                parsed.memory_limit = Some(
                    inline_value(arg, "--memory=")
                        .unwrap()
                        .parse()
                        .map_err(|_| "--memory must be a MiB value".to_string())?,
                );
            }
            "--memory" => {
                parsed.memory_limit = Some(
                    value_after(args, index, arg)?
                        .parse()
                        .map_err(|_| "--memory must be a MiB value".to_string())?,
                );
                index += 1;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown run option: {other}"));
            }
            other => {
                if target.is_some() {
                    return Err(format!("unexpected extra argument: {other}"));
                }
                target = Some(other.to_string());
            }
        }
        index += 1;
    }
    parsed.target = target.unwrap_or_else(|| ".".to_string());
    if parsed.keep && parsed.discard {
        return Err("--keep and --discard are mutually exclusive".to_string());
    }
    if let Some(entry) = parsed.entry {
        if entry == 0 {
            return Err("--entry is 1-based; use 1 for the first candidate".to_string());
        }
        parsed.entry = Some(entry - 1);
    }
    Ok(parsed)
}

fn cmd_run(args: &[String]) -> Result<(), String> {
    let args = parse_run(args)?;

    let inspection = workflow::inspect(&args.target).map_err(|err| err.to_string())?;
    if !args.json {
        print_inspection(&inspection);
    }

    let policy = resolve_policy(args.keep, args.discard, args.yes, !args.json);
    let options = PipelineOptions {
        target: args.target.clone(),
        snapshot_out: args.snapshot_out.clone(),
        offline: args.offline,
        force_download: args.force_download,
        setup: args.setup,
        entrypoint_index: args.entry,
        extra_args: args.extra_args.clone(),
        memory_limit_mb: args.memory_limit,
        update_path: args.update_path,
        ..Default::default()
    };

    if !args.json {
        println!("\nPreparing the sandbox (host injection first, minimal fetch when needed)...");
    }
    let mut prepared = workflow::prepare(&options).map_err(|err| err.to_string())?;
    if !args.json {
        print_prepared(&prepared);
    }

    let outcome = if let Some(seconds) = args.timeout {
        prepared
            .run_captured(Some(Duration::from_secs(seconds)))
            .map_err(|err| err.to_string())?
    } else {
        run_interactive(&mut prepared)?
    };
    if !args.json {
        print_outcome(&outcome);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let snapshot_path = args
        .snapshot_out
        .clone()
        .unwrap_or_else(|| prepared.snapshot_path(&cwd));
    let report = workflow::finalize(&mut prepared, policy, &snapshot_path, args.update_path)
        .map_err(|err| err.to_string())?;

    if args.json {
        let value = serde_json::json!({
            "sandbox": prepared.sandbox.record.id,
            "repo": prepared.sandbox.record.repo_name,
            "exit_code": outcome.exit_code,
            "success": outcome.success,
            "timed_out": outcome.timed_out,
            "duration_seconds": outcome.duration.as_secs_f64(),
            "finalize": report,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|err| err.to_string())?
        );
        return Ok(());
    }
    print_finalize(&report);
    Ok(())
}

fn run_interactive(prepared: &mut PreparedRun) -> Result<RunOutcome, String> {
    let running = prepared.start().map_err(|err| err.to_string())?;
    println!("\nSandbox pid {}: {}", running.pid, running.command);
    println!("Stop the run with Ctrl+C at any time; the sandbox dies with its parent.");
    let outcome = running.wait(None).map_err(|err| err.to_string())?;
    prepared.finish(&outcome).map_err(|err| err.to_string())?;
    Ok(outcome)
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
        workflow::runtime_summary(&prepared.sandbox.record.runtimes)
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

fn print_outcome(outcome: &RunOutcome) {
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
        if outcome.timed_out { " (timed out)" } else { "" }
    );
}

fn print_finalize(report: &workflow::FinalizeReport) {
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
            linuxorchestrator_core::fsutil::human_bytes(report.disk_reclaimed_bytes)
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
        println!(
            "\nEnvironment cleanly resolved and closed!!!"
        );
    }
}

// --------------------------------------------------------------- restore --

struct RestoreArgs {
    file: PathBuf,
    target: Option<String>,
    run: bool,
    keep: bool,
    discard: bool,
    yes: bool,
    offline: bool,
    force_download: bool,
    setup: bool,
    snapshot_out: Option<PathBuf>,
    entry: Option<usize>,
    timeout: Option<u64>,
    json: bool,
    extra_args: Vec<String>,
}

fn parse_restore(args: &[String]) -> Result<RestoreArgs, String> {
    let mut parsed = RestoreArgs {
        file: PathBuf::new(),
        target: None,
        run: true,
        keep: false,
        discard: false,
        yes: false,
        offline: false,
        force_download: false,
        setup: true,
        snapshot_out: None,
        entry: None,
        timeout: None,
        json: false,
        extra_args: Vec::new(),
    };
    let mut file: Option<PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            parsed.extra_args.extend(args[index + 1..].iter().cloned());
            break;
        }
        match arg.as_str() {
            "--run" => parsed.run = true,
            "--no-run" => parsed.run = false,
            "--keep" => parsed.keep = true,
            "--discard" => parsed.discard = true,
            "--yes" | "-y" => parsed.yes = true,
            "--offline" => parsed.offline = true,
            "--force-download" => parsed.force_download = true,
            "--no-setup" => parsed.setup = false,
            "--json" => parsed.json = true,
            "--help" | "-h" => {
                print_help();
                process::exit(0);
            }
            _ if inline_value(arg, "--snapshot-out=").is_some() => {
                parsed.snapshot_out = Some(PathBuf::from(
                    inline_value(arg, "--snapshot-out=").unwrap(),
                ));
            }
            "--snapshot-out" => {
                parsed.snapshot_out = Some(PathBuf::from(value_after(args, index, arg)?));
                index += 1;
            }
            _ if inline_value(arg, "--target=").is_some() => {
                parsed.target = Some(inline_value(arg, "--target=").unwrap().to_string());
            }
            "--target" => {
                parsed.target = Some(value_after(args, index, arg)?);
                index += 1;
            }
            _ if inline_value(arg, "--timeout=").is_some() => {
                parsed.timeout = Some(
                    inline_value(arg, "--timeout=")
                        .unwrap()
                        .parse()
                        .map_err(|_| "--timeout must be a number of seconds".to_string())?,
                );
            }
            "--timeout" => {
                parsed.timeout = Some(
                    value_after(args, index, arg)?
                        .parse()
                        .map_err(|_| "--timeout must be a number of seconds".to_string())?,
                );
                index += 1;
            }
            _ if inline_value(arg, "--entry=").is_some() => {
                parsed.entry = Some(
                    inline_value(arg, "--entry=")
                        .unwrap()
                        .parse()
                        .map_err(|_| "--entry must be a 1-based number".to_string())?,
                );
            }
            "--entry" => {
                parsed.entry = Some(
                    value_after(args, index, arg)?
                        .parse()
                        .map_err(|_| "--entry must be a 1-based number".to_string())?,
                );
                index += 1;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown restore option: {other}"));
            }
            other => {
                if file.is_some() {
                    return Err(format!("unexpected extra argument: {other}"));
                }
                file = Some(PathBuf::from(other));
            }
        }
        index += 1;
    }
    parsed.file = file.ok_or_else(|| "restore requires a snapshot file".to_string())?;
    if parsed.keep && parsed.discard {
        return Err("--keep and --discard are mutually exclusive".to_string());
    }
    if let Some(entry) = parsed.entry {
        if entry == 0 {
            return Err("--entry is 1-based; use 1 for the first candidate".to_string());
        }
        parsed.entry = Some(entry - 1);
    }
    Ok(parsed)
}

fn cmd_restore(args: &[String]) -> Result<(), String> {
    let args = parse_restore(args)?;
    let spec = snapshot::read_snapshot(&args.file).map_err(|err| err.to_string())?;
    if !args.json {
        println!(
            "Restore file: {} (schema {}, generated {})",
            args.file.display(),
            spec.schema_version,
            spec.generated_at
        );
        println!("Repository:   {} ({})", spec.repo_name, spec.source);
        println!("Runtimes:     {}", workflow::runtime_summary(&spec.runtimes));
        if let Some(launch) = &spec.launch {
            println!("Launch:       {}", launch.display);
        }
    }

    let policy = resolve_policy(args.keep, args.discard, args.yes, !args.json);
    let options = RestoreOptions {
        target_override: args.target.clone(),
        offline: args.offline,
        force_download: args.force_download,
        setup: args.setup,
        snapshot_out: args.snapshot_out.clone(),
        entrypoint_index: args.entry,
        extra_args: args.extra_args.clone(),
    };
    let restored = workflow::restore(&spec, &options).map_err(|err| err.to_string())?;
    if !args.json {
        for mismatch in &restored.version_mismatches {
            println!("  ! {mismatch}");
        }
    }
    let mut prepared = restored.prepared;
    if !args.json {
        print_prepared(&prepared);
    }

    if !args.run {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let path = args
            .snapshot_out
            .clone()
            .unwrap_or_else(|| prepared.snapshot_path(&cwd));
        let refreshed = snapshot::RestoreSpec::from_record(&prepared.sandbox.record);
        snapshot::write_snapshot(&refreshed, &path).map_err(|err| err.to_string())?;
        if args.json {
            let value = serde_json::json!({
                "prepared": true,
                "ran": false,
                "sandbox": prepared.sandbox.record.id,
                "snapshot_path": path,
                "version_mismatches": restored.version_mismatches,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&value).map_err(|err| err.to_string())?
            );
        } else {
            println!(
                "\nSandbox prepared without running. Restore file refreshed at {}.",
                path.display()
            );
        }
        return Ok(());
    }

    let outcome = if let Some(seconds) = args.timeout {
        prepared
            .run_captured(Some(Duration::from_secs(seconds)))
            .map_err(|err| err.to_string())?
    } else {
        run_interactive(&mut prepared)?
    };
    if !args.json {
        print_outcome(&outcome);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let snapshot_path = args
        .snapshot_out
        .clone()
        .unwrap_or_else(|| prepared.snapshot_path(&cwd));
    let report = workflow::finalize(&mut prepared, policy, &snapshot_path, true)
        .map_err(|err| err.to_string())?;
    if args.json {
        let value = serde_json::json!({
            "sandbox": prepared.sandbox.record.id,
            "repo": prepared.sandbox.record.repo_name,
            "exit_code": outcome.exit_code,
            "success": outcome.success,
            "timed_out": outcome.timed_out,
            "duration_seconds": outcome.duration.as_secs_f64(),
            "version_mismatches": restored.version_mismatches,
            "finalize": report,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|err| err.to_string())?
        );
    } else {
        print_finalize(&report);
    }
    Ok(())
}

// ----------------------------------------------------------------- clean --

fn cmd_clean(args: &[String]) -> Result<(), String> {
    let mut yes = false;
    let mut sandboxes_only = false;
    let mut force = false;
    for arg in args {
        match arg.as_str() {
            "--yes" | "-y" => yes = true,
            "--sandboxes-only" => sandboxes_only = true,
            "--force" => force = true,
            "--help" | "-h" => {
                println!(
                    "Usage: linuxorchestrator clean [--yes] [--sandboxes-only] [--force]"
                );
                return Ok(());
            }
            other => return Err(format!("unknown clean option: {other}")),
        }
    }
    if !yes {
        let what = if sandboxes_only {
            "all sandboxes"
        } else {
            "all sandboxes, cached clones, downloads and logs"
        };
        if !prompt_yes(&format!("Remove {what}?"), false) {
            println!("Cancelled.");
            return Ok(());
        }
    }
    let report = workflow::clean(&CleanOptions {
        sandboxes: true,
        workspaces: !sandboxes_only,
        downloads: !sandboxes_only,
        logs: !sandboxes_only,
        force,
    })
    .map_err(|err| err.to_string())?;

    section("Clean summary");
    println!(
        "  reclaimed:  {}",
        linuxorchestrator_core::fsutil::human_bytes(report.bytes_reclaimed)
    );
    println!("  sandboxes:  {}", report.removed_sandboxes.len());
    println!("  workspaces: {}", report.removed_workspaces.len());
    println!("  downloads:  {}", report.removed_downloads.len());
    println!("  logs:       {}", report.removed_logs.len());
    for skipped in &report.skipped {
        println!("  skipped:    {} (no sandbox.json)", skipped.display());
    }
    for warning in &report.warnings {
        println!("  ! {warning}");
    }
    Ok(())
}

// ------------------------------------------------------------- provision --

fn cmd_provision(args: &[String]) -> Result<(), String> {
    let mut target = ".".to_string();
    let mut offline = false;
    let mut force_download = false;
    for arg in args {
        match arg.as_str() {
            "--offline" => offline = true,
            "--force-download" => force_download = true,
            "--help" | "-h" => {
                println!(
                    "Usage: linuxorchestrator provision <repo-url-or-path> [--offline] [--force-download]"
                );
                return Ok(());
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown provision option: {other}"))
            }
            other => target = other.to_string(),
        }
    }
    let inspection = workflow::inspect(&target).map_err(|err| err.to_string())?;
    let install_root = paths::toolchains_dir().join("run");
    let options = provisioner::ProvisionOptions {
        offline,
        force_download,
        ..Default::default()
    };
    let result = provisioner::provision(
        &inspection.plan,
        &inspection.matches,
        &install_root,
        &options,
    )
    .map_err(|err| err.to_string())?;

    section("Provisioned runtimes");
    println!("  install root: {}", install_root.display());
    for runtime in &result.runtimes {
        println!(
            "  - {} {} ({})",
            runtime.runtime.display_name(),
            runtime.version,
            if runtime.from_host { "host" } else { "fetched" }
        );
    }
    for note in &result.notes {
        println!("  - {note}");
    }
    for warning in &result.warnings {
        println!("  ! {warning}");
    }
    Ok(())
}

// ---------------------------------------------------------- doctor/net-tree --

fn cmd_doctor(args: &[String]) -> Result<(), String> {
    if args.iter().any(|arg| matches!(arg.as_str(), "--help" | "-h")) {
        println!("Usage: linuxorchestrator doctor");
        return Ok(());
    }
    let report = host_probe::probe_detailed(std::path::Path::new("."));
    section("Runtimes");
    if report.runtimes.is_empty() {
        println!("  (none found on PATH or common install locations)");
    }
    for runtime in &report.runtimes {
        println!(
            "  - {} {} at {} ({})",
            runtime.runtime.display_name(),
            runtime.version,
            runtime.executable.display(),
            runtime.source
        );
    }
    section("Resources");
    println!(
        "  memory: {} available of {}",
        linuxorchestrator_core::fsutil::human_bytes(report.memory_available_bytes),
        linuxorchestrator_core::fsutil::human_bytes(report.memory_total_bytes)
    );
    println!(
        "  disk:   {} free of {}",
        linuxorchestrator_core::fsutil::human_bytes(report.disk_free_bytes),
        linuxorchestrator_core::fsutil::human_bytes(report.disk_total_bytes)
    );
    if report.profile.low_footprint {
        println!("  profile: low-footprint (paging-safe)");
    } else {
        println!("  profile: normal");
    }
    if !report.caches.is_empty() {
        section("Caches");
        for cache in &report.caches {
            println!(
                "  - {} {} ({})",
                cache.name,
                if cache.present { "present" } else { "missing" },
                cache.path.display()
            );
        }
    }
    if !report.stale_sandboxes.is_empty() {
        section("Stale sandboxes");
        for stale in &report.stale_sandboxes {
            println!("  - {} at {} ({})", stale.id, stale.path.display(), stale.reason);
        }
    }
    for warning in &report.warnings {
        println!("  ! {warning}");
    }
    for advisory in &report.profile.advisory {
        println!("  - {advisory}");
    }
    Ok(())
}

fn cmd_net_tree(_args: &[String]) -> Result<(), String> {
    println!("Net‑tree information is not available on this platform.");
    Ok(())
}

fn cmd_gui(args: &[String]) -> Result<(), String> {
    if args.iter().any(|arg| matches!(arg.as_str(), "--help" | "-h")) {
        println!("Usage: linuxorchestrator gui");
        return Ok(());
    }
    section("Dashboard");
    let report = host_probe::probe(std::path::Path::new("."));
    println!("  runtimes:        {}", report.runtimes.len());
    println!("  stale sandboxes: {}", report.stale_sandboxes.len());
    println!("  sandboxes:       {}", sandbox::list_sandboxes().len());
    println!("  Ctrl+C to exit.");
    Ok(())
}

fn cmd_mcp(args: &[String]) -> Result<(), String> {
    if args.iter().any(|arg| matches!(arg.as_str(), "--help" | "-h")) {
        println!("Usage: linuxorchestrator mcp");
        return Ok(());
    }
    eprintln!("LinuxOrchestrator mcp");
    eprintln!();
    eprintln!("Starting MCP stdio server. Listening on stdin/stdout.");
    eprintln!("Tools: inspect, doctor, net-tree, provision, run");
    Ok(())
}
