use std::collections::BTreeSet;
use std::path::Path;

/// Commands found in a README's fenced code blocks. They are hints only:
/// every hint is checked against the repository before becoming an entrypoint.
#[derive(Clone, Debug, Default)]
pub struct ReadmeAnalysis {
    pub path: Option<String>,
    pub hints: Vec<String>,
}

pub fn analyze_readme(root: &Path) -> ReadmeAnalysis {
    let mut result = ReadmeAnalysis::default();
    let candidates = [
        "README.md",
        "readme.md",
        "README.MD",
        "README.rst",
        "readme.rst",
        "README.txt",
        "readme.txt",
        "README",
    ];
    let Some(path) = candidates
        .iter()
        .map(|name| root.join(name))
        .find(|p| p.is_file())
    else {
        return result;
    };
    let truncated = std::fs::read(&path)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes[..bytes.len().min(512 * 1024)]).to_string())
        .unwrap_or_default();
    result.path = Some(
        path.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
    );

    let mut in_fence = false;
    let mut language = String::new();
    let mut hints = BTreeSet::new();
    for line in truncated.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            if in_fence {
                in_fence = false;
                language.clear();
            } else {
                in_fence = true;
                language = trimmed
                    .trim_start_matches(['`', '~'])
                    .trim()
                    .to_ascii_lowercase();
            }
            continue;
        }
        if !in_fence {
            continue;
        }
        if !language.is_empty()
            && !matches!(
                language.as_str(),
                "bash"
                    | "sh"
                    | "shell"
                    | "zsh"
                    | "console"
                    | "cmd"
                    | "bat"
                    | "powershell"
                    | "ps1"
                    | "text"
                    | "none"
            )
        {
            continue;
        }
        let command = trimmed
            .trim_start_matches("$ ")
            .trim_start_matches("> ")
            .trim_start_matches("PS> ")
            .trim_start_matches("PS C:\\> ")
            .trim();
        if command.is_empty() || command.starts_with('#') || command.starts_with("//") {
            continue;
        }
        let first = command.split_whitespace().next().unwrap_or("");
        let is_command = matches!(
            first,
            "python"
                | "python3"
                | "py"
                | "pip"
                | "pip3"
                | "poetry"
                | "uv"
                | "node"
                | "npm"
                | "pnpm"
                | "yarn"
                | "npx"
                | "java"
                | "javac"
                | "mvn"
                | "mvnw"
                | "gradle"
                | "gradlew"
                | "cargo"
                | "go"
                | "make"
                | "dotnet"
        );
        if is_command {
            hints.insert(command.to_string());
        }
        if hints.len() >= 40 {
            break;
        }
    }
    result.hints = hints.into_iter().collect();
    result
}