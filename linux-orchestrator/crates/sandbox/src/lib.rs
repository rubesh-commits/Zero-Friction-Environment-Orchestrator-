pub mod bwrap;
pub mod env;
pub mod launch;
pub mod layout;
pub mod setup;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use linuxorchestrator_core::fsutil;
use linuxorchestrator_core::{
    paths, Error, ExecutionProfile, LaunchRecord, LifecycleState, ResolutionPlan, Result,
    RuntimeRecord, SandboxRecord, SourceKind,
};

pub use env::SandboxEnvironment;
pub use layout::SandboxLayout;
pub use setup::{run_setup, SetupMode, SetupReport, SetupStep};

#[derive(Clone, Debug)]
pub struct SandboxOptions {
    pub base_dir: Option<PathBuf>,
    pub id: Option<String>,
    pub setup: SetupMode,
    pub offline: bool,
    pub max_file_bytes: u64,
    pub memory_limit_mb: Option<u64>,
    pub entrypoint_index: Option<usize>,
    pub extra_args: Vec<String>,
}

impl Default for SandboxOptions {
    fn default() -> Self {
        Self {
            base_dir: None,
            id: None,
            setup: SetupMode::Auto,
            offline: false,
            max_file_bytes: 512 * 1024 * 1024,
            memory_limit_mb: None,
            entrypoint_index: None,
            extra_args: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PreparedSandbox {
    pub layout: SandboxLayout,
    pub record: SandboxRecord,
}

impl PreparedSandbox {
    pub fn from_record(layout: SandboxLayout, record: SandboxRecord) -> Self {
        Self { layout, record }
    }

    pub fn launch_description(&self) -> String {
        self.record
            .launch
            .as_ref()
            .map(|launch| launch.display.clone())
            .unwrap_or_else(|| "<no entrypoint>".to_string())
    }

    /// Spawn the project with inherited stdio so interactive programs work.
    pub fn spawn_interactive(&self, extra_args: &[String]) -> Result<RunningProcess> {
        let (child, command) = self.spawn(extra_args, false)?;
        let pid = child.id();
        Ok(RunningProcess {
            child,
            pid,
            command,
            started: Instant::now(),
            waited: None,
        })
    }

    /// Run with captured output and a hard timeout.
    pub fn run_capture(
        &self,
        extra_args: &[String],
        timeout: Option<Duration>,
    ) -> Result<RunOutcome> {
        let (mut child, command) = self.spawn(extra_args, true)?;
        let stdout = child.stdout.take().map(|mut stream| {
            std::thread::spawn(move || {
                let mut buffer = Vec::new();
                let _ = stream.read_to_end(&mut buffer);
                buffer
            })
        });
        let stderr = child.stderr.take().map(|mut stream| {
            std::thread::spawn(move || {
                let mut buffer = Vec::new();
                let _ = stream.read_to_end(&mut buffer);
                buffer
            })
        });
        let started = Instant::now();
        let (status, timed_out) = loop {
            match child.try_wait() {
                Ok(Some(value)) => break (Some(value), false),
                Ok(None) => {
                    if let Some(limit) = timeout {
                        if started.elapsed() >= limit {
                            let _ = child.kill();
                            break (child.wait().ok(), true);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(err) => {
                    let _ = child.kill();
                    return Err(Error::Message(format!(
                        "while waiting for the sandbox: {err}"
                    )));
                }
            }
        };
        let stdout = stdout
            .and_then(|handle| handle.join().ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
            .unwrap_or_default();
        let stderr = stderr
            .and_then(|handle| handle.join().ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
            .unwrap_or_default();
        let duration = started.elapsed();
        Ok(RunOutcome {
            exit_code: status.and_then(|status| status.code()),
            success: status.map(|status| status.success()).unwrap_or(false) && !timed_out,
            timed_out,
            duration,
            stdout,
            stderr,
            command,
            pid: child.id(),
        })
    }

    fn spawn(&self, extra_args: &[String], capture: bool) -> Result<(Child, String)> {
        let launch = self
            .record
            .launch
            .as_ref()
            .ok_or_else(|| Error::NoEntrypoint(self.record.repo_name.clone()))?;
        let environment = SandboxEnvironment {
            vars: self.record.environment.clone(),
        };
        let isolated = bwrap::isolation_available();
        let mut command =
            bwrap::build_command(&self.layout, &environment, launch, extra_args, isolated);
        if capture {
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        } else {
            command
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
        }
        let mut all_args = launch.args.clone();
        all_args.extend(extra_args.iter().cloned());
        let command_text = linuxorchestrator_core::process::format_command(&launch.program, &all_args);
        let child = command.spawn().map_err(|source| Error::Io {
            context: format!(" while launching {command_text}"),
            source,
        })?;
        Ok((child, command_text))
    }

    pub fn write_record(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.record)?;
        fsutil::write_utf8(&self.layout.record_file, &format!("{json}\n"))
    }

    pub fn set_lifecycle(&mut self, lifecycle: LifecycleState) -> Result<()> {
        self.record.lifecycle = lifecycle;
        self.write_record()
    }
}

#[derive(Clone, Debug)]
pub struct RunOutcome {
    pub exit_code: Option<i32>,
    pub success: bool,
    pub timed_out: bool,
    pub duration: Duration,
    pub stdout: String,
    pub stderr: String,
    pub command: String,
    pub pid: u32,
}

impl RunOutcome {
    pub fn combined(&self) -> String {
        let stdout = self.stdout.trim();
        let stderr = self.stderr.trim();
        match (stdout.is_empty(), stderr.is_empty()) {
            (false, false) => format!("{stdout}\n{stderr}"),
            (false, true) => stdout.to_string(),
            (true, false) => stderr.to_string(),
            (true, true) => String::new(),
        }
    }
}

pub struct RunningProcess {
    child: Child,
    pub pid: u32,
    pub command: String,
    started: Instant,
    waited: Option<Option<i32>>,
}

impl RunningProcess {
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn try_wait(&mut self) -> Option<Option<i32>> {
        if let Some(value) = self.waited {
            return Some(value);
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                let code = status.code();
                self.waited = Some(code);
                Some(code)
            }
            Ok(None) => None,
            Err(_) => {
                self.waited = Some(None);
                Some(None)
            }
        }
    }

    pub fn wait(mut self, timeout: Option<Duration>) -> Result<RunOutcome> {
        let (code, timed_out) = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break (status.code(), false),
                Ok(None) => {
                    if let Some(limit) = timeout {
                        if self.started.elapsed() >= limit {
                            let _ = self.child.kill();
                            break (
                                self.child.wait().ok().and_then(|status| status.code()),
                                true,
                            );
                        }
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(err) => {
                    let _ = self.child.kill();
                    return Err(Error::Message(format!(
                        "while waiting for the sandbox: {err}"
                    )));
                }
            }
        };
        let duration = self.started.elapsed();
        let success = code == Some(0) && !timed_out;
        Ok(RunOutcome {
            exit_code: code,
            success,
            timed_out,
            duration,
            stdout: String::new(),
            stderr: String::new(),
            command: self.command.clone(),
            pid: self.pid,
        })
    }
}

impl Drop for RunningProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Create the sandbox, stage the repository and prepare dependencies.
pub fn prepare(
    source_root: &Path,
    source_label: &str,
    source_kind: SourceKind,
    plan: &ResolutionPlan,
    runtimes: &[RuntimeRecord],
    profile: &ExecutionProfile,
    options: &SandboxOptions,
) -> Result<PreparedSandbox> {
    if !source_root.is_dir() {
        return Err(Error::Message(format!(
            "cannot stage {}: not a directory",
            source_root.display()
        )));
    }
    paths::ensure_app_dirs()?;
    let id = options
        .id
        .clone()
        .unwrap_or_else(|| paths::unique_sandbox_id(&plan.repo_name));
    let layout = SandboxLayout::create(options.base_dir.as_deref(), &id)?;
    layout.ensure_safe()?;

    let copy = fsutil::copy_tree_filtered(source_root, &layout.workspace, options.max_file_bytes)?;

    let entrypoint = launch::select_entrypoint(plan, options.entrypoint_index)?;
    let setup_report = run_setup(
        &layout,
        plan,
        runtimes,
        &entrypoint,
        options.setup,
        options.offline,
    )?;
    let environment = SandboxEnvironment::build(&layout, runtimes, profile);
    let launch = launch::build_launch(&layout, plan, runtimes, &entrypoint, &options.extra_args)?;

    let mut notes = Vec::new();
    notes.push(format!(
        "Staged {} file(s) ({}) into {}.",
        copy.files,
        fsutil::human_bytes(copy.bytes),
        layout.workspace.display()
    ));
    notes.extend(copy.warnings);
    notes.extend(setup_report.notes);
    let mut warnings = setup_report.warnings;
    if !bwrap::isolation_available() {
        warnings.push(
            "bubblewrap (bwrap) is unavailable or cannot create namespaces; the project will run directly with a scoped environment instead of an isolated mount namespace."
                .to_string(),
        );
    }

    let mut record = SandboxRecord {
        schema_version: linuxorchestrator_core::SNAPSHOT_SCHEMA_VERSION,
        id: layout.id.clone(),
        repo_name: plan.repo_name.clone(),
        source: source_label.to_string(),
        source_kind,
        created_at: paths::iso8601_now(),
        root: layout.root.clone(),
        workspace: layout.workspace.clone(),
        plan: plan.clone(),
        runtimes: runtimes.to_vec(),
        environment: environment.vars.clone(),
        launch: Some(launch),
        profile: profile.clone(),
        lifecycle: LifecycleState::Prepared,
        notes,
        warnings,
        pid: None,
    };
    if !bwrap::isolation_available() {
        record.notes.push(
            "Isolation fallback: direct execution with a scrubbed environment (bwrap unavailable)."
                .to_string(),
        );
    }

    let prepared = PreparedSandbox { layout, record };
    prepared.write_record()?;
    Ok(prepared)
}

/// Read a sandbox record from its directory.
pub fn load_record(root: &Path) -> Result<SandboxRecord> {
    let file = SandboxRecord::record_path(root);
    let text = fsutil::read_utf8_strict(&file)?;
    let record: SandboxRecord = serde_json::from_str(&text)?;
    Ok(record)
}

pub fn open_sandbox(root: &Path) -> Result<PreparedSandbox> {
    let record = load_record(root)?;
    let layout = SandboxLayout::at(root.to_path_buf(), record.id.clone())?;
    Ok(PreparedSandbox { layout, record })
}

pub fn list_sandboxes() -> Vec<SandboxRecord> {
    list_sandboxes_in(&paths::sandboxes_dir())
}

pub fn list_sandboxes_in(base: &Path) -> Vec<SandboxRecord> {
    let mut records = Vec::new();
    let Ok(entries) = fsutil::read_dir_sorted(base) else {
        return records;
    };
    for entry in entries {
        if !entry.is_dir() {
            continue;
        }
        if let Ok(record) = load_record(&entry) {
            records.push(record);
        }
    }
    records
}

/// Destroy a sandbox. Refuses to remove a directory that does not contain a
/// sandbox record unless `force` is set, so a bad path can never erase a repo.
pub fn destroy(root: &Path, force: bool) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    if !force && !SandboxRecord::record_path(root).is_file() {
        return Err(Error::Message(format!(
            "refusing to remove {} because it is not a LinuxOrchestrator sandbox (no sandbox.json)",
            root.display()
        )));
    }
    fsutil::remove_tree(root)?;
    if root.exists() {
        return Err(Error::Message(format!(
            "sandbox cleanup left files behind at {}",
            root.display()
        )));
    }
    Ok(())
}

/// Compatibility helper for callers that already built a launch record.
pub fn execute_in_env(
    record: &LaunchRecord,
    env: &SandboxEnvironment,
    cwd: &Path,
    timeout: Option<Duration>,
) -> Result<std::process::ExitStatus> {
    let mut command = Command::new(&record.program);
    command.args(&record.args).current_dir(cwd);
    env.apply(&mut command);
    let mut child = command.spawn().map_err(|source| Error::Io {
        context: format!("running {}", record.display),
        source,
    })?;
    if let Some(limit) = timeout {
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) => {
                    if started.elapsed() >= limit {
                        let _ = child.kill();
                        return child.wait().map_err(|source| Error::Io {
                            context: format!("waiting for {}", record.display),
                            source,
                        });
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(source) => {
                    return Err(Error::Io {
                        context: format!("waiting for {}", record.display),
                        source,
                    })
                }
            }
        }
    }
    child.wait().map_err(|source| Error::Io {
        context: format!("waiting for {}", record.display),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use linuxorchestrator_core::{
        Entrypoint, EntrypointKind, Language, RuntimeKind, Version,
    };

    fn scratch(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("lo-sandbox-{name}-{stamp}"))
    }

    fn python_runtime() -> Option<RuntimeRecord> {
        let executable = linuxorchestrator_core::process::which("python3")?;
        Some(RuntimeRecord {
            runtime: RuntimeKind::Python,
            version: Version::new(3, 12, 0),
            executable,
            home: PathBuf::from("/usr"),
            from_host: true,
            source: "host (test)".to_string(),
            companions: Default::default(),
        })
    }

    #[test]
    fn prepares_writes_and_destroys_a_sandbox() {
        let Some(python) = python_runtime() else {
            return;
        };
        let source = scratch("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("main.py"), "print('ok')\n").unwrap();

        let mut plan = ResolutionPlan::new(source.clone(), "demo".to_string());
        plan.languages = vec![Language::Python];
        plan.entrypoints.push(Entrypoint::new(
            EntrypointKind::PythonScript,
            Some("main.py".to_string()),
            "test entrypoint",
        ));
        plan.requirements.push(linuxorchestrator_core::Requirement {
            runtime: RuntimeKind::Python,
            constraint: linuxorchestrator_core::VersionReq::parse(">=3.8").unwrap(),
            minimum: Some(Version::new(3, 8, 0)),
            reason: "test".to_string(),
            source: "test".to_string(),
        });

        let base = scratch("base");
        std::fs::create_dir_all(&base).unwrap();
        let options = SandboxOptions {
            base_dir: Some(base.clone()),
            id: Some("test-sandbox".to_string()),
            setup: SetupMode::Skip,
            ..Default::default()
        };
        let prepared = prepare(
            &source,
            source.display().to_string().as_str(),
            SourceKind::Local,
            &plan,
            &[python],
            &ExecutionProfile::default(),
            &options,
        )
        .unwrap();
        assert!(prepared.layout.record_file.is_file());
        assert!(prepared.record.launch.is_some());
        assert_eq!(prepared.record.lifecycle, LifecycleState::Prepared);

        let loaded = load_record(&prepared.layout.root).unwrap();
        assert_eq!(loaded.id, prepared.record.id);
        destroy(&prepared.layout.root, false).unwrap();
        assert!(!prepared.layout.root.exists());
        let _ = std::fs::remove_dir_all(&source);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn runs_a_trivial_python_script_when_bwrap_is_available() {
        if !bwrap::isolation_available() {
            return;
        }
        let Some(python) = python_runtime() else {
            return;
        };
        let source = scratch("run-source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("main.py"), "print('sandbox-ok')\n").unwrap();

        let mut plan = ResolutionPlan::new(source.clone(), "demo".to_string());
        plan.languages = vec![Language::Python];
        plan.entrypoints.push(Entrypoint::new(
            EntrypointKind::PythonScript,
            Some("main.py".to_string()),
            "test entrypoint",
        ));
        let base = scratch("run-base");
        std::fs::create_dir_all(&base).unwrap();
        let options = SandboxOptions {
            base_dir: Some(base.clone()),
            id: Some("run-sandbox".to_string()),
            setup: SetupMode::Skip,
            ..Default::default()
        };
        let prepared = prepare(
            &source,
            source.display().to_string().as_str(),
            SourceKind::Local,
            &plan,
            &[python],
            &ExecutionProfile::default(),
            &options,
        )
        .unwrap();
        let outcome = prepared
            .run_capture(&[], Some(Duration::from_secs(30)))
            .unwrap();
        assert!(outcome.stdout.contains("sandbox-ok"), "{:?}", outcome);
        destroy(&prepared.layout.root, false).unwrap();
        let _ = std::fs::remove_dir_all(&source);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn bwrap_arg_assembly() {
        /* implementation – assert bwrap command contains expected flags */
    }

    #[test]
    fn venv_path_roundtrip() {
        /* implementation – load a sandbox record, verify venv → layout.venv/bin/python */
    }

    #[test]
    fn sandbox_create_destroy() {
        /* implementation – create sandbox, then destroy; root should be gone */
    }
}
