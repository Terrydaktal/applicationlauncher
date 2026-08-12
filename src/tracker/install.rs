use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const KWIN_SCRIPT_ID: &str = "applicationlauncher-window-feed";
const KWIN_FEED_WATCHDOG_POLL_INTERVAL: Duration = Duration::from_secs(2);
const KWIN_FEED_MISSING_GRACE: Duration = Duration::from_secs(2);
const KWIN_FEED_RETRY_DELAY: Duration = Duration::from_secs(5);
const KWIN_METADATA: &str =
    include_str!("../../kwin/applicationlauncher-window-feed/metadata.json");
const KWIN_MAIN_JS: &str =
    include_str!("../../kwin/applicationlauncher-window-feed/contents/code/main.js");
static KWIN_SCRIPTING_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Default)]
struct KwinFeedWatchdogState {
    missing_since: Option<Instant>,
    retry_not_before: Option<Instant>,
}

impl KwinFeedWatchdogState {
    fn should_recover(&mut self, now: Instant, loaded: bool, kdotool_running: bool) -> bool {
        if loaded {
            self.missing_since = None;
            self.retry_not_before = None;
            return false;
        }

        // kdotool also uses KWin's scripting service. Wait for a full quiet
        // period after it exits rather than racing its temporary action script.
        if kdotool_running {
            self.missing_since = Some(now);
            return false;
        }

        if self
            .retry_not_before
            .is_some_and(|retry_not_before| now < retry_not_before)
        {
            return false;
        }

        let missing_since = *self.missing_since.get_or_insert(now);
        if now.duration_since(missing_since) < KWIN_FEED_MISSING_GRACE {
            return false;
        }

        self.missing_since = Some(now);
        self.retry_not_before = Some(now + KWIN_FEED_RETRY_DELAY);
        true
    }
}

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
    if link.read_link().ok().as_ref() != Some(&binary) {
        let unique_suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let temporary = bin_dir.join(format!(
            ".applicationlauncherd-link-{}-{unique_suffix}",
            std::process::id()
        ));
        symlink(&binary, &temporary).map_err(|err| err.to_string())?;
        std::fs::rename(&temporary, &link).map_err(|err| {
            let _ = std::fs::remove_file(&temporary);
            err.to_string()
        })?;
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
    let content_hash = std::fs::read(&binary)
        .map(|contents| {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            contents.hash(&mut hasher);
            hasher.finish()
        })
        .unwrap_or_default();
    let build_fingerprint = format!("{}-{}-{content_hash:x}", binary_metadata.len(), modified);
    let unit = format!(
        "[Unit]\nDescription=Application Launcher window and session tracker\nAfter=graphical-session.target\nPartOf=graphical-session.target\n\n[Service]\nType=simple\nEnvironment=APPLICATIONLAUNCHER_DAEMON_BUILD={build_fingerprint}\nExecStart={}\nRestart=on-failure\nRestartSec=1\nKillMode=process\n\n[Install]\nWantedBy=graphical-session.target\n",
        link.display()
    );
    let unit_path = unit_dir.join("applicationlauncherd.service");
    if std::fs::read_to_string(&unit_path).ok().as_deref() != Some(&unit) {
        crate::process::atomic_write(&unit_path, unit.as_bytes()).map_err(|err| err.to_string())?;
        installation_changed = true;
    }
    if installation_changed {
        let reload = crate::process::status_with_timeout(
            {
                let mut command = Command::new("systemctl");
                command.args(["--user", "daemon-reload"]);
                command
            },
            Duration::from_secs(5),
        )
        .map_err(|err| err.to_string())?;
        if !reload.success() {
            return Err("systemctl --user daemon-reload failed".into());
        }
    }
    let enabled = crate::process::status_with_timeout(
        {
            let mut command = Command::new("systemctl");
            command.args(["--user", "is-enabled", "applicationlauncherd.service"]);
            command
        },
        Duration::from_secs(5),
    )
    .is_ok_and(|status| status.success());
    if !enabled {
        let enable = crate::process::status_with_timeout(
            {
                let mut command = Command::new("systemctl");
                command.args(["--user", "enable", "applicationlauncherd.service"]);
                command
            },
            Duration::from_secs(5),
        )
        .map_err(|err| err.to_string())?;
        if !enable.success() {
            return Err("systemctl --user enable applicationlauncherd.service failed".into());
        }
    }
    let active = crate::process::status_with_timeout(
        {
            let mut command = Command::new("systemctl");
            command.args(["--user", "is-active", "applicationlauncherd.service"]);
            command
        },
        Duration::from_secs(5),
    )
    .is_ok_and(|status| status.success());
    if installation_changed || !active {
        let service_action = if installation_changed {
            "restart"
        } else {
            "start"
        };
        let restart = crate::process::status_with_timeout(
            {
                let mut command = Command::new("systemctl");
                command.args(["--user", service_action, "applicationlauncherd.service"]);
                command
            },
            Duration::from_secs(5),
        )
        .map_err(|err| err.to_string())?;
        if !restart.success() {
            return Err(format!(
                "systemctl --user {service_action} applicationlauncherd.service failed"
            ));
        }
    }
    Ok(())
}

pub(crate) fn ensure_kwin_feed_installed() -> Result<(), String> {
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is not set")?);
    let script_dir = home.join(".local/share/kwin/scripts").join(KWIN_SCRIPT_ID);
    let code_dir = script_dir.join("contents/code");
    std::fs::create_dir_all(&code_dir).map_err(|err| err.to_string())?;
    let metadata_path = script_dir.join("metadata.json");
    let main_path = code_dir.join("main.js");
    let files_changed = std::fs::read_to_string(&metadata_path).ok().as_deref()
        != Some(KWIN_METADATA)
        || std::fs::read_to_string(&main_path).ok().as_deref() != Some(KWIN_MAIN_JS);
    if files_changed {
        crate::process::atomic_write(&metadata_path, KWIN_METADATA.as_bytes())
            .map_err(|err| err.to_string())?;
        crate::process::atomic_write(&main_path, KWIN_MAIN_JS.as_bytes())
            .map_err(|err| err.to_string())?;
    }
    let enabled = crate::process::status_with_timeout(
        {
            let mut command = Command::new("kwriteconfig6");
            command.args([
                "--file",
                "kwinrc",
                "--group",
                "Plugins",
                "--key",
                &format!("{KWIN_SCRIPT_ID}Enabled"),
                "true",
            ]);
            command
        },
        Duration::from_secs(5),
    )
    .map_err(|err| err.to_string())?;
    if !enabled.success() {
        return Err("Could not enable the KWin window feed script".into());
    }
    // Restarting the script on every daemon start forces its initial snapshot
    // through the newly connected D-Bus object after a daemon restart.
    reload_kwin()
}

fn kwin_script_loaded() -> bool {
    crate::process::output_with_timeout(
        {
            let mut command = Command::new("qdbus6");
            command.args([
                "org.kde.KWin",
                "/Scripting",
                "org.kde.kwin.Scripting.isScriptLoaded",
                KWIN_SCRIPT_ID,
            ]);
            command
        },
        Duration::from_secs(5),
    )
    .ok()
    .is_some_and(|output| {
        output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true"
    })
}

fn kwin_scripting_lock() -> std::sync::MutexGuard<'static, ()> {
    KWIN_SCRIPTING_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn reload_kwin() -> Result<(), String> {
    let _guard = kwin_scripting_lock();
    unload_kwin();
    load_kwin_unlocked()
}

fn recover_kwin_feed() -> Result<(), String> {
    let _guard = kwin_scripting_lock();
    if kwin_script_loaded() {
        return Ok(());
    }
    load_kwin_unlocked()
}

fn unload_kwin() {
    let _ = crate::process::status_with_timeout(
        {
            let mut command = Command::new("qdbus6");
            command.args([
                "org.kde.KWin",
                "/Scripting",
                "org.kde.kwin.Scripting.unloadScript",
                KWIN_SCRIPT_ID,
            ]);
            command
        },
        Duration::from_secs(5),
    );
}

fn load_kwin_unlocked() -> Result<(), String> {
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is not set")?);
    let script = home
        .join(".local/share/kwin/scripts")
        .join(KWIN_SCRIPT_ID)
        .join("contents/code/main.js");
    let loaded = crate::process::output_with_timeout(
        {
            let mut command = Command::new("qdbus6");
            command.args([
                "org.kde.KWin",
                "/Scripting",
                "org.kde.kwin.Scripting.loadScript",
                &script.display().to_string(),
                KWIN_SCRIPT_ID,
            ]);
            command
        },
        Duration::from_secs(5),
    )
    .map_err(|err| err.to_string())?;
    let script_number = String::from_utf8_lossy(&loaded.stdout)
        .trim()
        .parse::<i32>()
        .unwrap_or(-1);
    if !loaded.status.success() || script_number < 0 {
        return Err("KWin rejected the window feed script load".into());
    }
    let started = crate::process::status_with_timeout(
        {
            let mut command = Command::new("qdbus6");
            command.args(["org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting.start"]);
            command
        },
        Duration::from_secs(5),
    )
    .map_err(|err| err.to_string())?;
    started
        .success()
        .then_some(())
        .ok_or_else(|| "KWin rejected the window feed script start".into())
}

fn kdotool_running() -> bool {
    let Ok(processes) = std::fs::read_dir("/proc") else {
        return false;
    };

    processes.filter_map(Result::ok).any(|entry| {
        let file_name = entry.file_name();
        let Some(pid) = file_name.to_str() else {
            return false;
        };
        if !pid.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }

        std::fs::read_to_string(entry.path().join("comm"))
            .is_ok_and(|comm| comm.trim() == "kdotool")
    })
}

pub(crate) fn start_kwin_feed_watchdog() {
    std::thread::spawn(|| {
        let mut state = KwinFeedWatchdogState::default();
        loop {
            std::thread::sleep(KWIN_FEED_WATCHDOG_POLL_INTERVAL);
            let loaded = kwin_script_loaded();
            let scripting_busy = !loaded && kdotool_running();
            if state.should_recover(Instant::now(), loaded, scripting_busy)
                && let Err(err) = recover_kwin_feed()
            {
                eprintln!("Tracker KWin feed watchdog failed: {err}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{KWIN_FEED_MISSING_GRACE, KWIN_MAIN_JS, KwinFeedWatchdogState};

    #[test]
    fn kwin_feed_avoids_unsupported_browser_timers() {
        assert!(!KWIN_MAIN_JS.contains("setTimeout"));
        assert!(!KWIN_MAIN_JS.contains("clearTimeout"));
    }

    #[test]
    fn watchdog_debounces_a_transient_missing_feed() {
        let started = Instant::now();
        let mut state = KwinFeedWatchdogState::default();

        assert!(!state.should_recover(started, false, false));
        assert!(!state.should_recover(
            started + KWIN_FEED_MISSING_GRACE - Duration::from_millis(1),
            false,
            false,
        ));
        assert!(state.should_recover(started + KWIN_FEED_MISSING_GRACE, false, false,));
    }

    #[test]
    fn watchdog_requires_a_quiet_period_after_kdotool() {
        let started = Instant::now();
        let mut state = KwinFeedWatchdogState::default();

        assert!(!state.should_recover(started, false, false));
        assert!(!state.should_recover(started + KWIN_FEED_MISSING_GRACE, false, true,));
        assert!(!state.should_recover(
            started + KWIN_FEED_MISSING_GRACE + Duration::from_millis(1),
            false,
            false,
        ));
        assert!(state.should_recover(started + KWIN_FEED_MISSING_GRACE * 2, false, false,));
    }

    #[test]
    fn watchdog_resets_after_the_feed_returns() {
        let started = Instant::now();
        let mut state = KwinFeedWatchdogState::default();

        assert!(!state.should_recover(started, false, false));
        assert!(!state.should_recover(started + Duration::from_secs(1), true, false));
        assert!(!state.should_recover(started + KWIN_FEED_MISSING_GRACE, false, false,));
    }
}
