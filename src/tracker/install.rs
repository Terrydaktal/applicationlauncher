use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::Command;

const KWIN_SCRIPT_ID: &str = "applicationlauncher-window-feed";
const KWIN_METADATA: &str =
    include_str!("../../kwin/applicationlauncher-window-feed/metadata.json");
const KWIN_MAIN_JS: &str =
    include_str!("../../kwin/applicationlauncher-window-feed/contents/code/main.js");

pub fn tracker_binary_path() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|err| err.to_string())?;
    Ok(current.with_file_name("applicationlauncherd"))
}

pub fn ensure_tracker_installed() -> Result<(), String> {
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is not set")?);
    let binary = tracker_binary_path()?;
    if !binary.exists() {
        return Err(format!(
            "Tracker binary is unavailable at {}",
            binary.display()
        ));
    }
    let bin_dir = home.join(".local/bin");
    std::fs::create_dir_all(&bin_dir).map_err(|err| err.to_string())?;
    let link = bin_dir.join("applicationlauncherd");
    let mut installation_changed = false;
    if link.symlink_metadata().is_ok() {
        if link.read_link().ok().as_ref() != Some(&binary) {
            std::fs::remove_file(&link).map_err(|err| err.to_string())?;
            installation_changed = true;
        }
    }
    if link.symlink_metadata().is_err() {
        symlink(&binary, &link).map_err(|err| err.to_string())?;
        installation_changed = true;
    }
    let unit_dir = home.join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir).map_err(|err| err.to_string())?;
    let binary_metadata = binary.metadata().map_err(|err| err.to_string())?;
    let modified = binary_metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs());
    let build_fingerprint = format!("{}-{}", binary_metadata.len(), modified);
    let unit = format!(
        "[Unit]\nDescription=Application Launcher window and session tracker\nAfter=graphical-session.target\nPartOf=graphical-session.target\n\n[Service]\nType=simple\nEnvironment=APPLICATIONLAUNCHER_DAEMON_BUILD={build_fingerprint}\nExecStart={}\nRestart=on-failure\nRestartSec=1\n\n[Install]\nWantedBy=graphical-session.target\n",
        link.display()
    );
    let unit_path = unit_dir.join("applicationlauncherd.service");
    if std::fs::read_to_string(&unit_path).ok().as_deref() != Some(&unit) {
        std::fs::write(&unit_path, unit).map_err(|err| err.to_string())?;
        installation_changed = true;
    }
    let reload = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()
        .map_err(|err| err.to_string())?;
    if !reload.success() {
        return Err("systemctl --user daemon-reload failed".into());
    }
    let enable = Command::new("systemctl")
        .args(["--user", "enable", "applicationlauncherd.service"])
        .status()
        .map_err(|err| err.to_string())?;
    if !enable.success() {
        return Err("systemctl --user enable applicationlauncherd.service failed".into());
    }
    let service_action = if installation_changed {
        "restart"
    } else {
        "start"
    };
    let restart = Command::new("systemctl")
        .args(["--user", service_action, "applicationlauncherd.service"])
        .status()
        .map_err(|err| err.to_string())?;
    if !restart.success() {
        return Err(format!(
            "systemctl --user {service_action} applicationlauncherd.service failed"
        ));
    }
    Ok(())
}

pub(crate) fn ensure_kwin_feed_installed() -> Result<(), String> {
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is not set")?);
    let script_dir = home.join(".local/share/kwin/scripts").join(KWIN_SCRIPT_ID);
    let code_dir = script_dir.join("contents/code");
    std::fs::create_dir_all(&code_dir).map_err(|err| err.to_string())?;
    std::fs::write(script_dir.join("metadata.json"), KWIN_METADATA)
        .map_err(|err| err.to_string())?;
    std::fs::write(code_dir.join("main.js"), KWIN_MAIN_JS).map_err(|err| err.to_string())?;
    let enabled = Command::new("kwriteconfig6")
        .args([
            "--file",
            "kwinrc",
            "--group",
            "Plugins",
            "--key",
            &format!("{KWIN_SCRIPT_ID}Enabled"),
            "true",
        ])
        .status()
        .map_err(|err| err.to_string())?;
    if !enabled.success() {
        return Err("Could not enable the KWin window feed script".into());
    }
    reload_kwin()
}

fn reload_kwin() -> Result<(), String> {
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is not set")?);
    let script = home
        .join(".local/share/kwin/scripts")
        .join(KWIN_SCRIPT_ID)
        .join("contents/code/main.js");
    let _ = Command::new("qdbus6")
        .args([
            "org.kde.KWin",
            "/Scripting",
            "org.kde.kwin.Scripting.unloadScript",
            KWIN_SCRIPT_ID,
        ])
        .status();
    let loaded = Command::new("qdbus6")
        .args([
            "org.kde.KWin",
            "/Scripting",
            "org.kde.kwin.Scripting.loadScript",
            &script.display().to_string(),
            KWIN_SCRIPT_ID,
        ])
        .status()
        .map_err(|err| err.to_string())?;
    if !loaded.success() {
        return Err("KWin rejected the window feed script load".into());
    }
    let started = Command::new("qdbus6")
        .args(["org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting.start"])
        .status()
        .map_err(|err| err.to_string())?;
    started
        .success()
        .then_some(())
        .ok_or_else(|| "KWin rejected the window feed script start".into())
}

pub(crate) fn start_kwin_feed_watchdog() {
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let loaded = Command::new("qdbus6")
                .args([
                    "org.kde.KWin",
                    "/Scripting",
                    "org.kde.kwin.Scripting.isScriptLoaded",
                    KWIN_SCRIPT_ID,
                ])
                .output()
                .ok()
                .is_some_and(|output| {
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout).trim() == "true"
                });
            if !loaded {
                if let Err(err) = reload_kwin() {
                    eprintln!("Tracker KWin feed watchdog failed: {err}");
                }
            }
        }
    });
}
