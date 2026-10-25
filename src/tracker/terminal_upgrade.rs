//! Optional bulk UI adapter. Terminals, not this daemon, own update transactions.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::TrackedWindow;
use serde::{Deserialize, Serialize};

const TERMINAL_PATH: &str = "/org/xfce/Terminal";
const TERMINAL_INTERFACE: &str = "org.xfce.Terminal5";
const UPDATE_PATH: &str = "/org/xfce/Terminal/Update";
const UPDATE_INTERFACE: &str = "org.xfce.Terminal.Update1";
const MAX_JOBS: usize = 16;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalUpgradePhase {
    #[default]
    Preparing,
    Ready,
    Running,
    Complete,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TerminalUpgradeWindow {
    pub window_id: String,
    #[serde(default)]
    pub terminal_window_uuid: String,
    #[serde(default)]
    pub owner_pid: i32,
    pub title: String,
    pub tabs: Vec<String>,
    pub eligible: bool,
    pub recommended: bool,
    pub preserves_processes: bool,
    pub executable: String,
    pub build_id: Option<String>,
    pub sha256: String,
    pub status: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TerminalUpgradeReport {
    pub operation_id: String,
    pub phase: TerminalUpgradePhase,
    // Retained for report compatibility; terminal-owned backups are not tracker snapshots.
    pub backup_snapshot_id: Option<i64>,
    pub windows: Vec<TerminalUpgradeWindow>,
    pub message: String,
}

#[derive(Clone)]
struct Target {
    id: String,
    owner: String,
    uuid: String,
}

struct Job {
    created: Instant,
    report: TerminalUpgradeReport,
    targets: Vec<Target>,
}

#[derive(Default)]
pub(super) struct UpgradeRegistry {
    jobs: VecDeque<(String, Arc<Mutex<Job>>)>,
}

impl UpgradeRegistry {
    pub(super) fn begin(&mut self) -> (String, Option<UpgradePreparation>) {
        if let Some((id, _)) = self.jobs.iter().find(|(_, job)| {
            matches!(
                job.lock().unwrap().report.phase,
                TerminalUpgradePhase::Preparing | TerminalUpgradePhase::Running
            )
        }) {
            return (id.clone(), None);
        }
        for (_, job) in &self.jobs {
            let mut job = job.lock().unwrap();
            if job.report.phase == TerminalUpgradePhase::Ready {
                job.report.phase = TerminalUpgradePhase::Failed;
                job.report.message = "Preview superseded; inspect again".into();
                job.targets.clear();
            }
        }
        while self.jobs.len() >= MAX_JOBS {
            self.jobs.pop_front();
        }
        let id = format!(
            "terminal-request-{}-{}",
            super::now_ms(),
            crate::observability::next_operation_id()
        );
        let job = Arc::new(Mutex::new(Job {
            created: Instant::now(),
            report: TerminalUpgradeReport {
                operation_id: id.clone(),
                message: "Asking terminals about available updates...".into(),
                ..Default::default()
            },
            targets: Vec::new(),
        }));
        self.jobs.push_back((id.clone(), job.clone()));
        (id, Some(UpgradePreparation(job)))
    }

    pub(super) fn report(&self, id: &str) -> Result<TerminalUpgradeReport, String> {
        self.jobs
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, job)| job.lock().unwrap().report.clone())
            .ok_or_else(|| "Terminal update request is unavailable; inspect again".into())
    }

    pub(super) fn start(
        &self,
        id: &str,
        selected: &[String],
        confirmed: bool,
    ) -> Result<Option<UpgradeWork>, String> {
        if !confirmed || selected.is_empty() {
            return Err("Explicit confirmation and at least one terminal are required".into());
        }
        let job = self
            .jobs
            .iter()
            .find(|(key, _)| key == id)
            .ok_or("Terminal update preview is unavailable")?
            .1
            .clone();
        let mut state = job.lock().unwrap();
        if matches!(
            state.report.phase,
            TerminalUpgradePhase::Running | TerminalUpgradePhase::Complete
        ) {
            return Ok(None);
        }
        if state.report.phase != TerminalUpgradePhase::Ready
            || state.created.elapsed() >= Duration::from_secs(300)
        {
            return Err("Terminal update preview is not ready or has expired".into());
        }
        let ids: HashSet<_> = selected.iter().collect();
        if ids.len() != selected.len()
            || ids.iter().any(|id| {
                !state
                    .report
                    .windows
                    .iter()
                    .any(|row| &row.window_id == *id && row.eligible)
            })
        {
            return Err("The selection contains duplicate, unknown or unsupported windows".into());
        }
        let targets = state
            .targets
            .iter()
            .filter(|target| ids.contains(&target.id))
            .cloned()
            .collect();
        state.report.phase = TerminalUpgradePhase::Running;
        for row in &mut state.report.windows {
            row.status = if ids.contains(&row.window_id) {
                "pending"
            } else {
                "skipped"
            }
            .into();
        }
        state.report.message =
            "Requesting terminal-owned updates; confirm non-tmux restarts in each terminal".into();
        drop(state);
        Ok(Some(UpgradeWork { job, targets }))
    }
}

pub(super) struct UpgradePreparation(Arc<Mutex<Job>>);

impl UpgradePreparation {
    pub(super) fn prepare(self, windows: Vec<TrackedWindow>) {
        let result = inspect(&windows);
        let mut job = self.0.lock().unwrap();
        match result {
            Ok((rows, targets)) => {
                job.report.windows = rows;
                job.targets = targets;
                job.report.phase = TerminalUpgradePhase::Ready;
                job.report.message = "No windows changed. Selected terminals manage their own update, confirmation, recovery and placement.".into();
            }
            Err(error) => {
                job.report.phase = TerminalUpgradePhase::Failed;
                job.report.message = error;
            }
        }
    }
}

impl Drop for UpgradePreparation {
    fn drop(&mut self) {
        let mut job = self.0.lock().unwrap();
        if job.report.phase == TerminalUpgradePhase::Preparing {
            job.report.phase = TerminalUpgradePhase::Failed;
            job.report.message = "Terminal inspection did not complete; no update requested".into();
        }
    }
}

fn connection() -> Result<zbus::blocking::Connection, String> {
    zbus::blocking::connection::Builder::session()
        .map_err(|error| error.to_string())?
        .method_timeout(Duration::from_secs(3))
        .build()
        .map_err(|error| error.to_string())
}

fn proxy<'a>(
    connection: &'a zbus::blocking::Connection,
    owner: &'a str,
    path: &'a str,
    interface: &'a str,
) -> Result<zbus::blocking::Proxy<'a>, String> {
    zbus::blocking::Proxy::new(connection, owner, path, interface)
        .map_err(|error| error.to_string())
}

fn json_call<B: serde::Serialize + zbus::zvariant::DynamicType>(
    proxy: &zbus::blocking::Proxy<'_>,
    method: &str,
    body: &B,
) -> Result<serde_json::Value, String> {
    let raw: String = proxy
        .call(method, body)
        .map_err(|error| error.to_string())?;
    if raw.len() > 65_536 {
        return Err("Terminal returned oversized update metadata".into());
    }
    serde_json::from_str(&raw).map_err(|error| error.to_string())
}

fn text(values: &HashMap<String, zbus::zvariant::OwnedValue>, key: &str) -> String {
    values
        .get(key)
        .and_then(|value| String::try_from(value.try_clone().ok()?).ok())
        .unwrap_or_default()
}

type Records = Vec<HashMap<String, zbus::zvariant::OwnedValue>>;

fn inspect(windows: &[TrackedWindow]) -> Result<(Vec<TerminalUpgradeWindow>, Vec<Target>), String> {
    let connection = connection()?;
    let bus =
        zbus::blocking::fdo::DBusProxy::new(&connection).map_err(|error| error.to_string())?;
    let mut owners = HashMap::<i32, Vec<(String, Records)>>::new();
    let mut seen = HashSet::new();
    for name in bus.list_names().map_err(|error| error.to_string())? {
        if name.as_str() != "org.xfce.Terminal5"
            && !name.as_str().starts_with("org.xfce.Terminal5.Instance.")
        {
            continue;
        }
        let Ok(owner) = bus.get_name_owner(name.clone().into()) else {
            continue;
        };
        if !seen.insert(owner.to_string()) {
            continue;
        }
        let Ok(pid) = bus.get_connection_unix_process_id(name.into()) else {
            continue;
        };
        let terminal = proxy(
            &connection,
            owner.as_str(),
            TERMINAL_PATH,
            TERMINAL_INTERFACE,
        )?;
        if let Ok(records) = terminal.call::<_, _, Records>("ListTerminals", &()) {
            if records.len() <= 512 {
                owners
                    .entry(pid as i32)
                    .or_default()
                    .push((owner.to_string(), records));
            }
        }
    }
    let mut rows = Vec::new();
    let mut targets = Vec::new();
    for window in windows
        .iter()
        .filter(|window| window.class.to_lowercase().contains("xfce4-terminal"))
        .take(512)
    {
        let mut row = TerminalUpgradeWindow {
            window_id: window.id.clone(), terminal_window_uuid: String::new(), owner_pid: window.pid,
            title: window.title.clone(), tabs: Vec::new(), eligible: false, recommended: false,
            preserves_processes: false, executable: String::new(), build_id: None, sha256: String::new(),
            status: "unsupported".into(),
            message: "This running terminal lacks the terminal-owned update API; restart it once manually".into(),
        };
        let mut matches = Vec::new();
        if let Some(entries) = owners.get(&window.pid) {
            for (owner, records) in entries {
                let single_window = windows
                    .iter()
                    .filter(|other| other.pid == window.pid)
                    .count()
                    == 1;
                let matching: HashSet<_> = records
                    .iter()
                    .filter(|record| single_window || text(record, "window_title") == window.title)
                    .map(|record| text(record, "window_uuid"))
                    .filter(|uuid| !uuid.is_empty())
                    .collect();
                for uuid in matching {
                    // Identical captions in a shared process cannot identify one KWin window.
                    if windows
                        .iter()
                        .filter(|other| other.pid == window.pid && other.title == window.title)
                        .count()
                        != 1
                    {
                        continue;
                    }
                    matches.push((owner, uuid, records));
                }
            }
        }
        if let [(owner, uuid, records)] = matches.as_slice() {
            let terminal = proxy(&connection, owner, TERMINAL_PATH, TERMINAL_INTERFACE)?;
            if let Ok(status) = json_call(&terminal, "GetUpdateStatus", &(uuid.as_str(),)) {
                row.terminal_window_uuid = uuid.clone();
                row.tabs = records
                    .iter()
                    .filter(|record| text(record, "window_uuid") == *uuid)
                    .map(|record| text(record, "working_directory"))
                    .collect();
                row.eligible = status["updatable"] == true && status["pending"] != true;
                row.recommended = row.eligible;
                row.status = if row.eligible {
                    "ready"
                } else if status["pending"] == true {
                    "pending"
                } else {
                    "up_to_date"
                }
                .into();
                row.message = if row.eligible {
                    "The terminal will validate its tabs and request confirmation where necessary"
                } else {
                    "No available update, or the terminal is already updating"
                }
                .into();
                if row.eligible {
                    targets.push(Target {
                        id: window.id.clone(),
                        owner: (*owner).clone(),
                        uuid: uuid.clone(),
                    });
                }
            }
        }
        rows.push(row);
    }
    Ok((rows, targets))
}

pub(super) struct UpgradeWork {
    job: Arc<Mutex<Job>>,
    targets: Vec<Target>,
}

impl UpgradeWork {
    pub(super) fn window_ids(&self) -> Vec<String> {
        self.targets
            .iter()
            .map(|target| target.id.clone())
            .collect()
    }

    pub(super) fn run(self) {
        for target in &self.targets {
            let result = self.request(target);
            let mut job = self.job.lock().unwrap();
            if let Some(row) = job
                .report
                .windows
                .iter_mut()
                .find(|row| row.window_id == target.id)
            {
                match result {
                    Ok(message) => {
                        row.status = "restarted".into();
                        row.message = message;
                    }
                    Err(message) => {
                        row.status = "failed".into();
                        row.message = message;
                    }
                }
            }
        }
        let mut job = self.job.lock().unwrap();
        job.report.phase = TerminalUpgradePhase::Complete;
        job.report.message = "Terminal-owned update requests finished. No application windows were launched or closed by Application Launcher.".into();
    }

    fn request(&self, target: &Target) -> Result<String, String> {
        let connection = connection()?;
        let terminal = proxy(
            &connection,
            &target.owner,
            TERMINAL_PATH,
            TERMINAL_INTERFACE,
        )?;
        let status = json_call(&terminal, "GetUpdateStatus", &(target.uuid.as_str(),))?;
        if status["updatable"] != true || status["pending"] == true {
            return Err("The terminal is no longer eligible for an update".into());
        }
        let service: String = terminal
            .call("UpdateInPlace", &(target.uuid.as_str(),))
            .map_err(|error| error.to_string())?;
        if !valid_service(&service) {
            return Err("The terminal returned an invalid update service".into());
        }
        let bus =
            zbus::blocking::fdo::DBusProxy::new(&connection).map_err(|error| error.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(420);
        let discovery_deadline = Instant::now() + Duration::from_secs(10);
        let mut owner = None;
        while Instant::now() < deadline {
            let name = zbus::names::BusName::try_from(service.as_str())
                .map_err(|error| error.to_string())?;
            if let Ok(current) = bus.get_name_owner(name) {
                if owner.as_ref().is_some_and(|owner| owner != &current) {
                    return Err("Terminal update helper identity changed".into());
                }
                owner = Some(current.clone());
                let helper = proxy(&connection, current.as_str(), UPDATE_PATH, UPDATE_INTERFACE)?;
                let status = json_call(&helper, "GetStatus", &())?;
                if status["phase"] == "complete" {
                    return Ok(status["message"]
                        .as_str()
                        .unwrap_or("Terminal updated")
                        .into());
                }
                if status["phase"] == "failed" {
                    return Err(status["message"]
                        .as_str()
                        .unwrap_or("Terminal update failed")
                        .into());
                }
            } else if owner.is_some() {
                return Err("Terminal update helper exited before reporting completion".into());
            } else if Instant::now() >= discovery_deadline {
                return Err(
                    "The terminal update helper did not start; check the terminal's error dialog"
                        .into(),
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(
            "Timed out observing the terminal-owned update; it was not retried or force-killed"
                .into(),
        )
    }
}

impl Drop for UpgradeWork {
    fn drop(&mut self) {
        if let Ok(mut job) = self.job.lock()
            && job.report.phase == TerminalUpgradePhase::Running
        {
            job.report.phase = TerminalUpgradePhase::Failed;
            job.report.message =
                "Update observation stopped; terminal-owned helpers were not killed or retried"
                    .into();
        }
    }
}

fn valid_service(service: &str) -> bool {
    service
        .strip_prefix("org.xfce.Terminal5.Update.i")
        .is_some_and(|suffix| {
            suffix.len() == 36
                && suffix.bytes().enumerate().all(|(i, byte)| {
                    if [8, 13, 18, 23].contains(&i) {
                        byte == b'_'
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const FIXTURE_UUID: &str = "21e67a7a-6f26-47c4-9b4b-fb6e3e7776c3";
    const FIXTURE_SERVICE: &str = "org.xfce.Terminal5.Update.i21e67a7a_6f26_47c4_9b4b_fb6e3e7776c3";

    struct TerminalFixture(Arc<AtomicUsize>);

    #[zbus::interface(name = "org.xfce.Terminal5")]
    impl TerminalFixture {
        fn list_terminals(&self) -> Records {
            let mut record = HashMap::new();
            for (key, value) in [
                ("window_uuid", FIXTURE_UUID),
                ("window_title", "Terminal"),
                ("working_directory", "/tmp"),
            ] {
                record.insert(
                    key.to_string(),
                    zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(value))
                        .unwrap(),
                );
            }
            vec![record]
        }

        fn get_update_status(&self, window_uuid: &str) -> String {
            assert_eq!(window_uuid, FIXTURE_UUID);
            r#"{"updatable":true,"pending":false}"#.into()
        }

        fn update_in_place(&self, window_uuid: &str) -> String {
            assert_eq!(window_uuid, FIXTURE_UUID);
            self.0.fetch_add(1, Ordering::SeqCst);
            FIXTURE_SERVICE.into()
        }
    }

    struct HelperFixture;

    #[zbus::interface(name = "org.xfce.Terminal.Update1")]
    impl HelperFixture {
        fn get_status(&self) -> String {
            r#"{"phase":"complete","message":"terminal-owned update completed"}"#.into()
        }
    }

    #[test]
    fn private_bus_update_requests_are_delegated_once_and_ambiguous_windows_are_refused() {
        const PRIVATE: &str = "APPLICATIONLAUNCHER_UPDATE_ADAPTER_TEST_PRIVATE";
        if std::env::var(PRIVATE).as_deref() != Ok("1") {
            let result = std::process::Command::new("dbus-run-session")
                .arg("--").arg(std::env::current_exe().unwrap())
                .args(["--exact", "tracker::terminal_upgrade::tests::private_bus_update_requests_are_delegated_once_and_ambiguous_windows_are_refused", "--nocapture"])
                .env(PRIVATE, "1").status().unwrap();
            assert!(result.success());
            return;
        }
        let requests = Arc::new(AtomicUsize::new(0));
        let _fixture = zbus::blocking::connection::Builder::session()
            .unwrap()
            .name("org.xfce.Terminal5.Instance.ifixup")
            .unwrap()
            .name(FIXTURE_SERVICE)
            .unwrap()
            .serve_at(TERMINAL_PATH, TerminalFixture(requests.clone()))
            .unwrap()
            .serve_at(UPDATE_PATH, HelperFixture)
            .unwrap()
            .build()
            .unwrap();
        let window = TrackedWindow {
            id: "private-fixture".into(),
            title: "Terminal".into(),
            class: "xfce4-terminal".into(),
            pid: std::process::id() as i32,
            ..Default::default()
        };
        let mut duplicate = window.clone();
        duplicate.id = "other-fixture".into();
        let (rows, targets) = inspect(&[window.clone(), duplicate]).unwrap();
        assert!(targets.is_empty());
        assert!(rows.iter().all(|row| !row.eligible));
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        let mut registry = UpgradeRegistry::default();
        let (id, prepare) = registry.begin();
        // A changing caption must not hide a single UUID in a single-window process.
        let mut animated = window.clone();
        animated.title = "Terminal - animated".into();
        prepare.unwrap().prepare(vec![animated]);
        assert!(registry.report(&id).unwrap().windows[0].eligible);
        registry
            .start(&id, &[window.id.clone()], true)
            .unwrap()
            .unwrap()
            .run();
        let report = registry.report(&id).unwrap();
        assert_eq!(report.phase, TerminalUpgradePhase::Complete);
        assert_eq!(report.windows[0].status, "restarted");
        assert!(report.backup_snapshot_id.is_none());
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(registry.start(&id, &[window.id], true).unwrap().is_none());
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn delegated_update_service_is_bounded_and_cannot_target_an_arbitrary_bus_name() {
        assert!(valid_service(
            "org.xfce.Terminal5.Update.i21e67a7a_6f26_47c4_9b4b_fb6e3e7776c3"
        ));
        for name in [
            "org.kde.KWin",
            "org.xfce.Terminal5",
            "org.xfce.Terminal5.Update.i",
            "org.xfce.Terminal5.Update.i../../tmp",
        ] {
            assert!(!valid_service(name));
        }
    }
    #[test]
    fn delegated_previews_are_bounded_single_use_and_require_confirmation() {
        let mut registry = UpgradeRegistry::default();
        let (id, pending) = registry.begin();
        assert!(pending.is_some());
        assert_eq!(registry.begin().0, id);
        assert!(registry.start(&id, &["unknown".into()], false).is_err());
        drop(pending);
        assert_eq!(
            registry.report(&id).unwrap().phase,
            TerminalUpgradePhase::Failed
        );
        for _ in 0..32 {
            let (_, pending) = registry.begin();
            drop(pending);
        }
        assert_eq!(registry.jobs.len(), MAX_JOBS);
    }
}
