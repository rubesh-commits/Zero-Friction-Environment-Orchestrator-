use std::path::{Path, PathBuf};
use linuxorchestrator_core::fsutil;
use linuxorchestrator_core::{paths, Error, Result};

/// Deterministic directory layout for one sandbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxLayout {
    pub id: String,
    pub root: PathBuf,
    pub workspace: PathBuf,
    pub runtime_root: PathBuf,
    pub venv: PathBuf,
    pub home: PathBuf,
    pub temp: PathBuf,
    pub cache: PathBuf,
    pub logs: PathBuf,
    pub tools: PathBuf,
    pub meta: PathBuf,
    pub classes: PathBuf,
    pub record_file: PathBuf,
    pub script_file: PathBuf,
}

impl SandboxLayout {
    pub fn create(base: Option<&Path>, id: &str) -> Result<Self> {
        let root = base
            .map(|base| base.join(id))
            .unwrap_or_else(|| paths::sandboxes_dir().join(id));
        let layout = Self {
            id: id.to_string(),
            workspace: root.join("workspace"),
            runtime_root: root.join("runtimes"),
            venv: root.join("venv"),
            home: root.join("home"),
            temp: root.join("temp"),
            cache: root.join("cache"),
            logs: root.join("logs"),
            tools: root.join("tools"),
            meta: root.join(".linuxorchestrator"),
            classes: root.join("classes"),
            record_file: root.join("sandbox.json"),
            script_file: root.join("run.sh"),
            root,
        };
        for dir in [
            &layout.root,
            &layout.workspace,
            &layout.runtime_root,
            &layout.home,
            &layout.temp,
            &layout.cache,
            &layout.logs,
            &layout.tools,
            &layout.meta,
            &layout.classes,
        ] {
            fsutil::ensure_dir(dir)?;
        }
        for dir in [
            layout.home.join(".config"),
            layout.home.join(".local").join("share"),
            layout.home.join(".local").join("state"),
            layout.home.join(".cache"),
            layout.home.join(".npm"),
            layout.cache.join("pip"),
            layout.cache.join("npm"),
            layout.cache.join("cargo"),
            layout.cache.join("go"),
            layout.cache.join("gradle"),
            layout.cache.join("pycache"),
            layout.meta.join("bin"),
        ] {
            fsutil::ensure_dir(&dir)?;
        }
        Ok(layout)
    }

    pub fn exists(&self) -> bool {
        self.record_file.is_file()
    }

    /// Rebuild the layout for an existing sandbox without creating directories.
    pub fn at(root: PathBuf, id: String) -> Result<Self> {
        let layout = Self {
            id,
            workspace: root.join("workspace"),
            runtime_root: root.join("runtimes"),
            venv: root.join("venv"),
            home: root.join("home"),
            temp: root.join("temp"),
            cache: root.join("cache"),
            logs: root.join("logs"),
            tools: root.join("tools"),
            meta: root.join(".linuxorchestrator"),
            classes: root.join("classes"),
            record_file: root.join("sandbox.json"),
            script_file: root.join("run.sh"),
            root,
        };
        layout.ensure_safe()?;
        Ok(layout)
    }

    pub fn ensure_safe(&self) -> Result<()> {
        if !paths::is_within(&self.root, &self.workspace) {
            return Err(Error::PathEscape(self.workspace.clone()));
        }
        Ok(())
    }
}
