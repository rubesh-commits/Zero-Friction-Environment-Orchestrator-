use std::collections::BTreeMap;
use std::path::PathBuf;
use winorchestrator_core::{ExecutionProfile, RuntimeRecord};

use crate::layout::SandboxLayout;

/// A deliberately small, scoped environment. The host `PATH` is never used:
/// only sandbox toolchains and Windows' own system directories are visible.
#[derive(Clone, Debug, Default)]
pub struct SandboxEnvironment {
    pub vars: BTreeMap<String, String>,
}

impl SandboxEnvironment {
    pub fn build(
        layout: &SandboxLayout,
        runtimes: &[RuntimeRecord],
        profile: &ExecutionProfile,
    ) -> Self {
        let mut vars = BTreeMap::new();
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
        let comspec = std::env::var("ComSpec")
            .unwrap_or_else(|_| format!("{system_root}\\System32\\cmd.exe"));
        let pathext = std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD;.VBS;.JS;.WSF;.MSC".to_string());

        for key in [
            "PROCESSOR_ARCHITECTURE",
            "PROCESSOR_IDENTIFIER",
            "NUMBER_OF_PROCESSORS",
            "OS",
            "ProgramData",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "CommonProgramFiles",
        ] {
            if let Ok(value) = std::env::var(key) {
                vars.insert(key.to_string(), value);
            }
        }

        vars.insert("SystemRoot".to_string(), system_root.clone());
        vars.insert("windir".to_string(), system_root.clone());
        vars.insert("SystemDrive".to_string(), system_drive);
        vars.insert("ComSpec".to_string(), comspec);
        vars.insert("PATHEXT".to_string(), pathext);
        vars.insert("LANG".to_string(), "C.UTF-8".to_string());
        vars.insert("LC_ALL".to_string(), "C.UTF-8".to_string());
        vars.insert("WINORCHESTRATOR_SANDBOX".to_string(), layout.id.clone());

        vars.insert("HOME".to_string(), layout.home.display().to_string());
        vars.insert("USERPROFILE".to_string(), layout.home.display().to_string());
        vars.insert(
            "APPDATA".to_string(),
            layout
                .home
                .join("AppData")
                .join("Roaming")
                .display()
                .to_string(),
        );
        vars.insert(
            "LOCALAPPDATA".to_string(),
            layout
                .home
                .join("AppData")
                .join("Local")
                .display()
                .to_string(),
        );
        vars.insert("TEMP".to_string(), layout.temp.display().to_string());
        vars.insert("TMP".to_string(), layout.temp.display().to_string());
        vars.insert("TMPDIR".to_string(), layout.temp.display().to_string());

        vars.insert(
            "PIP_CACHE_DIR".to_string(),
            layout.cache.join("pip").display().to_string(),
        );
        vars.insert("PIP_DISABLE_PIP_VERSION_CHECK".to_string(), "1".to_string());
        vars.insert("PYTHONNOUSERSITE".to_string(), "1".to_string());
        vars.insert("PYTHONUTF8".to_string(), "1".to_string());
        vars.insert(
            "PYTHONPYCACHEPREFIX".to_string(),
            layout.cache.join("pycache").display().to_string(),
        );
        vars.insert(
            "npm_config_cache".to_string(),
            layout.cache.join("npm").display().to_string(),
        );
        vars.insert(
            "npm_config_userconfig".to_string(),
            layout.home.join(".npmrc").display().to_string(),
        );
        vars.insert("npm_config_audit".to_string(), "false".to_string());
        vars.insert("npm_config_fund".to_string(), "false".to_string());
        vars.insert(
            "CARGO_HOME".to_string(),
            layout.cache.join("cargo").display().to_string(),
        );
        vars.insert(
            "GOPATH".to_string(),
            layout.cache.join("go").display().to_string(),
        );
        vars.insert(
            "GRADLE_USER_HOME".to_string(),
            layout.cache.join("gradle").display().to_string(),
        );
        vars.insert(
            "NODE_REPL_HISTORY".to_string(),
            layout.cache.join("node_repl_history").display().to_string(),
        );

        if layout.venv.is_dir() {
            vars.insert("VIRTUAL_ENV".to_string(), layout.venv.display().to_string());
        }

        for runtime in runtimes {
            if runtime.runtime == winorchestrator_core::RuntimeKind::Java {
                vars.insert("JAVA_HOME".to_string(), runtime.home.display().to_string());
            }
        }

        let mut path_dirs: Vec<PathBuf> = Vec::new();
        if layout.venv.join("Scripts").is_dir() {
            path_dirs.push(layout.venv.join("Scripts"));
        }
        path_dirs.push(layout.meta.join("bin"));
        path_dirs.push(layout.tools.join("bin"));
        path_dirs.push(layout.home.join(".local").join("bin"));
        for runtime in runtimes {
            path_dirs.push(runtime.path_dir());
        }
        path_dirs.push(PathBuf::from(format!("{system_root}\\System32")));
        path_dirs.push(PathBuf::from(system_root.clone()));
        path_dirs.push(PathBuf::from(format!("{system_root}\\System32\\Wbem")));
        path_dirs.push(PathBuf::from(format!(
            "{system_root}\\System32\\WindowsPowerShell\\v1.0"
        )));
        path_dirs.push(PathBuf::from(format!("{system_root}\\System32\\OpenSSH")));
        path_dirs.push(PathBuf::from(layout.runtime_root.display().to_string()));

        let mut seen = std::collections::BTreeSet::new();
        let mut path_text = String::new();
        for dir in path_dirs {
            let key = dir.display().to_string().to_ascii_lowercase();
            if !seen.insert(key) {
                continue;
            }
            if !path_text.is_empty() {
                path_text.push(';');
            }
            path_text.push_str(&dir.display().to_string());
        }
        vars.insert("PATH".to_string(), path_text);
        vars.insert(
            "MEMORY_CAP_MB".to_string(),
            profile
                .memory_limit_mb
                .map(|mb| mb.to_string())
                .unwrap_or_default(),
        );
        Self { vars }
    }

    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        self.vars.insert(key.to_string(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(|value| value.as_str())
    }

    pub fn apply(&self, command: &mut std::process::Command) {
        command.env_clear();
        for (key, value) in &self.vars {
            command.env(key, value);
        }
    }

    pub fn pairs(&self) -> Vec<(String, String)> {
        self.vars
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}
