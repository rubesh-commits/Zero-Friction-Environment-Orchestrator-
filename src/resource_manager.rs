use std::process::Command;

// Target common RAM/VRAM hogs
const HEAVY_APPS: &[&str] = &["discord", "spotify", "chrome", "firefox", "slack"];

pub fn freeze_background_apps() {
    println!("Freezing non-essential background apps to free up RAM...");
    let mut froze_any = false;

    for app in HEAVY_APPS {
        let status = Command::new("killall")
            .arg("-q") // quiet
            .arg("-STOP")
            .arg(app)
            .status();
        
        if let Ok(stat) = status {
            if stat.success() {
                println!("  -> Froze {}", app);
                froze_any = true;
            }
        }
    }

    if froze_any {
        println!("Memory eviction hints sent. Resources temporarily reclaimed.");
    } else {
        println!("No heavy background apps detected.");
    }
}

pub fn thaw_background_apps() {
    println!("Thawing background apps...");
    for app in HEAVY_APPS {
        let _ = Command::new("killall")
            .arg("-q")
            .arg("-CONT")
            .arg(app)
            .status();
    }
    println!("System resources restored to normal.");
}