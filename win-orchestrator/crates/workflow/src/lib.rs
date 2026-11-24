use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use winorchestrator_core::fsutil;
use winorchestrator_core::{
    paths, DependencyOrigin, Error, HostMatch, KeepPolicy, LifecycleState, ResolutionPlan, Result,
    RuntimeKind, RuntimeRecord, SandboxRecord, SourceKind,
};
use winorchestrator_provisioner::{ProvisionOptions, ProvisionResult, ProvisionedRuntime};
use winorchestrator_sandbox::{
    PreparedSandbox, RunOutcome, RunningProcess, SandboxEnvironment, SandboxOptions, SetupMode,
};
use winorchestrator_snapshot::{default_snapshot_path, RestoreSpec};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AcquisitionInfo {
    pub root: PathBuf,
    pub repo_name: String,
    pub display_target: String,
    pub source_kind: SourceKind,
    pub commit: Option<String>,
    pub cache_hit: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Inspection {
    pub target: String,
    pub acquisition: AcquisitionInfo,
    pub plan: ResolutionPlan,
    pub matches: Vec<HostMatch>,
    pub probe: winorchestrator_host_probe::ProbeReport,
    pub estimated_download_bytes: u64,
    pub required_disk_bytes: u64,
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PipelineOptions {
    pub target: String,
    pub id: Option<String>,
    pub base_dir: Option<PathBuf>,
    pub offline: bool,
    pub force_download: bool,
    pub setup: bool,
    pub write_windows_sandbox: bool,
    pub entrypoint_index: Option<usize>,
    pub extra_args: Vec<String>,
    pub snapshot_out: Option<PathBuf>,
    pub memory_limit_mb: Option<u64>,
    pub update_path: bool,
}

impl Default for PipelineOptions {
    fn default() -> Self {
        Self {
            target: String::new(),
            id: None,
            base_dir: None,
            offline: false,
            force_download: false,
            setup: true,
            write_windows_sandbox: false,
            entrypoint_index: None,
            extra_args: Vec::new(),
            snapshot_out: None,
            memory_limit_mb: None,
            update_path: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PreparedRun {
    pub inspection: Inspection,
    pub sandbox: PreparedSandbox,
    pub provision: ProvisionResult,
    pub source_root: PathBuf,
    pub source_is_cached_clone: bool,
    pub options: PipelineOptions,
}

pub fn inspect(target: &str) -> Result<Inspection> {
    let acquisition = winorchestrator_git_acquire::acquire(
        target,
        &winorchestrator_git_acquire::AcquireOptions::default(),
    )?;
    let plan = winorchestrator_analyzer::analyze(&acquisition.root, &acquisition.repo_name)?;
    let probe = winorchestrator_host_probe::probe(&acquisition.root);
    let matches = winorchestrator_host_probe::match_plan(&plan, &probe);
    let estimated = winorchestrator_provisioner::estimate_download_bytes(&matches);
    let source_bytes = fsutil::directory_size(&acquisition.root);
    let required_disk_bytes = estimated
        .saturating_add(source_bytes)
        .saturating_add(256 * 1024 * 1024);

    let mut notes = vec![
        format!(
            "Source: {} ({}).",
            acquisition.root.display(),
            match acquisition.source {
                winorchestrator_git_acquire::SourceKind::Remote => "shallow clone",
                winorchestrator_git_acquire::SourceKind::Local => "local checkout, read-only",
            }
        ),
        format!(
            "Detected {} language(s), {} requirement(s), {} entrypoint(s).",
            plan.languages.len(),
            plan.requirements.len(),
            plan.entrypoints.len()
        ),
    ];
    if acquisition.cache_hit {
        notes.push("Reused the cached shallow clone.".to_string());
    }
    let mut warnings = plan.warnings.clone();
    warnings.extend(probe.warnings.clone());
    warnings.extend(acquisition.warnings.clone());
    for stale in &probe.stale_sandboxes {
        warnings.push(format!(
            "Stale sandbox {} left at {} ({}). Run `clean` to reclaim it.",
            stale.id,
            stale.path.display(),
            stale.reason
        ));
    }
    if probe.disk_free_bytes > 0 && probe.disk_free_bytes < required_disk_bytes {
        warnings.push(format!(
            "Disk space is tight: {:.1} GiB free, about {:.1} GiB estimated for downloads plus the staged copy. Free space or use --offline after caching.",
            probe.disk_free_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
            required_disk_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
        ));
    }

    Ok(build_inspection(
        target,
        acquisition,
        plan,
        probe,
        matches,
        estimated,
        required_disk_bytes,
        notes,
        warnings,
    ))
}

#[allow(clippy::too_many_arguments)]
fn build_inspection(
    target: &str,
    acquisition: winorchestrator_git_acquire::Acquisition,
    plan: ResolutionPlan,
    probe: winorchestrator_host_probe::ProbeReport,
    matches: Vec<HostMatch>,
    estimated: u64,
    required_disk_bytes: u64,
    notes: Vec<String>,
    warnings: Vec<String>,
) -> Inspection {
    Inspection {
        target: target.to_string(),
        acquisition: AcquisitionInfo {
            root: acquisition.root,
            repo_name: acquisition.repo_name,
            display_target: acquisition.display_target,
            source_kind: match acquisition.source {
                winorchestrator_git_acquire::SourceKind::Remote => SourceKind::Remote,
                winorchestrator_git_acquire::SourceKind::Local => SourceKind::Local,
            },
            commit: acquisition.commit,
            cache_hit: acquisition.cache_hit,
        },
        plan,
        matches,
        probe,
        estimated_download_bytes: estimated,
        required_disk_bytes,
        notes,
        warnings,
    }
}

fn runtime_record(runtime: &ProvisionedRuntime) -> RuntimeRecord {
    RuntimeRecord {
        runtime: runtime.runtime,
        version: runtime.version,
        executable: runtime.executable.clone(),
        home: runtime.home.clone(),
        from_host: runtime.from_host,
        source: runtime.source.clone(),
        companions: runtime.companions.clone(),
    }
}

fn provisioned_runtime(runtime: &RuntimeRecord) -> ProvisionedRuntime {
    ProvisionedRuntime {
        runtime: runtime.runtime,
        version: runtime.version,
        home: runtime.home.clone(),
        executable: runtime.executable.clone(),
        companions: runtime.companions.clone(),
        from_host: runtime.from_host,
        source: runtime.source.clone(),
    }
}

pub fn prepare(options: &PipelineOptions) -> Result<PreparedRun> {
    if options.target.trim().is_empty() {
        return Err(Error::Message(
            "no repository URL or path was provided".to_string(),
        ));
    }
    let inspection = inspect(&options.target)?;

    if inspection.probe.disk_free_bytes > 0
        && inspection.probe.disk_free_bytes
            < inspection
                .estimated_download_bytes
                .saturating_add(64 * 1024 * 1024)
    {
        return Err(Error::Message(format!(
            "not enough free disk space on the target volume: {} free, at least {} required for the minimal runtime downloads. Free space and retry.",
            fsutil::human_bytes(inspection.probe.disk_free_bytes),
            fsutil::human_bytes(
                inspection
                    .estimated_download_bytes
                    .saturating_add(64 * 1024 * 1024)
            )
        )));
    }

    let plan = &inspection.plan;
    let id = options
        .id
        .clone()
        .unwrap_or_else(|| paths::unique_sandbox_id(&plan.repo_name));
    let base = options
        .base_dir
        .clone()
        .unwrap_or_else(paths::sandboxes_dir);
    let install_root = base.join(&id).join("runtimes");
    let provision_options = ProvisionOptions {
        offline: options.offline,
        force_download: options.force_download,
        ..Default::default()
    };
    let provision = winorchestrator_provisioner::provision(
        plan,
        &inspection.matches,
        &install_root,
        &provision_options,
    )?;

    let records: Vec<RuntimeRecord> = provision.runtimes.iter().map(runtime_record).collect();
    let mut profile = inspection.probe.profile.clone();
    if let Some(limit) = options.memory_limit_mb {
        profile.memory_limit_mb = Some(limit.clamp(256, 32768));
    }
    let mut sandbox_options = SandboxOptions {
        base_dir: Some(base),
        id: Some(id.clone()),
        setup: if options.setup {
            SetupMode::Auto
        } else {
            SetupMode::Skip
        },
        offline: options.offline,
        max_file_bytes: 512 * 1024 * 1024,
        memory_limit_mb: profile.memory_limit_mb,
        entrypoint_index: options.entrypoint_index,
        extra_args: options.extra_args.clone(),
        write_windows_sandbox: options.write_windows_sandbox,
    };
    // The sandbox id already has the install root prepared.
    let source_kind = inspection.acquisition.source_kind;
    let mut sandbox = winorchestrator_sandbox::prepare(
        &inspection.acquisition.root,
        &inspection.acquisition.display_target,
        source_kind,
        plan,
        &records,
        &profile,
        &sandbox_options,
    )?;
    sandbox.record.notes.extend(provision.notes.clone());
    sandbox.record.warnings.extend(provision.warnings.clone());
    sandbox.record.notes.push(format!(
        "Host profile: {} parallel job(s){}{}.",
        profile.parallel_jobs,
        if profile.low_footprint {
            ", low-footprint mode"
        } else {
            ""
        },
        profile
            .memory_limit_mb
            .map(|mb| format!(", {mb} MiB sandbox cap"))
            .unwrap_or_default()
    ));
    for advisory in &profile.advisory {
        sandbox.record.notes.push(advisory.clone());
    }
    sandbox.write_record()?;
    let _ = &mut sandbox_options;

    Ok(PreparedRun {
        inspection,
        sandbox,
        provision,
        source_root: String::new().into(),
        source_is_cached_clone: false,
        options: options.clone(),
    })
}

impl PreparedRun {
    pub fn snapshot_path(&self, cwd: &Path) -> PathBuf {
        self.options
            .snapshot_out
            .clone()
            .unwrap_or_else(|| default_snapshot_path(&self.inspection.acquisition.repo_name, cwd))
    }

    /// Spawn the project attached to the current console.
    pub fn start(&mut self) -> Result<RunningProcess> {
        self.sandbox.set_lifecycle(LifecycleState::Running)?;
        self.sandbox.spawn_interactive(&self.options.extra_args)
    }

    pub fn finish(&mut self, outcome: &RunOutcome) -> Result<()> {
        let lifecycle = if outcome.success {
            LifecycleState::Finished
        } else {
            LifecycleState::Failed
        };
        self.sandbox.set_lifecycle(lifecycle)
    }

    /// Non-interactive execution used by MCP and the GUI.
    pub fn run_captured(&mut self, timeout: Option<Duration>) -> Result<RunOutcome> {
        self.sandbox.set_lifecycle(LifecycleState::Running)?;
        let outcome = self
            .sandbox
            .run_capture(&self.options.extra_args, timeout)?;
        self.finish(&outcome)?;
        Ok(outcome)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FinalizeReport {
    pub lifecycle: String,
    pub snapshot_path: Option<PathBuf>,
    pub promoted: Vec<PathBuf>,
    pub path_updated: bool,
    pub path_backup: Option<PathBuf>,
    pub sandbox_removed: bool,
    pub disk_reclaimed_bytes: u64,
    pub removed_artifacts: Vec<PathBuf>,
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

/// Write the mandatory restore file, then apply Keep (promote + PATH) or
/// Discard (destroy + reclaim). The snapshot is written before teardown so it
/// survives even when the sandbox is deleted.
pub fn finalize(
    prepared: &mut PreparedRun,
    policy: KeepPolicy,
    snapshot_path: &Path,
    update_path: bool,
) -> Result<FinalizeReport> {
    if policy == KeepPolicy::Ask {
        return Err(Error::Message(
            "KeepPolicy::Ask must be resolved by the caller before finalize".to_string(),
        ));
    }
    let mut report = FinalizeReport::default();
    let root = prepared.sandbox.layout.root.clone();
    let size_before = fsutil::directory_size(&root);

    if policy == KeepPolicy::Keep {
        let runtimes: Vec<ProvisionedRuntime> = prepared
            .sandbox
            .record
            .runtimes
            .iter()
            .map(provisioned_runtime)
            .collect();
        match winorchestrator_provisioner::promote(&runtimes, &paths::toolchains_dir(), update_path)
        {
            Ok(promotion) => {
                report.promoted = promotion.promoted.clone();
                report.path_updated = promotion.path_updated;
                report.path_backup = promotion.backup.clone();
                report.notes.extend(promotion.notes.clone());
                // Rewrite fetched runtime locations to the promoted toolchain.
                for runtime in &mut prepared.sandbox.record.runtimes {
                    if runtime.from_host {
                        continue;
                    }
                    let target = paths::toolchains_dir().join(format!(
                        "{}-{}",
                        runtime.runtime.as_str(),
                        runtime.version
                    ));
                    let relative = runtime
                        .executable
                        .strip_prefix(&runtime.home)
                        .map(|path| path.to_path_buf())
                        .unwrap_or_else(|_| {
                            PathBuf::from(runtime.executable.file_name().unwrap_or_default())
                        });
                    runtime.home = target.clone();
                    runtime.executable = target.join(relative);
                    runtime
                        .source
                        .push_str(" (promoted to the user toolchain directory)");
                }
                prepared.sandbox.record.environment = SandboxEnvironment::build(
                    &prepared.sandbox.layout,
                    &prepared.sandbox.record.runtimes,
                    &prepared.sandbox.record.profile,
                )
                .vars;
                prepared.sandbox.set_lifecycle(LifecycleState::Kept)?;
            }
            Err(err) => {
                report.errors.push(format!(
                    "Keep & Promote failed; the sandbox was left in place for recovery: {err}"
                ));
                prepared.sandbox.set_lifecycle(LifecycleState::Failed)?;
            }
        }
    } else {
        prepared.sandbox.set_lifecycle(LifecycleState::Discarded)?;
    }

    let spec = RestoreSpec::from_record(&prepared.sandbox.record);
    winorchestrator_snapshot::write_snapshot(&spec, snapshot_path)?;
    report.snapshot_path = Some(snapshot_path.to_path_buf());
    report.notes.push(format!(
        "Restore file written to {} (strict UTF-8).",
        snapshot_path.display()
    ));

    if report.errors.is_empty() {
        winorchestrator_sandbox::destroy(&root, false)?;
        report.sandbox_removed = true;
        report.disk_reclaimed_bytes = size_before;
        report.notes.push(format!(
            "Sandbox {} removed; {} reclaimed.",
            prepared.sandbox.record.id,
            fsutil::human_bytes(size_before)
        ));

        if policy == KeepPolicy::Discard {
            for download in &prepared.provision.downloads {
                if download.file.is_file() {
                    if fsutil::remove_tree(&download.file).is_ok() {
                        report.removed_artifacts.push(download.file.clone());
                    }
                }
            }
            if prepared.inspection.acquisition.source_kind == SourceKind::Remote
                && paths::is_within(
                    &paths::workspaces_dir(),
                    &prepared.inspection.acquisition.root,
                )
            {
                if fsutil::remove_tree(&prepared.inspection.acquisition.root).is_ok() {
                    report
                        .removed_artifacts
                        .push(prepared.inspection.acquisition.root.clone());
                }
            }
            if !report.removed_artifacts.is_empty() {
                report.notes.push(format!(
                    "Discarded {} cached artifact(s) owned by this run.",
                    report.removed_artifacts.len()
                ));
            }
        }
    } else {
        report.warnings.push(
            "Promotion errors prevented teardown; inspect the sandbox and re-run with --discard to clean it."
                .to_string(),
        );
    }

    report.lifecycle = policy_name(policy).to_string();
    Ok(report)
}

fn policy_name(policy: KeepPolicy) -> &'static str {
    match policy {
        KeepPolicy::Keep => "keep",
        KeepPolicy::Discard => "discard",
        KeepPolicy::Ask => "ask",
    }
}

/// Close a sandbox that was prepared by an earlier process (for example from
/// the GUI). Promotes runtimes on Keep, always writes the restore file, and
/// removes the sandbox only when promotion succeeds.
pub fn finalize_existing(
    root: &Path,
    policy: KeepPolicy,
    snapshot_path: &Path,
    update_path: bool,
) -> Result<FinalizeReport> {
    if policy == KeepPolicy::Ask {
        return Err(Error::Message(
            "KeepPolicy::Ask must be resolved by the caller".to_string(),
        ));
    }
    let mut prepared = winorchestrator_sandbox::open_sandbox(root)?;
    let size_before = fsutil::directory_size(&prepared.layout.root);
    let mut report = FinalizeReport::default();

    if policy == KeepPolicy::Keep {
        let runtimes: Vec<ProvisionedRuntime> = prepared
            .record
            .runtimes
            .iter()
            .map(provisioned_runtime)
            .collect();
        match winorchestrator_provisioner::promote(&runtimes, &paths::toolchains_dir(), update_path)
        {
            Ok(promotion) => {
                report.promoted = promotion.promoted.clone();
                report.path_updated = promotion.path_updated;
                report.path_backup = promotion.backup.clone();
                report.notes.extend(promotion.notes.clone());
                for runtime in &mut prepared.record.runtimes {
                    if runtime.from_host {
                        continue;
                    }
                    let target = paths::toolchains_dir().join(format!(
                        "{}-{}",
                        runtime.runtime.as_str(),
                        runtime.version
                    ));
                    let relative = runtime
                        .executable
                        .strip_prefix(&runtime.home)
                        .map(|path| path.to_path_buf())
                        .unwrap_or_else(|_| {
                            PathBuf::from(runtime.executable.file_name().unwrap_or_default())
                        });
                    runtime.home = target.clone();
                    runtime.executable = target.join(relative);
                }
                prepared.record.environment = SandboxEnvironment::build(
                    &prepared.layout,
                    &prepared.record.runtimes,
                    &prepared.record.profile,
                )
                .vars;
                prepared.set_lifecycle(LifecycleState::Kept)?;
            }
            Err(err) => {
                report.errors.push(format!(
                    "Keep & Promote failed; the sandbox was left in place: {err}"
                ));
                prepared.set_lifecycle(LifecycleState::Failed)?;
            }
        }
    } else {
        prepared.set_lifecycle(LifecycleState::Discarded)?;
    }

    let spec = RestoreSpec::from_record(&prepared.record);
    winorchestrator_snapshot::write_snapshot(&spec, snapshot_path)?;
    report.snapshot_path = Some(snapshot_path.to_path_buf());
    report.notes.push(format!(
        "Restore file written to {}.",
        snapshot_path.display()
    ));

    if report.errors.is_empty() {
        winorchestrator_sandbox::destroy(&prepared.layout.root, false)?;
        report.sandbox_removed = true;
        report.disk_reclaimed_bytes = size_before;
    }
    report.lifecycle = policy_name(policy).to_string();
    Ok(report)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RestoredRun {
    pub version_mismatches: Vec<String>,
    pub prepared: PreparedRunReport,
    pub spec: RestoreSpec,
}

/// Serializable summary; the live `PreparedRun` is returned by `restore`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparedRunReport {
    pub id: String,
    pub root: PathBuf,
    pub workspace: PathBuf,
    pub launch: Option<String>,
}

pub struct RestoreResult {
    pub prepared: PreparedRun,
    pub spec: RestoreSpec,
    pub version_mismatches: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct RestoreOptions {
    pub target_override: Option<String>,
    pub offline: bool,
    pub force_download: bool,
    pub setup: bool,
    pub snapshot_out: Option<PathBuf>,
    pub entrypoint_index: Option<usize>,
    pub extra_args: Vec<String>,
}

impl Default for RestoreOptions {
    fn default() -> Self {
        Self {
            target_override: None,
            offline: false,
            force_download: false,
            setup: true,
            snapshot_out: None,
            entrypoint_index: None,
            extra_args: Vec::new(),
        }
    }
}

pub fn restore(spec: &RestoreSpec, options: &RestoreOptions) -> Result<RestoreResult> {
    spec.validate()?;
    let source = options
        .target_override
        .clone()
        .unwrap_or_else(|| spec.source.clone());
    let acquisition = winorchestrator_git_acquire::acquire(
        &source,
        &winorchestrator_git_acquire::AcquireOptions::default(),
    )?;
    let plan = spec.to_plan();
    let probe = winorchestrator_host_probe::probe(&acquisition.root);
    let matches = winorchestrator_host_probe::match_plan(&plan, &probe);
    let id = paths::unique_sandbox_id(&plan.repo_name);
    let base = paths::sandboxes_dir();
    let install_root = base.join(&id).join("runtimes");
    let provision_options = ProvisionOptions {
        offline: options.offline,
        force_download: options.force_download,
        ..Default::default()
    };
    let provision =
        winorchestrator_provisioner::provision(&plan, &matches, &install_root, &provision_options)?;
    let mut mismatches = Vec::new();
    for expected in &spec.runtimes {
        match provision
            .runtimes
            .iter()
            .find(|runtime| runtime.runtime == expected.runtime)
        {
            Some(actual) if actual.version != expected.version => mismatches.push(format!(
                "{} was {} in the snapshot but {} is being used now.",
                expected.runtime.display_name(),
                expected.version,
                actual.version
            )),
            None => mismatches.push(format!(
                "{} ({}) from the snapshot could not be resolved.",
                expected.runtime.display_name(),
                expected.version
            )),
            _ => {}
        }
    }
    let records: Vec<RuntimeRecord> = provision.runtimes.iter().map(runtime_record).collect();
    let sandbox_options = SandboxOptions {
        base_dir: Some(base.clone()),
        id: Some(id),
        setup: if options.setup {
            SetupMode::Auto
        } else {
            SetupMode::Skip
        },
        offline: options.offline,
        max_file_bytes: 512 * 1024 * 1024,
        memory_limit_mb: probe.profile.memory_limit_mb,
        entrypoint_index: options.entrypoint_index,
        extra_args: options.extra_args.clone(),
        write_windows_sandbox: false,
    };
    let mut sandbox = winorchestrator_sandbox::prepare(
        &acquisition.root,
        spec.source.as_str(),
        spec.source_kind,
        &plan,
        &records,
        &probe.profile,
        &sandbox_options,
    )?;
    sandbox.record.notes.extend(provision.notes.clone());
    sandbox.record.warnings.extend(provision.warnings.clone());
    sandbox.record.warnings.extend(mismatches.clone());
    sandbox.set_lifecycle(LifecycleState::Restored)?;
    sandbox.write_record()?;
    let pipeline_options = PipelineOptions {
        target: source,
        id: Some(sandbox.record.id.clone()),
        base_dir: Some(base),
        offline: options.offline,
        force_download: options.force_download,
        setup: options.setup,
        write_windows_sandbox: false,
        entrypoint_index: options.entrypoint_index,
        extra_args: options.extra_args.clone(),
        snapshot_out: options.snapshot_out.clone(),
        memory_limit_mb: None,
        update_path: true,
    };
    let estimated = winorchestrator_provisioner::estimate_download_bytes(&matches);
    let source_bytes = fsutil::directory_size(&acquisition.root);
    let required_disk_bytes = estimated
        .saturating_add(source_bytes)
        .saturating_add(256 * 1024 * 1024);
    let notes = vec![
        format!(
            "Restored from a WinOrchestrator snapshot; source reacquired at {}.",
            acquisition.root.display()
        ),
        format!(
            "Detected {} language(s), {} requirement(s), {} entrypoint(s).",
            plan.languages.len(),
            plan.requirements.len(),
            plan.entrypoints.len()
        ),
    ];
    let mut warnings = plan.warnings.clone();
    warnings.extend(probe.warnings.clone());
    warnings.extend(mismatches.clone());
    let inspection = build_inspection(
        &pipeline_options.target,
        acquisition,
        plan,
        probe,
        matches,
        estimated,
        required_disk_bytes,
        notes,
        warnings,
    );
    Ok(RestoreResult {
        prepared: PreparedRun {
            inspection,
            sandbox,
            provision,
            source_root: PathBuf::new(),
            source_is_cached_clone: false,
            options: pipeline_options,
        },
        spec: spec.clone(),
        version_mismatches: mismatches,
    })
}

#[derive(Clone, Debug)]
pub struct CleanOptions {
    pub sandboxes: bool,
    pub workspaces: bool,
    pub downloads: bool,
    pub logs: bool,
    pub force: bool,
    pub stop_stale: bool,
}

impl Default for CleanOptions {
    fn default() -> Self {
        Self {
            sandboxes: true,
            workspaces: true,
            downloads: true,
            logs: true,
            force: false,
            stop_stale: false,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CleanReport {
    pub removed_sandboxes: Vec<PathBuf>,
    pub removed_workspaces: Vec<PathBuf>,
    pub removed_downloads: Vec<PathBuf>,
    pub removed_logs: Vec<PathBuf>,
    pub skipped: Vec<PathBuf>,
    pub stopped_pids: Vec<u32>,
    pub bytes_reclaimed: u64,
    pub warnings: Vec<String>,
}

pub fn clean(options: &CleanOptions) -> Result<CleanReport> {
    paths::ensure_app_dirs()?;
    let mut report = CleanReport::default();

    if options.stop_stale {
        let stale = winorchestrator_host_probe::find_stale_sandboxes();
        for item in stale {
            if let Some(pid) = item.pid {
                if winorchestrator_core::winapi::is_pid_alive(pid) {
                    let outcome = winorchestrator_core::process::terminate_pid_tree(pid);
                    if outcome.success {
                        report.stopped_pids.push(pid);
                    } else {
                        report.warnings.push(format!(
                            "could not stop stale pid {pid}: {}",
                            outcome.combined()
                        ));
                    }
                }
            }
        }
    }

    if options.sandboxes {
        for entry in fsutil::read_dir_sorted(&paths::sandboxes_dir())? {
            if !entry.is_dir() {
                continue;
            }
            let is_sandbox = SandboxRecord::record_path(&entry).is_file();
            if !is_sandbox && !options.force {
                report.skipped.push(entry);
                continue;
            }
            let size = fsutil::directory_size(&entry);
            fsutil::remove_tree(&entry)?;
            report.bytes_reclaimed += size;
            report.removed_sandboxes.push(entry);
        }
    }
    if options.workspaces {
        for entry in fsutil::read_dir_sorted(&paths::workspaces_dir())? {
            if entry.is_dir() {
                let size = fsutil::directory_size(&entry);
                fsutil::remove_tree(&entry)?;
                report.bytes_reclaimed += size;
                report.removed_workspaces.push(entry);
            }
        }
    }
    if options.downloads {
        for entry in fsutil::read_dir_sorted(&paths::downloads_dir())? {
            let size = if entry.is_dir() {
                fsutil::directory_size(&entry)
            } else {
                fsutil::file_size(&entry)
            };
            fsutil::remove_tree(&entry)?;
            report.bytes_reclaimed += size;
            report.removed_downloads.push(entry);
        }
    }
    if options.logs {
        for entry in fsutil::read_dir_sorted(&paths::logs_dir())? {
            let size = if entry.is_dir() {
                fsutil::directory_size(&entry)
            } else {
                fsutil::file_size(&entry)
            };
            fsutil::remove_tree(&entry)?;
            report.bytes_reclaimed += size;
            report.removed_logs.push(entry);
        }
    }
    Ok(report)
}

pub fn teardown(target: &str, force: bool) -> Result<PathBuf> {
    let candidate = PathBuf::from(target);
    let root = if candidate.is_dir() {
        candidate
    } else {
        paths::sandboxes_dir().join(target)
    };
    winorchestrator_sandbox::destroy(&root, force)?;
    Ok(root)
}

/// Background socket observer keyed to a sandbox root pid. The GUI reads
/// `latest()`; the CLI prints the final snapshot after the process exits.
pub struct NetObserver {
    stop: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<winorchestrator_net_tree::NetTreeReport>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl NetObserver {
    pub fn start(pid: u32, interval: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let latest = Arc::new(Mutex::new(None));
        let stop_thread = Arc::clone(&stop);
        let latest_thread = Arc::clone(&latest);
        let handle = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                let report = winorchestrator_net_tree::collect(&[pid]);
                if let Ok(mut slot) = latest_thread.lock() {
                    *slot = Some(report);
                }
                let mut slept = Duration::ZERO;
                while slept < interval && !stop_thread.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(100));
                    slept += Duration::from_millis(100);
                }
            }
        });
        Self {
            stop,
            latest,
            handle: Some(handle),
        }
    }

    pub fn latest(&self) -> Option<winorchestrator_net_tree::NetTreeReport> {
        self.latest.lock().ok().and_then(|slot| slot.clone())
    }
}

impl Drop for NetObserver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// A quick, deterministic summary of why dependencies were installed, used by
/// the CLI sign-off and the GUI dependency panel.
pub fn dependency_breakdown(plan: &ResolutionPlan) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for dependency in &plan.dependencies {
        let key = match dependency.origin {
            DependencyOrigin::Manifest => "manifest",
            DependencyOrigin::Ast => "ast-only",
        };
        *counts.entry(key.to_string()).or_insert(0) += 1;
    }
    counts
}

pub fn runtime_summary(runtimes: &[RuntimeRecord]) -> String {
    if runtimes.is_empty() {
        return "no runtimes".to_string();
    }
    runtimes
        .iter()
        .map(|runtime| {
            format!(
                "{} {} ({})",
                runtime.runtime.display_name(),
                runtime.version,
                if runtime.from_host { "host" } else { "fetched" }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn listening_conflicts(ports: &[u16]) -> Vec<String> {
    let bound = winorchestrator_net_tree::listening_ports();
    let mut conflicts = Vec::new();
    for port in ports {
        if let Some(pids) = bound.get(port) {
            conflicts.push(format!(
                "port {port} is already bound by pid(s) {:?}; the sandbox may fail to listen",
                pids
            ));
        }
    }
    conflicts
}

pub fn describe_runtime_kinds(runtimes: &[RuntimeRecord]) -> Vec<RuntimeKind> {
    runtimes.iter().map(|runtime| runtime.runtime).collect()
}
