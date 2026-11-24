use clap::{Parser, Subcommand};
use serde::Deserialize;
use serde_json::json;
use std::env;
use std::process::Command;
mod resource_manager;

#[derive(Parser)]
#[command(name = "Zero-Friction Orchestrator")]
#[command(about = "AI-Powered Environment Manager")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Inspect {
        #[arg(default_value = ".")]
        path: String,
    },
    Run {
        #[arg(default_value = ".")]
        path: String,
        #[arg(short, long)]
        persist: bool,
        #[arg(short, long)]
        autopilot: bool, // AI kicks in only if this is passed
    },
    Clean,
}

#[derive(Deserialize, Debug)]
struct EnvironmentManifest {
    inject_host_python: bool,
    reason: String,
}

fn resolve_target_path(target: &str) -> Option<String> {
    if target.starts_with("http") || target.starts_with("git@") {
        println!("Detected remote URL. Cloning {}...", target);
        let repo_name = target.split('/').last().unwrap_or("repo").replace(".git", "");
        
        let status = Command::new("git").args(["clone", target]).status();
        if let Ok(stat) = status {
            if stat.success() {
                return Some(repo_name);
            }
        }
        println!("Git clone failed.");
        return None;
    }
    Some(target.to_string())
}

fn check_runtime(cmd: &str, arg: &str) -> Option<String> {
    if let Ok(output) = Command::new(cmd).arg(arg).output() {
        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if version.is_empty() {
                return Some(String::from_utf8_lossy(&output.stderr).trim().to_string());
            }
            return Some(version);
        }
    }
    None
}

async fn scan_host_environment() -> (String, String) {
    println!("Scanning host runtimes...\n");

    let py_ver = check_runtime("python3", "--version").unwrap_or_else(|| "None".to_string());
    let node_ver = check_runtime("node", "--version").unwrap_or_else(|| "None".to_string());

    println!("Found Python: {}", py_ver);
    println!("Found Node.js: {}\n", node_ver);

    (py_ver, node_ver)
}

fn extract_dependencies_via_ast(path: &str) -> String {
    println!("Running Python AST parser on {}...", path);
    
    let output = Command::new("python3")
        .arg("src/parser.py")
        .arg(path)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let result = String::from_utf8_lossy(&out.stdout).trim().to_string();
            println!("AST Extraction: {}", result);
            result
        }
        _ => {
            println!("Failed to run parser.py. Falling back to empty dependency list.");
            "[]".to_string()
        }
    }
}

async fn analyze_codebase_with_groq(path: &str, host_py: &str, host_node: &str) -> Option<EnvironmentManifest> {
    let deps = extract_dependencies_via_ast(path);
    
    println!("Sending environment data to Groq for analysis...");

    let api_key = match env::var("GROQ_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            println!("Error: GROQ_API_KEY environment variable not set.");
            return None;
        }
    };

    let client = reqwest::Client::new();
    
    let prompt = format!(
        "The codebase at {} has these third-party dependencies: {}. The host machine is running {} and {}. Are any of these packages deprecated? Are there breaking syntax differences? Decide if we should safely inject the host runtime. Return ONLY a JSON object exactly like this: {{\"inject_host_python\": true, \"reason\": \"explanation here\"}}",
        path, deps, host_py, host_node
    );

    let payload = json!({
        "model": "openai/gpt-oss-20b",
        "response_format": { "type": "json_object" },
        "messages": [{
            "role": "user",
            "content": prompt
        }]
    });

    let res = client.post("https://api.groq.com/openai/v1/chat/completions")
        .bearer_auth(api_key)
        .json(&payload)
        .send()
        .await;

    match res {
        Ok(response) => {
            if let Ok(json_val) = response.json::<serde_json::Value>().await {
                if let Some(content) = json_val["choices"][0]["message"]["content"].as_str() {
                    match serde_json::from_str::<EnvironmentManifest>(content) {
                        Ok(manifest) => {
                            println!("AI Decision: {:#?}", manifest);
                            return Some(manifest);
                        }
                        Err(e) => println!("Failed to parse AI JSON: {}\nRaw output: {}", e, content),
                    }
                } else {
                    println!("Groq returned an error or unexpected format:\n{:#?}", json_val);
                }
            }
        }
        Err(e) => println!("Failed to reach AI engine: {}", e),
    }
    None
}

#[cfg(target_os = "linux")]
fn build_ephemeral_sandbox(path: &str) {
    println!("Spinning up ephemeral bwrap sandbox for {}...", path);
    
    let canonical = std::path::Path::new(path).canonicalize().unwrap();
    
    // If the path is a file, grab its parent directory instead
    let dir_path = if canonical.is_file() {
        canonical.parent().unwrap().to_path_buf()
    } else {
        canonical
    };
    
    let abs_dir = dir_path.display().to_string();

    let status = Command::new("bwrap")
        .arg("--ro-bind").arg("/").arg("/")
        .arg("--dev").arg("/dev")
        .arg("--proc").arg("/proc")
        .arg("--bind").arg(&abs_dir).arg(&abs_dir)
        .arg("--chdir").arg(&abs_dir)
        .arg("--unshare-all")
        .arg("--share-net")
        .arg("--hostname").arg("sandbox")
        .arg("--setenv").arg("PS1").arg("[zero-friction] \\w $ ")
        .arg("--die-with-parent")
        .arg("bash")
        .arg("--norc") 
        .status();

    if let Ok(stat) = status {
        if stat.success() {
            println!("Sandbox execution finished cleanly.");
        } else {
            println!("Sandbox exited with a non-zero status.");
        }
    } else {
        println!("Failed to launch bwrap. Ensure bubblewrap is installed.");
    }
}

#[cfg(target_os = "windows")]
fn build_ephemeral_sandbox(path: &str) {
    use std::fs::File;
    use std::io::Write;
    use std::path::Path;

    println!("Generating ephemeral Windows Sandbox for {}...", path);

    let abs_path = Path::new(path).canonicalize().unwrap().display().to_string();
    let abs_path = abs_path.replace("\\\\?\\", "");

    let wsb_content = format!(
        r#"<Configuration>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>{}</HostFolder>
      <SandboxFolder>C:\Workspace</SandboxFolder>
      <ReadOnly>false</ReadOnly>
    </MappedFolder>
  </MappedFolders>
  <LogonCommand>
    <Command>cmd.exe /k "cd C:\Workspace"</Command>
  </LogonCommand>
</Configuration>"#,
        abs_path
    );

    let wsb_path = "ephemeral_env.wsb";
    if let Ok(mut file) = File::create(wsb_path) {
        file.write_all(wsb_content.as_bytes()).unwrap();
    }

    let status = Command::new("cmd")
        .args(["/C", "start", "/wait", wsb_path])
        .status();

    if let Ok(stat) = status {
        if stat.success() {
            println!("Windows Sandbox closed cleanly.");
        } else {
            println!("Sandbox execution failed.");
        }
    }

    let _ = std::fs::remove_file(wsb_path);
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Inspect { path } => {
            println!("Inspecting repository at: {}", path);
            let (host_py, host_node) = scan_host_environment().await;
            let _ = analyze_codebase_with_groq(path, &host_py, &host_node).await;
        }
        Commands::Run { path, persist: _, autopilot } => {
            let target_dir = match resolve_target_path(path) {
                Some(dir) => dir,
                None => {
                    println!("Aborting run due to invalid target or failed clone.");
                    return;
                }
            };
            
            println!("Preparing to run: {}", target_dir);
            
            let (host_py, host_node) = scan_host_environment().await;
            
            if *autopilot {
                println!("Autopilot enabled. Routing to AI...");
                if let Some(manifest) = analyze_codebase_with_groq(&target_dir, &host_py, &host_node).await {
                    println!("AI Verdict: {}", manifest.reason);
                    if manifest.inject_host_python {
                        build_ephemeral_sandbox(&target_dir);
                    }
                }
            } else {
                println!("Offline mode engaged. Running deterministic checks...");
                let deps = extract_dependencies_via_ast(&target_dir);
                
                if deps == "[]" {
                    println!("Offline Check: No third-party dependencies found. Safe to inject host runtime.");
                    
                    // Freeze before sandbox
                    resource_manager::freeze_background_apps();
                    
                    build_ephemeral_sandbox(&target_dir);
                    
                    // Thaw after sandbox exits
                    resource_manager::thaw_background_apps();
                } else {
                    println!("Offline Check: Found external dependencies: {}. We need to download these (Step 4 pending).", deps);
                }
            }
        }
        Commands::Clean => {
            println!("Cleaning up all persistent sandboxes...");
        }
    }
}