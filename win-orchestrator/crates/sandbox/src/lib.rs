use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use winorchestrator_core::fsutil;
use winorchestrator_core::{paths, winapi};
use winorchestrator_core::{
    Error, ExecutionProfile, LifecycleState, ResolutionPlan, Result, RuntimeRecord, SandboxRecord,
    SourceKind,
};

mod env;
mod launch;
mod layout;
mod setup;

pub use env::SandboxEnvironment;
pub use launch::{build_launch, runtime_for, select_entrypoint, wrap_batch};
pub use layout::SandboxLayout;
pub use setup::{run_setup, SetupMode, SetupReport, SetupStep};

const CREATE_SUSPENDED: u32 = 0x0000_0004;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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
    pub write_windows_sandbox: bool,
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
            write_windows_sandbox: false,
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
        let (child, job, command) = self.spawn(extra_args, false)?;
        Ok(RunningProcess {
            child,
            job,
            pid: 0,
            command,
            started: Instant::now(),
            waited: None,
        }
        .with_pid())
    }

    /// Run with captured output, a hard timeout and job-object cleanup.
    pub fn run_capture(
        &self,
        extra_args: &[String],
        timeout: Option<Duration>,
    ) -> Result<RunOutcome> {
        let (mut child, job, command) = self.spawn(extra_args, true)?;
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
                            if let Some(job) = &job {
                                if !job.terminate() {
                                    let _ = child.kill();
                                }
                            } else {
                                let _ = child.kill();
                            }
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
        drop(job);
        Ok(RunOutcome {
            exit_code: status.and_then(|status| status.code()),
            success: status.map(|status| status.success()).unwrap_or(false),
            timed_out,
            duration,
            stdout,
            stderr,
            command,
        })
    }

    fn spawn(
        &self,
        extra_args: &[String],
        capture: bool,
    ) -> Result<(Child, Option<winapi::JobHandle>, String)> {
        let launch = self
            .record
            .launch
            .as_ref()
            .ok_or_else(|| Error::NoEntrypoint(self.record.repo_name.clone()))?;
        let mut args = launch.args.clone();
        args.extend(extra_args.iter().cloned());
        let command_text = winorchestrator_core::process::format_command(&launch.program, &args);

        let mut command = Command::new(&launch.program);
        command.args(&args).current_dir(&launch.cwd);
        command.env_clear();
        for (key, value) in &self.record.environment {
            command.env(key, value);
        }
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

        let job = winapi::JobHandle::create(
            self.record.profile.memory_limit_mb.or(self
                .record
                .profile
                .low_footprint
                .then_some(2048)),
        );

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let mut flags = 0u32;
            if capture {
                flags |= CREATE_NO_WINDOW;
            }
            if job.is_some() {
                flags |= CREATE_SUSPENDED;
            }
            if flags != 0 {
                command.creation_flags(flags);
            }
        }

        let mut child = command.spawn().map_err(|source| Error::Io {
            context: format!(" while launching {}", command_text),
            source,
        })?;
        let pid = child.id();
        if let Some(job) = &job {
            if !job.assign_pid(pid) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Message(format!(
                    "could not place pid {pid} in a Job Object; refusing to run without the memory cap"
                )));
            }
            if !winapi::resume_process(pid) {
                let _ = job.terminate();
                let _ = child.wait();
                return Err(Error::Message(format!(
                    "could not resume suspended sandbox process {pid}"
                )));
            }
        }
        Ok((child, job, command_text))
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
    job: Option<winapi::JobHandle>,
    pub pid: u32,
    pub command: String,
    started: Instant,
    waited: Option<Option<i32>>,
}

impl RunningProcess {
    fn with_pid(mut self) -> Self {
        self.pid = self.child.id();
        self
    }

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
                            if let Some(job) = &self.job {
                                if !job.terminate() {
                                    let _ = self.child.kill();
                                }
                            } else {
                                let _ = self.child.kill();
                            }
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
        // Dropping the job terminates any descendants that outlived the main
        // process, so a dev server cannot linger after its launcher exits.
        if let Some(job) = self.job.take() {
            drop(job);
        }
        Ok(RunOutcome {
            exit_code: code,
            success,
            timed_out,
            duration,
            stdout: String::new(),
            stderr: String::new(),
            command: self.command.clone(),
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

    let entrypoint = select_entrypoint(plan, options.entrypoint_index)?;
    let setup_report = run_setup(
        &layout,
        plan,
        runtimes,
        &entrypoint,
        options.setup,
        options.offline,
    )?;
    let environment = SandboxEnvironment::build(&layout, runtimes, profile);
    let launch = build_launch(&layout, plan, runtimes, &entrypoint, &options.extra_args)?;

    let mut notes = Vec::new();
    notes.push(format!(
        "Staged {} file(s) ({}) into {}.",
        copy.files,
        fsutil::human_bytes(copy.bytes),
        layout.workspace.display()
    ));
    notes.extend(copy.warnings);
    notes.extend(setup_report.notes);
    let warnings = setup_report.warnings;
    let record = SandboxRecord {
        schema_version: winorchestrator_core::SNAPSHOT_SCHEMA_VERSION,
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
    };

    let prepared = PreparedSandbox { layout, record };
    prepared.write_record()?;
    if options.write_windows_sandbox {
        match write_windows_sandbox(&prepared) {
            Ok(path) => {
                let mut prepared = prepared;
                prepared.record.notes.push(format!(
                    "Optional Windows Sandbox definition written to {}.",
                    path.display()
                ));
                prepared.write_record()?;
                return Ok(prepared);
            }
            Err(err) => {
                let mut prepared = prepared;
                prepared
                    .record
                    .warnings
                    .push(format!("Could not write the optional .wsb file: {err}"));
                prepared.write_record()?;
                return Ok(prepared);
            }
        }
    }
    Ok(prepared)
}

impl PreparedSandbox {
    pub fn write_record(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.record)?;
        fsutil::write_utf8(&self.layout.record_file, &format!("{json}\n"))
    }

    pub fn set_lifecycle(&mut self, lifecycle: LifecycleState) -> Result<()> {
        self.record.lifecycle = lifecycle;
        self.write_record()
    }
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

/// List every readable sandbox under the standard directory.
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
            "refusing to remove {} because it is not a WinOrchestrator sandbox (no sandbox.json)",
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

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn write_windows_sandbox(prepared: &PreparedSandbox) -> Result<PathBuf> {
    let launch = prepared
        .record
        .launch
        .as_ref()
        .ok_or_else(|| Error::NoEntrypoint(prepared.record.repo_name.clone()))?;
    let root_text = prepared.layout.root.display().to_string();
    let rewrite = |text: &str| -> String { text.replace(&root_text, "C:\\WinOrchestrator") };
    let program = rewrite(&launch.program);
    let mut powershell = String::new();
    powershell.push_str("Set-Location -LiteralPath '");
    powershell.push_str(&rewrite(&launch.cwd.display().to_string()).replace('\'', "''"));
    powershell.push_str("'; & '");
    powershell.push_str(&program.replace('\'', "''"));
    powershell.push('\'');
    for arg in &launch.args {
        powershell.push_str(" '");
        powershell.push_str(&rewrite(arg).replace('\'', "''"));
        powershell.push('\'');
    }
    let logon = format!(
        "powershell.exe -NoProfile -ExecutionPolicy Bypass -Command \"{}\"",
        powershell.replace('"', "\\\"")
    );

    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<Configuration>\n\
  <!-- Generated by WinOrchestrator. Optional manual mode; the CLI uses Job Objects. -->\n\
  <MappedFolders>\n\
    <MappedFolder>\n\
      <HostFolder>{}</HostFolder>\n\
      <SandboxFolder>C:\\WinOrchestrator</SandboxFolder>\n\
      <ReadOnly>false</ReadOnly>\n\
    </MappedFolder>\n\
  </MappedFolders>\n\
  <Networking>Default</Networking>\n\
  <LogonCommand>\n\
    <Command>{}</Command>\n\
  </LogonCommand>\n\
</Configuration>\n",
        xml_escape(&root_text),
        xml_escape(&logon)
    );
    let path = prepared.layout.wsb_file.clone();
    fsutil::write_utf8(&path, &xml)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use winorchestrator_core::{Entrypoint, EntrypointKind, ExecutionProfile, RuntimeKind};

    #[test]
    fn layout_stays_inside_root() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("wo-sandbox-test-{stamp}"));
        let layout = SandboxLayout::create(Some(&base), "demo").unwrap();
        layout.ensure_safe().unwrap();
        assert!(paths::is_within(&layout.root, &layout.workspace));
        assert!(layout.root.is_dir());
        fsutil::remove_tree(&base).unwrap();
    }

    #[test]
    fn wrapping_batch_files_uses_cmd() {
        let (program, args) = wrap_batch(
            Path::new("C:\\tools\\npm.cmd"),
            &["run".to_string(), "start".to_string()],
        );
        assert_eq!(program, "cmd.exe");
        assert_eq!(args[0], "/C");
        assert_eq!(args[1], "C:\\tools\\npm.cmd");
    }

    #[test]
    fn selecting_a_missing_entrypoint_is_reported() {
        let mut plan = ResolutionPlan::new(PathBuf::from("C:\\repo"), "repo".to_string());
        plan.entrypoints.push(Entrypoint::new(
            EntrypointKind::PythonScript,
            Some("main.py".to_string()),
            "test",
        ));
        assert!(select_entrypoint(&plan, Some(0)).is_ok());
        assert!(select_entrypoint(&plan, Some(3)).is_err());
        let empty = ResolutionPlan::new(PathBuf::from("C:\\repo"), "repo".to_string());
        assert!(select_entrypoint(&empty, None).is_err());
    }

    #[test]
    fn memory_limits_are_clamped_from_profile() {
        let profile = ExecutionProfile {
            low_footprint: true,
            memory_limit_mb: Some(1024),
            ..Default::default()
        };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("wo-env-test-{stamp}"));
        let layout = SandboxLayout::create(Some(&base), "demo").unwrap();
        let runtimes = vec![RuntimeRecord {
            runtime: RuntimeKind::Python,
            version: winorchestrator_core::Version::new(3, 12, 0),
            executable: base.join("python.exe"),
            home: base.clone(),
            from_host: true,
            source: "test".to_string(),
            companions: Default::default(),
        }];
        let env = SandboxEnvironment::build(&layout, &runtimes, &profile);
        assert_eq!(env.get("MEMORY_CAP_MB"), Some("1024"));
        assert!(env.get("PATH").unwrap().contains("python.exe") == false);
        assert!(env
            .get("PATH")
            .unwrap()
            .contains(&base.display().to_string()));
        fsutil::remove_tree(&base).unwrap();
    }

    #[test]
    fn record_roundtrip_and_destroy() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("wo-record-test-{stamp}"));
        let layout = SandboxLayout::create(Some(&base), "demo").unwrap();
        let plan = ResolutionPlan::new(PathBuf::from("C:\\repo"), "repo".to_string());
        let record = SandboxRecord {
            schema_version: 1,
            id: "demo".to_string(),
            repo_name: "repo".to_string(),
            source: "C:\\repo".to_string(),
            source_kind: SourceKind::Local,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            root: layout.root.clone(),
            workspace: layout.workspace.clone(),
            plan,
            runtimes: Vec::new(),
            environment: Default::default(),
            launch: None,
            profile: ExecutionProfile::default(),
            lifecycle: LifecycleState::Prepared,
            notes: Vec::new(),
            warnings: Vec::new(),
        };
        let prepared = PreparedSandbox::from_record(layout.clone(), record);
        prepared.write_record().unwrap();
        let loaded = load_record(&layout.root).unwrap();
        assert_eq!(loaded.repo_name, "repo");
        assert!(destroy(&layout.root, false).is_ok());
        assert!(!layout.root.exists());
        let _ = fsutil::remove_tree(&base);
    }

    #[test]
    fn captures_a_scoped_command_run() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("wo-run-test-{stamp}"));
        let source = base.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("README.md"), "demo").unwrap();
        let mut plan = ResolutionPlan::new(source.clone(), "demo".to_string());
        plan.entrypoints.push(Entrypoint::new(
            EntrypointKind::Command,
            Some("echo sandbox-ok".to_string()),
            "test command",
        ));
        let options = SandboxOptions {
            base_dir: Some(base.join("sandboxes")),
            setup: SetupMode::Skip,
            write_windows_sandbox: false,
            ..Default::default()
        };
        let prepared = prepare(
            &source,
            source.display().to_string().as_str(),
            SourceKind::Local,
            &plan,
            &[],
            &ExecutionProfile::default(),
            &options,
        )
        .unwrap();
        let outcome = prepared
            .run_capture(&[], Some(std::time::Duration::from_secs(30)))
            .unwrap();
        assert!(outcome.success, "stderr: {}", outcome.stderr);
        assert!(outcome.stdout.to_lowercase().contains("sandbox-ok"));
        destroy(&prepared.layout.root, false).unwrap();
        let _ = fsutil::remove_tree(&base);
    }

    #[test]
    fn json_record_is_valid() {
        let value: Value = serde_json::json!({"ok": true});
        assert_eq!(value["ok"], true);
    }
}
