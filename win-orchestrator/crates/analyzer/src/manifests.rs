use regex::Regex;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use winorchestrator_core::{
    Dependency, DependencyKind, DependencyOrigin, Entrypoint, EntrypointKind, Language,
    Requirement, RuntimeKind, VersionReq,
};

#[derive(Clone, Debug, Default)]
pub struct ManifestBundle {
    pub languages: BTreeSet<Language>,
    pub requirements: Vec<Requirement>,
    pub dependencies: Vec<Dependency>,
    pub entrypoints: Vec<Entrypoint>,
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
}

impl ManifestBundle {
    fn add_requirement(&mut self, req: Option<Requirement>) {
        if let Some(req) = req {
            self.requirements.push(req);
        }
    }

    fn add_dependency(&mut self, dep: Option<Dependency>) {
        if let Some(dep) = dep {
            self.dependencies.push(dep);
        }
    }

    fn warning(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }
}

/// Parse a manifest by filename. Unknown files yield an empty bundle.
pub fn parse_manifest(kind: &str, path: &Path, root: &Path, relative: &str) -> ManifestBundle {
    let mut bundle = ManifestBundle::default();
    match kind {
        "requirements.txt" | "requirements-dev.txt" | "constraints.txt" => {
            bundle.languages.insert(Language::Python);
            parse_requirements(path, relative, root, &mut bundle, 0);
        }
        "pyproject.toml" => {
            bundle.languages.insert(Language::Python);
            parse_pyproject(path, relative, &mut bundle);
        }
        "setup.py" | "setup.cfg" => {
            bundle.languages.insert(Language::Python);
            parse_setup(path, relative, &mut bundle);
        }
        "package.json" => {
            bundle.languages.insert(Language::JavaScript);
            parse_package_json(path, relative, &mut bundle);
        }
        "Cargo.toml" => {
            bundle.languages.insert(Language::Rust);
            parse_cargo(path, relative, root, &mut bundle);
        }
        "pom.xml" => {
            bundle.languages.insert(Language::Java);
            parse_pom(path, relative, root, &mut bundle);
        }
        "build.gradle" | "build.gradle.kts" => {
            bundle.languages.insert(Language::Java);
            parse_gradle(path, relative, root, &mut bundle);
        }
        "go.mod" => {
            bundle.languages.insert(Language::Go);
            parse_go_mod(path, relative, &mut bundle);
        }
        _ => {}
    }
    bundle
}

pub fn manifest_kind(filename: &str) -> Option<&'static str> {
    let lowered = filename.to_ascii_lowercase();
    match lowered.as_str() {
        "requirements.txt" => Some("requirements.txt"),
        "requirements-dev.txt" => Some("requirements-dev.txt"),
        "constraints.txt" => Some("constraints.txt"),
        "pyproject.toml" => Some("pyproject.toml"),
        "setup.py" => Some("setup.py"),
        "setup.cfg" => Some("setup.cfg"),
        "package.json" => Some("package.json"),
        "cargo.toml" => Some("Cargo.toml"),
        "pom.xml" => Some("pom.xml"),
        "build.gradle" => Some("build.gradle"),
        "build.gradle.kts" => Some("build.gradle.kts"),
        "go.mod" => Some("go.mod"),
        _ => None,
    }
}

fn read_text(path: &Path, relative: &str, bundle: &mut ManifestBundle) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(err) => {
            bundle.warning(format!("could not read {relative}: {err}"));
            None
        }
    }
}

fn parse_requirements(
    path: &Path,
    relative: &str,
    root: &Path,
    bundle: &mut ManifestBundle,
    depth: usize,
) {
    if depth > 5 {
        return;
    }
    let Some(text) = read_text(path, relative, bundle) else {
        return;
    };
    for (line_number, raw_line) in text.lines().enumerate() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line
            .strip_prefix("-r ")
            .or_else(|| line.strip_prefix("--requirement "))
            .or_else(|| line.strip_prefix("-c "))
            .or_else(|| line.strip_prefix("--constraint "))
        {
            let include = path
                .parent()
                .unwrap_or(root)
                .join(rest.trim().trim_matches('"'));
            if include.starts_with(root) && include.is_file() {
                let rel = include
                    .strip_prefix(root)
                    .unwrap_or(&include)
                    .display()
                    .to_string();
                parse_requirements(&include, &rel, root, bundle, depth + 1);
            }
            continue;
        }
        if line.starts_with('-') {
            continue;
        }
        let Some((name, requirement)) = parse_python_spec(line) else {
            bundle.warning(format!(
                "{relative}:{}: could not parse requirement `{line}`",
                line_number + 1
            ));
            continue;
        };
        bundle.add_dependency(Some(Dependency {
            name,
            requirement,
            kind: DependencyKind::Runtime,
            source_file: relative.to_string(),
            origin: DependencyOrigin::Manifest,
        }));
    }
}

/// Parse a PEP 508-ish package specification: `name[extras]>=1.2; marker`.
pub fn parse_python_spec(spec: &str) -> Option<(String, Option<String>)> {
    let spec = spec.split(';').next()?.trim();
    if spec.is_empty() {
        return None;
    }
    // Direct URL reference: name @ https://...
    if let Some((name, _url)) = spec.split_once('@') {
        let name = clean_package_name(name);
        return if name.is_empty() {
            None
        } else {
            Some((name, None))
        };
    }
    let name_end = spec
        .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '=' | '!' | '~' | '^' | '('))
        .unwrap_or(spec.len());
    let name = clean_package_name(&spec[..name_end]);
    if name.is_empty() {
        return None;
    }
    let rest = spec[name_end..].trim().trim_end_matches(')').trim();
    let requirement = if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    };
    Some((name, requirement))
}

fn clean_package_name(raw: &str) -> String {
    let name = raw.trim().trim_start_matches('"').trim_end_matches('"');
    let name = name.split('[').next().unwrap_or(name).trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return String::new();
    }
    name.to_ascii_lowercase()
}

fn parse_pyproject(path: &Path, relative: &str, bundle: &mut ManifestBundle) {
    let Some(text) = read_text(path, relative, bundle) else {
        return;
    };
    let value: toml::Value = match text.parse() {
        Ok(value) => value,
        Err(err) => {
            bundle.warning(format!("{relative}: invalid TOML: {err}"));
            return;
        }
    };

    if let Some(requires_python) =
        get_toml(&value, &["project", "requires-python"]).and_then(|v| v.as_str())
    {
        bundle.add_requirement(Requirement::new(
            RuntimeKind::Python,
            requires_python,
            "pyproject.toml declares requires-python",
            relative,
        ));
    }

    if let Some(deps) = get_toml(&value, &["project", "dependencies"]).and_then(|v| v.as_array()) {
        add_python_dep_array(deps, DependencyKind::Runtime, relative, bundle);
    }
    if let Some(optional) =
        get_toml(&value, &["project", "optional-dependencies"]).and_then(|v| v.as_table())
    {
        for (group, items) in optional {
            if let Some(items) = items.as_array() {
                add_python_dep_array(
                    items,
                    DependencyKind::Optional,
                    &format!("{relative} [{group}]"),
                    bundle,
                );
            }
        }
    }
    if let Some(build) = get_toml(&value, &["build-system", "requires"]).and_then(|v| v.as_array())
    {
        add_python_dep_array(build, DependencyKind::Build, relative, bundle);
    }

    if let Some(scripts) = get_toml(&value, &["project", "scripts"]).and_then(|v| v.as_table()) {
        for (name, target) in scripts {
            if let Some(target) = target.as_str() {
                if let Some((module, _function)) = target.split_once(':') {
                    bundle.entrypoints.push(Entrypoint::new(
                        EntrypointKind::PythonModule,
                        Some(module.to_string()),
                        format!("pyproject script `{name}`"),
                    ));
                }
            }
        }
    }

    // Poetry layouts.
    let poetry = get_toml(&value, &["tool", "poetry"]);
    if let Some(poetry) = poetry {
        if let Some(python) = get_toml(poetry, &["dependencies", "python"]).and_then(|v| v.as_str())
        {
            bundle.add_requirement(Requirement::new(
                RuntimeKind::Python,
                python,
                "Poetry declares the supported Python range",
                relative,
            ));
        }
        if let Some(table) = get_toml(poetry, &["dependencies"]).and_then(|v| v.as_table()) {
            for (name, value) in table {
                if name.eq_ignore_ascii_case("python") {
                    continue;
                }
                let requirement = match value {
                    toml::Value::String(version) => Some(version.clone()),
                    toml::Value::Table(table) => table
                        .get("version")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string()),
                    _ => None,
                };
                bundle.add_dependency(Some(Dependency {
                    name: name.to_ascii_lowercase(),
                    requirement,
                    kind: DependencyKind::Runtime,
                    source_file: relative.to_string(),
                    origin: DependencyOrigin::Manifest,
                }));
            }
        }
        if let Some(scripts) = get_toml(poetry, &["scripts"]).and_then(|v| v.as_table()) {
            for (name, target) in scripts {
                if let Some(target) = target.as_str() {
                    if let Some((module, _function)) = target.split_once(':') {
                        bundle.entrypoints.push(Entrypoint::new(
                            EntrypointKind::PythonModule,
                            Some(module.to_string()),
                            format!("Poetry script `{name}`"),
                        ));
                    }
                }
            }
        }
    }
}

fn add_python_dep_array(
    items: &[toml::Value],
    kind: DependencyKind,
    source: &str,
    bundle: &mut ManifestBundle,
) {
    for item in items {
        if let Some(spec) = item.as_str() {
            if let Some((name, requirement)) = parse_python_spec(spec) {
                bundle.add_dependency(Some(Dependency {
                    name,
                    requirement,
                    kind,
                    source_file: source.to_string(),
                    origin: DependencyOrigin::Manifest,
                }));
            }
        }
    }
}

fn parse_setup(path: &Path, relative: &str, bundle: &mut ManifestBundle) {
    let Some(text) = read_text(path, relative, bundle) else {
        return;
    };
    let requires_python = Regex::new(r#"python_requires\s*=\s*["']([^"']+)["']"#).unwrap();
    if let Some(captures) = requires_python.captures(&text) {
        bundle.add_requirement(Requirement::new(
            RuntimeKind::Python,
            &captures[1],
            "setup.py declares python_requires",
            relative,
        ));
    }
    let install_requires = Regex::new(r"(?s)install_requires\s*=\s*\[(.*?)\]").unwrap();
    if let Some(captures) = install_requires.captures(&text) {
        let inner = &captures[1];
        for item in inner.split(',') {
            let item = item.trim().trim_matches(|c| c == '"' || c == '\'');
            if let Some((name, requirement)) = parse_python_spec(item) {
                bundle.add_dependency(Some(Dependency {
                    name,
                    requirement,
                    kind: DependencyKind::Runtime,
                    source_file: relative.to_string(),
                    origin: DependencyOrigin::Manifest,
                }));
            }
        }
    }
    // setup.cfg style: install_requires = newline list until blank line.
    if relative.ends_with("setup.cfg") {
        let mut in_install = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("install_requires") {
                if let Some(rest) = rest.trim().strip_prefix('=') {
                    let rest = rest.trim();
                    if !rest.is_empty() {
                        if let Some((name, requirement)) = parse_python_spec(rest) {
                            bundle.add_dependency(Some(Dependency {
                                name,
                                requirement,
                                kind: DependencyKind::Runtime,
                                source_file: relative.to_string(),
                                origin: DependencyOrigin::Manifest,
                            }));
                        }
                    } else {
                        in_install = true;
                    }
                }
                continue;
            }
            if in_install {
                if trimmed.is_empty() {
                    in_install = false;
                    continue;
                }
                if let Some((name, requirement)) = parse_python_spec(trimmed) {
                    bundle.add_dependency(Some(Dependency {
                        name,
                        requirement,
                        kind: DependencyKind::Runtime,
                        source_file: relative.to_string(),
                        origin: DependencyOrigin::Manifest,
                    }));
                }
            }
        }
    }
}

fn parse_package_json(path: &Path, relative: &str, bundle: &mut ManifestBundle) {
    let Some(text) = read_text(path, relative, bundle) else {
        return;
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            bundle.warning(format!("{relative}: invalid JSON: {err}"));
            return;
        }
    };

    if let Some(node) = value.pointer("/engines/node").and_then(|v| v.as_str()) {
        bundle.add_requirement(Requirement::new(
            RuntimeKind::Node,
            node,
            "package.json engines.node",
            relative,
        ));
    }
    for (pointer, kind) in [
        ("/dependencies", DependencyKind::Runtime),
        ("/devDependencies", DependencyKind::Dev),
        ("/optionalDependencies", DependencyKind::Optional),
        ("/peerDependencies", DependencyKind::Runtime),
    ] {
        if let Some(table) = value.pointer(pointer).and_then(|v| v.as_object()) {
            for (name, version) in table {
                bundle.add_dependency(Some(Dependency {
                    name: name.clone(),
                    requirement: version.as_str().map(|v| v.to_string()),
                    kind,
                    source_file: relative.to_string(),
                    origin: DependencyOrigin::Manifest,
                }));
            }
        }
    }

    if let Some(main) = value.get("main").and_then(|v| v.as_str()) {
        bundle.entrypoints.push(Entrypoint::new(
            EntrypointKind::NodeScript,
            Some(main.to_string()),
            "package.json main field",
        ));
    }
    if let Some(bin) = value.get("bin") {
        match bin {
            serde_json::Value::String(path) => bundle.entrypoints.push(Entrypoint::new(
                EntrypointKind::NodeScript,
                Some(path.clone()),
                "package.json bin field",
            )),
            serde_json::Value::Object(table) => {
                for path in table.values().filter_map(|v| v.as_str()) {
                    bundle.entrypoints.push(Entrypoint::new(
                        EntrypointKind::NodeScript,
                        Some(path.to_string()),
                        "package.json bin field",
                    ));
                    break;
                }
            }
            _ => {}
        }
    }
    if let Some(scripts) = value.get("scripts").and_then(|v| v.as_object()) {
        for script in ["start", "dev", "serve"] {
            if scripts.contains_key(script) {
                bundle.entrypoints.push(
                    Entrypoint::new(
                        EntrypointKind::NpmScript,
                        Some(script.to_string()),
                        format!("package.json script `{script}`"),
                    )
                    .with_args(&["run", script]),
                );
            }
        }
    }
}

fn parse_cargo(path: &Path, relative: &str, root: &Path, bundle: &mut ManifestBundle) {
    let Some(text) = read_text(path, relative, bundle) else {
        return;
    };
    let value: toml::Value = match text.parse() {
        Ok(value) => value,
        Err(err) => {
            bundle.warning(format!("{relative}: invalid TOML: {err}"));
            return;
        }
    };
    let rust_version = get_toml(&value, &["package", "rust-version"])
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    if let Some(rust_version) = &rust_version {
        bundle.add_requirement(Requirement::new(
            RuntimeKind::Rust,
            rust_version,
            "Cargo.toml package.rust-version",
            relative,
        ));
    } else if let Some(edition) = get_toml(&value, &["package", "edition"]).and_then(|v| v.as_str())
    {
        let requirement = match edition {
            "2024" => ">=1.85",
            "2021" => ">=1.56",
            "2018" => ">=1.31",
            _ => ">=1.0",
        };
        bundle.add_requirement(Requirement::new(
            RuntimeKind::Rust,
            requirement,
            format!("Cargo.toml edition {edition}"),
            relative,
        ));
    }
    for (table_name, kind) in [
        ("dependencies", DependencyKind::Runtime),
        ("dev-dependencies", DependencyKind::Dev),
        ("build-dependencies", DependencyKind::Build),
    ] {
        if let Some(table) = get_toml(&value, &[table_name]).and_then(|v| v.as_table()) {
            for (name, spec) in table {
                let requirement = match spec {
                    toml::Value::String(version) => Some(version.clone()),
                    toml::Value::Table(table) => table
                        .get("version")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string()),
                    _ => None,
                };
                bundle.add_dependency(Some(Dependency {
                    name: name.clone(),
                    requirement,
                    kind,
                    source_file: relative.to_string(),
                    origin: DependencyOrigin::Manifest,
                }));
            }
        }
    }
    if root.join("src").join("main.rs").is_file() {
        bundle.entrypoints.push(Entrypoint::new(
            EntrypointKind::RustBinary,
            Some(".".to_string()),
            "src/main.rs exists (cargo run)",
        ));
    }
}

fn parse_pom(path: &Path, relative: &str, root: &Path, bundle: &mut ManifestBundle) {
    let Some(text) = read_text(path, relative, bundle) else {
        return;
    };
    let version_tag = Regex::new(
        r"(?i)<(?:maven\.compiler\.(?:source|target|release)|java\.version|release)>\s*([0-9][0-9._]*)\s*</",
    )
    .unwrap();
    for captures in version_tag.captures_iter(&text) {
        let raw = &captures[1];
        let normalized = raw.replace('_', ".");
        bundle.add_requirement(Requirement::new(
            RuntimeKind::Java,
            &normalized,
            format!("pom.xml compiler level {raw}"),
            relative,
        ));
    }

    let dependency = Regex::new(r"(?s)<dependency>(.*?)</dependency>").unwrap();
    for captures in dependency.captures_iter(&text) {
        let block = &captures[1];
        let group = xml_tag(block, "groupId").unwrap_or_default();
        let artifact = xml_tag(block, "artifactId").unwrap_or_default();
        if artifact.is_empty() {
            continue;
        }
        let version = xml_tag(block, "version").filter(|v| !v.contains("${"));
        let scope = xml_tag(block, "scope").unwrap_or_default();
        let kind = match scope.as_str() {
            "test" => DependencyKind::Dev,
            "provided" | "system" => DependencyKind::Build,
            _ => DependencyKind::Runtime,
        };
        let name = if group.is_empty() {
            artifact
        } else {
            format!("{group}:{artifact}")
        };
        bundle.add_dependency(Some(Dependency {
            name,
            requirement: version,
            kind,
            source_file: relative.to_string(),
            origin: DependencyOrigin::Manifest,
        }));
    }

    if text.contains("spring-boot-maven-plugin") {
        let wrapper = root.join("mvnw.cmd");
        bundle.entrypoints.push(
            Entrypoint::new(
                EntrypointKind::Maven,
                Some(if wrapper.is_file() { "mvnw.cmd" } else { "mvn" }.to_string()),
                "Spring Boot Maven plugin",
            )
            .with_args(&["spring-boot:run"]),
        );
    }
}

fn parse_gradle(path: &Path, relative: &str, root: &Path, bundle: &mut ManifestBundle) {
    let Some(text) = read_text(path, relative, bundle) else {
        return;
    };
    for pattern in [
        r"JavaLanguageVersion\.of\(\s*([0-9]+)\s*\)",
        r#"sourceCompatibility\s*=\s*['"]?([0-9]+)"#,
        r#"targetCompatibility\s*=\s*['"]?([0-9]+)"#,
        r"JavaVersion\.VERSION_([0-9_]+)",
    ] {
        if let Ok(regex) = Regex::new(pattern) {
            for captures in regex.captures_iter(&text) {
                let raw = captures[1].replace('_', ".");
                if let Some(req) = Requirement::new(
                    RuntimeKind::Java,
                    &format!(">={raw}"),
                    format!("Gradle compiler level {raw}"),
                    relative,
                ) {
                    bundle.requirements.push(req);
                }
            }
        }
    }
    let dependency = Regex::new(
        r#"(?:implementation|api|compileOnly|runtimeOnly|testImplementation)\s*\(?\s*['"]([^:'"]+):([^:'"]+):([^'")]+)['"]"#,
    )
    .unwrap();
    for captures in dependency.captures_iter(&text) {
        let name = format!("{}:{}", &captures[1], &captures[2]);
        let version = captures[3].trim();
        let kind = if text[..captures.get(0).unwrap().start()].ends_with("testImplementation") {
            DependencyKind::Dev
        } else {
            DependencyKind::Runtime
        };
        bundle.add_dependency(Some(Dependency {
            name,
            requirement: (!version.is_empty() && !version.contains('$'))
                .then(|| version.to_string()),
            kind,
            source_file: relative.to_string(),
            origin: DependencyOrigin::Manifest,
        }));
    }
    if text.contains("org.springframework.boot") {
        let wrapper = root.join("gradlew.bat");
        bundle.entrypoints.push(
            Entrypoint::new(
                EntrypointKind::Gradle,
                Some(
                    if wrapper.is_file() {
                        "gradlew.bat"
                    } else {
                        "gradle"
                    }
                    .to_string(),
                ),
                "Spring Boot Gradle plugin",
            )
            .with_args(&["bootRun"]),
        );
    }
}

fn parse_go_mod(path: &Path, relative: &str, bundle: &mut ManifestBundle) {
    let Some(text) = read_text(path, relative, bundle) else {
        return;
    };
    let mut in_require_block = false;
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("go ") {
            bundle.add_requirement(Requirement::new(
                RuntimeKind::Go,
                &format!(">={}", rest.trim()),
                "go.mod go directive",
                relative,
            ));
            continue;
        }
        if line.starts_with("require (") {
            in_require_block = true;
            continue;
        }
        if in_require_block && line == ")" {
            in_require_block = false;
            continue;
        }
        let require = if in_require_block {
            Some(line)
        } else {
            line.strip_prefix("require ")
        };
        if let Some(require) = require {
            let mut parts = require.split_whitespace();
            if let Some(name) = parts.next() {
                let version = parts.next().map(|v| v.to_string());
                bundle.add_dependency(Some(Dependency {
                    name: name.to_string(),
                    requirement: version,
                    kind: DependencyKind::Runtime,
                    source_file: relative.to_string(),
                    origin: DependencyOrigin::Manifest,
                }));
            }
        }
    }
}

fn get_toml<'a>(value: &'a toml::Value, path: &[&str]) -> Option<&'a toml::Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn xml_tag(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let start = block.find(&open)?;
    let after_open = &block[start..];
    let gt = after_open.find('>')?;
    let rest = &after_open[gt + 1..];
    let close = format!("</{tag}>");
    let end = rest.find(&close)?;
    let value = rest[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Check whether a version string parses; used by the analyzer to choose
/// between a manifest's declared range and a safe default.
pub fn requirement_parses(raw: &str) -> bool {
    VersionReq::parse(raw).is_some()
}

/// Normalised relative path for display in plans and snapshots.
pub fn rel_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(PathBuf::from)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_python_specs() {
        assert_eq!(
            parse_python_spec("requests>=2.31.0"),
            Some(("requests".to_string(), Some(">=2.31.0".to_string())))
        );
        assert_eq!(
            parse_python_spec("Flask[async] == 3.0.0 ; python_version >= '3.9'"),
            Some(("flask".to_string(), Some("== 3.0.0".to_string())))
        );
        assert_eq!(
            parse_python_spec("sqlalchemy"),
            Some(("sqlalchemy".to_string(), None))
        );
        assert!(parse_python_spec(">>> invalid <<<").is_none());
    }

    #[test]
    fn parses_xml_tags() {
        let block = "<groupId>org.acme</groupId><artifactId>core</artifactId>";
        assert_eq!(xml_tag(block, "groupId"), Some("org.acme".to_string()));
        assert_eq!(xml_tag(block, "artifactId"), Some("core".to_string()));
        assert_eq!(xml_tag(block, "version"), None);
    }
}
