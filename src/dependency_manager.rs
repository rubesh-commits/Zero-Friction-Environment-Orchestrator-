use std::process::Command;
use std::path::Path;

// Offline map for common import-to-PyPI mismatches
fn resolve_pypi_name(import_name: &str) -> Option<&str> {
    match import_name {
        "sklearn" => Some("scikit-learn"),
        "OpenGL" => Some("PyOpenGL"),
        "cv2" => Some("opencv-python"),
        "PIL" => Some("Pillow"),
        "yaml" => Some("PyYAML"),
        "dotenv" => Some("python-dotenv"),
        "bs4" => Some("beautifulsoup4"),
        "obelix" => None, // Explicitly drop unresolvable/internal modules
        _ => Some(import_name),
    }
}

pub fn setup_venv_and_install(target_dir: &str, deps_str: &str) -> bool {
    let venv_path = format!("{}/.venv", target_dir);
    
    if !Path::new(&venv_path).exists() {
        println!("No virtual environment found in {}. Creating one cleanly...", target_dir);
        let status = Command::new("python3")
            .args(["-m", "venv", &venv_path])
            .status();
            
        if let Ok(stat) = status {
            if !stat.success() {
                println!("Failed to create virtual environment.");
                return false;
            }
        } else {
            println!("Failed to execute python3 venv module.");
            return false;
        }
    } else {
        println!("Found existing virtual environment.");
    }

    let clean_deps = deps_str
        .trim_matches(|c| c == '[' || c == ']' || c == '\n')
        .replace('\'', "")
        .replace('"', "");
        
    let dep_list: Vec<&str> = clean_deps
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter_map(|s| resolve_pypi_name(s)) // This safely drops anything that returns None
        .collect();

    if dep_list.is_empty() {
        return true;
    }

    println!("Installing missing dependencies: {:?}", dep_list);
    let pip_path = format!("{}/bin/pip", venv_path);
    
    let status = Command::new(&pip_path)
        .arg("install")
        .args(&dep_list)
        .status();
    
    if let Ok(stat) = status {
        if stat.success() {
            println!("Dependencies installed successfully!");
            return true;
        }
    }
    
    println!("Pip install failed. Some packages might need manual fixing.");
    false
}