//! Optional, expiring activation for scalar lifecycle timing. No restart actions.
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

static DEADLINE_MS: AtomicU64 = AtomicU64::new(0);
const MAX_TTL_SECONDS: u32 = 300;
pub const PATH: &str = "/LifecycleTiming";
pub const INTERFACE: &str = "com.terrydaktal.ApplicationLauncher.LifecycleTiming1";

pub struct Service;

#[zbus::interface(name = "com.terrydaktal.ApplicationLauncher.LifecycleTiming1")]
impl Service {
    fn activate_update_timings(&self, seconds: u32) -> zbus::fdo::Result<String> {
        activate(seconds)
            .map(|value| value.to_string())
            .map_err(zbus::fdo::Error::Failed)
    }
}

pub fn activate(seconds: u32) -> Result<Value, String> {
    if seconds > MAX_TTL_SECONDS {
        return Err("Update timing activation must be between 0 and 300 seconds".into());
    }
    if !crate::observability::category_enabled("update-timings") {
        return Err(
            "The global APPLICATIONLAUNCHER_DIAGNOSTICS=minimal switch disables update timings"
                .into(),
        );
    }
    let deadline = if seconds == 0 {
        0
    } else {
        (crate::tracker::now_ms().max(0) as u64).saturating_add(u64::from(seconds) * 1000)
    };
    DEADLINE_MS.store(deadline, Ordering::Release);
    Ok(
        json!({"category":"update-timings","deadline_unix_ms":deadline,"max_ttl_seconds":MAX_TTL_SECONDS,"retained_files":64,"max_file_bytes":131072}),
    )
}

pub(super) fn prepare_job(request: &std::path::Path) {
    let deadline = DEADLINE_MS.load(Ordering::Acquire);
    if deadline == 0 || !crate::observability::category_enabled("update-timings") {
        return;
    }
    if deadline <= crate::tracker::now_ms().max(0) as u64 {
        return;
    }
    if let Some(directory) = request.parent() {
        let lease =
            json!({"schema_version":1,"deadline_unix_ms":deadline,"build_id":crate::BUILD_ID});
        if super::write_private(&directory.join("timing-request.json"), &lease).is_err() {
            crate::observability::record(crate::observability::Event::new(
                "lifecycle",
                "timing-lease-unavailable",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_is_bounded_opt_in_and_cannot_override_the_global_minimal_switch() {
        const CHILD: &str = "APPLICATIONLAUNCHER_TIMING_GLOBAL_SWITCH_TEST";
        if std::env::var(CHILD).as_deref() == Ok("1") {
            assert!(activate(120).unwrap_err().contains("global"));
            return;
        }
        let directory =
            std::env::temp_dir().join(format!("launcher-timing-activation-{}", std::process::id()));
        super::super::private_directory(&directory).unwrap();
        let request = directory.join("request.json");
        activate(0).unwrap();
        prepare_job(&request);
        assert!(!directory.join("timing-request.json").exists());
        assert!(activate(301).is_err());
        let lease = activate(120).unwrap();
        assert!(lease["deadline_unix_ms"].as_u64().unwrap() > crate::tracker::now_ms() as u64);
        prepare_job(&request);
        let captured = super::super::read_private(&directory.join("timing-request.json")).unwrap();
        assert_eq!(captured["deadline_unix_ms"], lease["deadline_unix_ms"]);
        assert_eq!(captured["build_id"], crate::BUILD_ID);
        activate(0).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "lifecycle::timing::tests::activation_is_bounded_opt_in_and_cannot_override_the_global_minimal_switch"])
            .env(CHILD, "1").env("APPLICATIONLAUNCHER_DIAGNOSTICS", "minimal").status().unwrap();
        assert!(result.success());
    }
}
