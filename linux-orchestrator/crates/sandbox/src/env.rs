use std::collections::BTreeMap;
use std::path::PathBuf;
use linuxorchestrator_core::{ExecutionProfile, RuntimeRecord};

use crate::layout::SandboxLayout;

/// Forward host display-related environment variables into the sandbox.
/// This enables GUI applications (Qt, etc.) launched inside the sandbox to
/// connect to the host's X11/Wayland display.
fn forward_display_environment(vars: &mut BTreeMap<String, String>) {
    // Forward native display variables.
    for key in ["DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"] {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                vars.insert(key.to_string(), value);
            }
        }
    }
    // Disable X11 MIT-SHM extension so Qt avoids shared-memory errors
    // inside a sandboxed mount namespace.
    if std::env::var_os("DISPLAY").is_some() {
        vars.insert("QT_X11_NO_MITSHM".to_string(), "1".to_string());
    }
}

/// A deliberately small, scoped environment. The host `PATH` is never used:
/// only sandbox toolchains and the base system directories are visible.
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

        vars.insert("LANG".to_string(), "C.UTF-8".to_string());
        vars.insert("LC_ALL".to_string(), "C.UTF-8".to_string());
        vars.insert("LINUXORCHESTRATOR_SANDBOX".to_string(), layout.id.clone());
        vars.insert("TERM".to_string(), "xterm-256color".to_string());
        forward_display_environment(&mut vars);

        let home = layout.home.display().to_string();
        vars.insert("HOME".to_string(), home.clone());
        vars.insert(
            "XDG_CONFIG_HOME".to_string(),
            layout.home.join(".config").display().to_string(),
        );
        vars.insert(
            "XDG_CACHE_HOME".to_string(),
            layout.home.join(".cache").display().to_string(),
        );
        vars.insert(
            "XDG_DATA_HOME".to_string(),
            layout
                .home
                .join(".local")
                .join("share")
                .display()
                .to_string(),
        );
        vars.insert(
            "XDG_STATE_HOME".to_string(),
            layout
                .home
                .join(".local")
                .join("state")
                .display()
                .to_string(),
        );
        vars.insert("TMPDIR".to_string(), layout.temp.display().to_string());
        vars.insert("TEMP".to_string(), layout.temp.display().to_string());
        vars.insert("TMP".to_string(), layout.temp.display().to_string());

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
            if runtime.runtime == linuxorchestrator_core::RuntimeKind::Java {
                vars.insert("JAVA_HOME".to_string(), runtime.home.display().to_string());
            }
        }

        let mut path_dirs: Vec<PathBuf> = Vec::new();
        for bin in ["bin", "sbin"] {
            let venv_bin = layout.venv.join(bin);
            if venv_bin.is_dir() {
                path_dirs.push(venv_bin);
            }
        }
        path_dirs.push(layout.meta.join("bin"));
        path_dirs.push(layout.tools.join("bin"));
        path_dirs.push(layout.home.join(".local").join("bin"));
        for runtime in runtimes {
            if let Some(parent) = runtime.executable.parent() {
                path_dirs.push(parent.to_path_buf());
            }
            path_dirs.push(runtime.home.join("bin"));
            path_dirs.push(runtime.path_dir());
        }
        for dir in ["/usr/local/bin", "/usr/bin", "/bin", "/usr/local/sbin", "/usr/sbin", "/sbin"] {
            path_dirs.push(PathBuf::from(dir));
        }
        path_dirs.push(layout.runtime_root.clone());

        let mut seen = std::collections::BTreeSet::new();
        let mut path_text = String::new();
        for dir in path_dirs {
            let key = dir.display().to_string();
            if !seen.insert(key) {
                continue;
            }
            if !path_text.is_empty() {
                path_text.push(':');
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
