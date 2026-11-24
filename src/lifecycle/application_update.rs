//! Confirmed lifecycle operations, independent of the launcher GUI and tracker storage.
use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::tracker::{TerminalUpgradePhase, TrackedWindow};

const WORKER: &str = include_str!("worker.py");

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TmuxServerIdentity {
    pub pid: i32,
    pub start_ticks: u64,
    pub socket_path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TmuxRestartPane {
    pub session: String,
    pub window_index: String,
    pub pane_index: String,
    pub application: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TmuxRestartRemoteClient {
    pub pid: i32,
    pub session: String,
    pub transport: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TmuxServerRestartScope {
    pub identity: TmuxServerIdentity,
    pub executable: String,
    pub sessions: Vec<String>,
    pub windows: usize,
    pub terminal_tabs: usize,
    pub panes: Vec<TmuxRestartPane>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_clients: Vec<TmuxRestartRemoteClient>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProgramUpdateIdentity {
    pub pid: i32,
    pub start_ticks: u64,
    pub application: String,
}

impl ProgramUpdateIdentity {
    pub fn validate(&self) -> Result<(), String> {
        if self.pid <= 1
            || self.start_ticks == 0
            || !matches!(self.application.as_str(), "fish" | "codex" | "tmux")
        {
            return Err("Select an exact running Fish, Codex or tmux client process".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TmuxClientRestartScope {
    pub server: TmuxServerIdentity,
    pub session_id: String,
    pub session_name: String,
    pub session_created: u64,
    pub read_only: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProgramRestartScope {
    pub identity: ProgramUpdateIdentity,
    pub executable: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_pane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_socket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_client: Option<TmuxClientRestartScope>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ApplicationUpdateAction {
    pub id: String,
    pub label: String,
    pub eligible: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_server: Option<TmuxServerRestartScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<ProgramRestartScope>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationRestoreStatus {
    #[default]
    Pending,
    Restored,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ApplicationRestorePane {
    pub session: String,
    pub window_index: String,
    pub pane_index: String,
    pub pane_id: String,
    pub application: String,
    pub cwd: String,
    pub status: ApplicationRestoreStatus,
    pub message: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ApplicationRestoration {
    pub server_replaced: bool,
    pub clients_reattached: usize,
    pub finished: bool,
    pub panes: Vec<ApplicationRestorePane>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ApplicationUpdateReport {
    pub operation_id: String,
    pub window_id: String,
    pub title: String,
    pub phase: TerminalUpgradePhase,
    pub actions: Vec<ApplicationUpdateAction>,
    pub selected_action: Option<String>,
    pub backup_path: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restoration: Option<ApplicationRestoration>,
}

struct Job {
    created: Instant,
    report: ApplicationUpdateReport,
    plans: HashMap<String, serde_json::Value>,
}

#[derive(Default)]
pub(crate) struct Registry {
    jobs: VecDeque<Arc<Mutex<Job>>>,
}

pub(crate) fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

impl Registry {
    pub(crate) fn begin(&mut self, window_id: &str) -> Result<(String, Preparation), String> {
        if self
            .jobs
            .iter()
            .filter(|job| job.lock().unwrap().report.phase == TerminalUpgradePhase::Preparing)
            .count()
            >= 2
        {
            return Err(
                "Application inspections are already running; wait before refreshing".into(),
            );
        }
        self.jobs.retain(|job| {
            let job = job.lock().unwrap();
            job.created.elapsed() < Duration::from_secs(600)
                || job.report.phase == TerminalUpgradePhase::Running
        });
        while self.jobs.len() >= 16 {
            if let Some(index) = self
                .jobs
                .iter()
                .position(|job| job.lock().unwrap().report.phase != TerminalUpgradePhase::Running)
            {
                self.jobs.remove(index);
            } else {
                break;
            }
        }
        let id = format!(
            "application-update-{}-{}",
            crate::tracker::now_ms(),
            crate::operation::next_id()
        );
        let job = Arc::new(Mutex::new(Job {
            created: Instant::now(),
            report: ApplicationUpdateReport {
                operation_id: id.clone(),
                window_id: window_id.into(),
                message: "Inspecting this terminal's active programs and restart capabilities..."
                    .into(),
                ..Default::default()
            },
            plans: HashMap::new(),
        }));
        self.jobs.push_back(job.clone());
        Ok((id, Preparation(job)))
    }

    pub(crate) fn report(&self, id: &str) -> Result<ApplicationUpdateReport, String> {
        if let Some(report) = durable_report(id) {
            return Ok(report);
        }
        self.job(id).map(|job| job.lock().unwrap().report.clone())
    }

    fn job(&self, id: &str) -> Result<Arc<Mutex<Job>>, String> {
        self.jobs
            .iter()
            .find(|job| job.lock().unwrap().report.operation_id == id)
            .cloned()
            .ok_or_else(|| "Application update preview expired; inspect again".into())
    }

    pub(crate) fn start(
        &self,
        id: &str,
        action: &str,
        confirmed: bool,
    ) -> Result<Option<Work>, String> {
        if !confirmed {
            return Err("Explicit application restart confirmation is required".into());
        }
        let job = self.job(id)?;
        if self.jobs.iter().any(|other| {
            !Arc::ptr_eq(other, &job)
                && other.lock().unwrap().report.phase == TerminalUpgradePhase::Running
        }) {
            return Err("Another application update is in progress".into());
        }
        let mut state = job.lock().unwrap();
        if state.report.selected_action.as_deref() == Some(action)
            && matches!(
                state.report.phase,
                TerminalUpgradePhase::Running | TerminalUpgradePhase::Complete
            )
        {
            return Ok(None);
        }
        if state.report.phase != TerminalUpgradePhase::Ready
            || state.created.elapsed() >= Duration::from_secs(300)
        {
            return Err("Application update preview is not ready or has expired".into());
        }
        if !state
            .report
            .actions
            .iter()
            .any(|row| row.id == action && row.eligible)
        {
            return Err("This application restart is unavailable or unsupported".into());
        }
        let plan = state
            .plans
            .get(action)
            .cloned()
            .ok_or("The verified restart plan is unavailable")?;
        state.report.phase = TerminalUpgradePhase::Running;
        state.report.selected_action = Some(action.into());
        state.report.message =
            "Backing up and revalidating the selected scope before restarting...".into();
        drop(state);
        Ok(Some(Work { job, plan }))
    }
}

pub(crate) struct Preparation(Arc<Mutex<Job>>);

impl Preparation {
    pub(crate) fn prepare(self, windows: Vec<TrackedWindow>) {
        self.prepare_with(windows, None);
    }

    pub(crate) fn prepare_process(
        self,
        windows: Vec<TrackedWindow>,
        process: ProgramUpdateIdentity,
    ) {
        self.prepare_with(windows, Some(process));
    }

    fn prepare_with(self, windows: Vec<TrackedWindow>, process: Option<ProgramUpdateIdentity>) {
        let (id, window_id) = {
            let job = self.0.lock().unwrap();
            (
                job.report.operation_id.clone(),
                job.report.window_id.clone(),
            )
        };
        let result = (|| {
            let directory = private_directory(&format!("preview-{id}"))?;
            let input = directory.join("request.json");
            let mut payload = serde_json::json!({"window_id":window_id,"windows":windows});
            if let Some(process) = process {
                process.validate()?;
                payload["process"] =
                    serde_json::to_value(process).map_err(|error| error.to_string())?;
            }
            let result = write_private(&input, &payload)
                .and_then(|_| run_worker("preview", &input, Duration::from_secs(90)));
            let _ = std::fs::remove_file(&input);
            let _ = std::fs::remove_dir(&directory);
            result
        })();
        let mut job = self.0.lock().unwrap();
        match result.and_then(|value| {
            Ok((
                serde_json::from_value::<Vec<ApplicationUpdateAction>>(value["actions"].clone())
                    .map_err(|error| error.to_string())?,
                serde_json::from_value::<HashMap<String, serde_json::Value>>(
                    value["plans"].clone(),
                )
                .map_err(|error| error.to_string())?,
                value["title"].as_str().unwrap_or_default().to_owned(),
            ))
        }) {
            Ok((actions, plans, title)) => {
                job.report.actions = actions;
                job.report.title = title;
                job.plans = plans;
                job.report.phase = TerminalUpgradePhase::Ready;
                job.report.message =
                    "Preview only. No application has been stopped or restarted.".into();
            }
            Err(error) => {
                job.report.phase = TerminalUpgradePhase::Failed;
                job.report.message = error;
            }
        }
    }
}

impl Drop for Preparation {
    fn drop(&mut self) {
        let mut job = self.0.lock().unwrap();
        if job.report.phase == TerminalUpgradePhase::Preparing {
            job.report.phase = TerminalUpgradePhase::Failed;
            job.report.message = "Application update inspection stopped before completion".into();
        }
    }
}

pub(crate) struct Work {
    job: Arc<Mutex<Job>>,
    plan: serde_json::Value,
}

impl Work {
    pub(crate) fn run(self) {
        let id = self.job.lock().unwrap().report.operation_id.clone();
        let result = (|| {
            let directory = private_directory(&id)?;
            let backup = directory.join("restart-plan.json");
            write_private(&backup, &self.plan)?;
            self.job.lock().unwrap().report.backup_path = Some(backup.display().to_string());
            write_private(
                &directory.join("report.json"),
                &serde_json::to_value(&self.job.lock().unwrap().report)
                    .map_err(|e| e.to_string())?,
            )?;
            if let Err(error) = super::spawn_job(&id, "program", &backup) {
                write_private(
                    &directory.join("result.json"),
                    &serde_json::json!({"ok":false,"message":error}),
                )?;
                return Err(error);
            }
            super::wait_result(&directory, Duration::from_secs(300))
        })();
        let mut job = self.job.lock().unwrap();
        match result {
            Ok(value) => {
                apply_result(&mut job.report, &value);
            }
            Err(error) => {
                job.report.phase = TerminalUpgradePhase::Failed;
                job.report.message =
                    format!("{error}. Do not retry blindly; inspect the retained restart backup.");
            }
        }
    }
}

fn durable_report(id: &str) -> Option<ApplicationUpdateReport> {
    if !id.starts_with("application-update-") || !super::valid_id(id) {
        return None;
    }
    let directory = crate::tracker::state_dir()
        .join("application-updates")
        .join(id);
    report_from_directory(id, &directory)
}

fn report_from_directory(id: &str, directory: &std::path::Path) -> Option<ApplicationUpdateReport> {
    let mut report: ApplicationUpdateReport =
        serde_json::from_value(super::read_private(&directory.join("report.json")).ok()?).ok()?;
    if report.operation_id != id {
        return None;
    }
    if report.selected_action.as_deref() == Some("tmux_server") {
        if let Ok(value) = super::read_private(&directory.join("restore-status.json")) {
            report.restoration = serde_json::from_value(value).ok();
            if report.phase == TerminalUpgradePhase::Running && report.restoration.is_some() {
                report.message = "Server replacement and local tab reattachment completed; verifying restored applications.".into();
            }
        }
    }
    if let Ok(result) = super::read_private(&directory.join("result.json")) {
        apply_result(&mut report, &result);
    }
    if report.phase == TerminalUpgradePhase::Running
        && !super::worker_pending_or_alive(directory, &directory.join("restart-plan.json"))
    {
        report.phase = TerminalUpgradePhase::Failed;
        report.message = "Lifecycle worker exited without a completion record. Inspect the retained recovery plan; no automatic retry was attempted".into();
    }
    Some(report)
}

fn apply_result(report: &mut ApplicationUpdateReport, result: &serde_json::Value) {
    report.phase = if result["ok"] == true {
        TerminalUpgradePhase::Complete
    } else {
        TerminalUpgradePhase::Failed
    };
    report.message = result["message"]
        .as_str()
        .unwrap_or("Invalid lifecycle worker result; inspect recovery records")
        .into();
    if let Some(value) = result.get("restoration") {
        match serde_json::from_value::<ApplicationRestoration>(value.clone()) {
            Ok(restoration) => {
                if result["ok"] == true
                    && (!restoration.finished
                        || !restoration.server_replaced
                        || restoration.panes.is_empty()
                        || restoration
                            .panes
                            .iter()
                            .any(|pane| pane.status == ApplicationRestoreStatus::Pending))
                {
                    report.phase = TerminalUpgradePhase::Failed;
                    report.message = "The worker returned an incomplete restoration result; inspect recovery records before retrying".into();
                }
                report.restoration = Some(restoration);
            }
            Err(_) => {
                report.phase = TerminalUpgradePhase::Failed;
                report.message = "The worker returned invalid restoration details; inspect recovery records before retrying".into();
            }
        }
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let mut job = self.job.lock().unwrap();
        if job.report.phase == TerminalUpgradePhase::Running {
            job.report.phase = TerminalUpgradePhase::Failed;
            job.report.message =
                "Restart worker stopped unexpectedly; inspect the retained backup before retrying"
                    .into();
        }
    }
}

fn private_directory(name: &str) -> Result<PathBuf, String> {
    let parent = crate::tracker::state_dir().join("application-updates");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&parent)
        .map_err(|error| error.to_string())?;
    let metadata = std::fs::symlink_metadata(&parent).map_err(|error| error.to_string())?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(
            "Application update backup directory must be private and owned by this user".into(),
        );
    }
    let directory = parent.join(name);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(|error| error.to_string())?;
    File::open(&parent)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(directory)
}

fn write_private(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("Application update metadata exceeds its safety limit".into());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| error.to_string())?;
    File::open(path.parent().unwrap())
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
}

fn run_worker(mode: &str, input: &Path, timeout: Duration) -> Result<serde_json::Value, String> {
    let mut command = Command::new("/usr/bin/python3");
    command.arg("-I").arg("-c").arg(WORKER).arg(mode).arg(input);
    let output =
        crate::process::output_with_timeout(command, timeout).map_err(|error| error.to_string())?;
    if output.stdout.len() > 2 * 1024 * 1024 {
        return Err("Application restart worker returned oversized metadata".into());
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        "Application restart worker returned no valid report; Python 3 and PyGObject/Gio are required".to_owned()
    })?;
    if !output.status.success() || value.get("error").is_some() {
        return Err(value["error"]
            .as_str()
            .unwrap_or("Application restart worker failed")
            .into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restoration_result() -> serde_json::Value {
        serde_json::json!({
            "ok": true, "message": "Server updated; one application needs attention",
            "restoration": {"server_replaced": true, "clients_reattached": 1, "finished": true,
                "panes": [
                    {"session":"work", "window_index":"0", "pane_index":"0", "pane_id":"%10", "application":"codex", "cwd":"/work", "status":"restored", "message":"Exact conversation verified"},
                    {"session":"girls", "window_index":"0", "pane_index":"0", "pane_id":"%11", "application":"pi", "cwd":"/girls", "status":"failed", "message":"Application launcher exited (status 2)"}
                ]}
        })
    }

    #[test]
    fn application_restore_warnings_survive_durable_reports_without_failing_the_server_update() {
        let id = format!(
            "application-update-results-{}-{}",
            std::process::id(),
            crate::tracker::now_ms()
        );
        let directory = std::env::temp_dir().join(&id);
        super::super::private_directory(&directory).unwrap();
        let report = ApplicationUpdateReport {
            operation_id: id.clone(),
            selected_action: Some("tmux_server".into()),
            phase: TerminalUpgradePhase::Running,
            ..Default::default()
        };
        write_private(
            &directory.join("report.json"),
            &serde_json::to_value(&report).unwrap(),
        )
        .unwrap();
        let result = restoration_result();
        write_private(&directory.join("result.json"), &result).unwrap();
        let restored = report_from_directory(&id, &directory).unwrap();
        assert_eq!(restored.phase, TerminalUpgradePhase::Complete);
        let status = restored.restoration.unwrap();
        assert!(status.finished && status.server_replaced);
        assert_eq!(status.panes[0].status, ApplicationRestoreStatus::Restored);
        assert_eq!(status.panes[1].status, ApplicationRestoreStatus::Failed);
        assert!(status.panes[1].message.contains("status 2"));
        std::fs::remove_dir_all(directory).unwrap();
        let mut older = serde_json::to_value(report).unwrap();
        older.as_object_mut().unwrap().remove("restoration");
        assert!(
            serde_json::from_value::<ApplicationUpdateReport>(older)
                .unwrap()
                .restoration
                .is_none()
        );
    }

    #[test]
    fn incomplete_or_invalid_restoration_results_cannot_claim_success() {
        for modification in ["pending", "unfinished", "no-server", "malformed", "empty"] {
            let mut result = restoration_result();
            match modification {
                "pending" => result["restoration"]["panes"][0]["status"] = "pending".into(),
                "unfinished" => result["restoration"]["finished"] = false.into(),
                "no-server" => result["restoration"]["server_replaced"] = false.into(),
                "malformed" => result["restoration"] = true.into(),
                "empty" => result["restoration"]["panes"] = serde_json::json!([]),
                _ => unreachable!(),
            }
            let mut report = ApplicationUpdateReport::default();
            apply_result(&mut report, &result);
            assert_eq!(report.phase, TerminalUpgradePhase::Failed, "{modification}");
            assert!(report.message.contains("inspect recovery records"));
        }
    }

    #[test]
    fn completed_or_interrupted_program_reports_survive_an_empty_registry() {
        let id = format!(
            "application-update-recovery-{}-{}",
            std::process::id(),
            crate::tracker::now_ms()
        );
        let directory = std::env::temp_dir().join(&id);
        super::super::private_directory(&directory).unwrap();
        let report = ApplicationUpdateReport {
            operation_id: id.clone(),
            phase: TerminalUpgradePhase::Running,
            ..Default::default()
        };
        super::super::write_private(
            &directory.join("report.json"),
            &serde_json::to_value(report).unwrap(),
        )
        .unwrap();
        super::super::write_private(
            &directory.join("worker.json"),
            &serde_json::json!({"pid":std::process::id(),"start":0}),
        )
        .unwrap();
        let recovered = report_from_directory(&id, &directory).unwrap();
        assert_eq!(recovered.phase, TerminalUpgradePhase::Failed);
        assert!(recovered.message.contains("no automatic retry"));
        super::super::write_private(
            &directory.join("result.json"),
            &serde_json::json!({"ok":true,"message":"verified restart"}),
        )
        .unwrap();
        let recovered = report_from_directory(&id, &directory).unwrap();
        assert_eq!(recovered.phase, TerminalUpgradePhase::Complete);
        assert_eq!(recovered.message, "verified restart");
        assert!(report_from_directory("a-different-operation", &directory).is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn program_identities_require_exact_supported_processes_and_scope_survives_serialization() {
        for application in ["fish", "codex", "tmux"] {
            let process = ProgramUpdateIdentity {
                pid: 123,
                start_ticks: 456,
                application: application.into(),
            };
            assert!(process.validate().is_ok());
            for mutation in 0..3 {
                let mut invalid = process.clone();
                match mutation {
                    0 => invalid.pid = 1,
                    1 => invalid.start_ticks = 0,
                    _ => invalid.application = "pi".into(),
                }
                assert!(invalid.validate().is_err());
            }
            let mut value = serde_json::json!({
                "id": "program", "label": "Update", "eligible": true, "message": "preview",
                "program": {"identity": process, "executable": format!("/fixture/{application}"),
                    "cwd": "/fixture/work", "tmux_pane": "%7", "tmux_socket": "/fixture/tmux.sock"}
            });
            if application == "tmux" {
                let program = value["program"].as_object_mut().unwrap();
                program.remove("tmux_pane");
                program.remove("tmux_socket");
                program.insert("tmux_client".into(), serde_json::json!({
                    "server": {"pid": 234, "start_ticks": 567, "socket_path": "/fixture/server.sock"},
                    "session_id": "$1", "session_name": "work", "session_created": 12345, "read_only": true
                }));
            }
            let action: ApplicationUpdateAction = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                action.program.as_ref().unwrap().identity.application,
                application
            );
            assert_eq!(serde_json::to_value(action).unwrap(), value);
            value.as_object_mut().unwrap().remove("program");
            let older: ApplicationUpdateAction = serde_json::from_value(value).unwrap();
            assert!(older.program.is_none());
        }
    }

    #[test]
    fn restart_actions_preserve_structured_server_scope_and_read_older_reports() {
        let mut value = serde_json::json!({
            "id": "tmux_server", "label": "Update server", "eligible": true, "message": "preview",
            "tmux_server": {
                "identity": {"pid": 123, "start_ticks": 456, "socket_path": "/fixture/tmux.sock"},
                "executable": "/fixture/tmux", "sessions": ["work", "detached"],
                "windows": 2, "terminal_tabs": 1,
                "panes": [{"session": "work", "window_index": "3", "pane_index": "0", "application": "codex", "cwd": "/one"}]
            }
        });
        let action: ApplicationUpdateAction = serde_json::from_value(value.clone()).unwrap();
        let scope = action.tmux_server.as_ref().unwrap();
        assert_eq!(scope.identity.start_ticks, 456);
        assert_eq!(scope.sessions, ["work", "detached"]);
        assert_eq!(scope.panes[0].application, "codex");
        assert!(scope.remote_clients.is_empty());
        assert_eq!(serde_json::to_value(action).unwrap(), value);
        value["tmux_server"]["remote_clients"] = serde_json::json!([
            {"pid": 789, "session": "work", "transport": "mosh"}
        ]);
        let action: ApplicationUpdateAction = serde_json::from_value(value.clone()).unwrap();
        let remote = &action.tmux_server.as_ref().unwrap().remote_clients[0];
        assert_eq!(
            (
                remote.pid,
                remote.session.as_str(),
                remote.transport.as_str()
            ),
            (789, "work", "mosh")
        );
        assert_eq!(serde_json::to_value(action).unwrap(), value);
        value.as_object_mut().unwrap().remove("tmux_server");
        let older: ApplicationUpdateAction = serde_json::from_value(value).unwrap();
        assert!(older.tmux_server.is_none());
    }

    #[test]
    fn update_registry_requires_fresh_explicit_scope_and_is_idempotent() {
        let mut registry = Registry::default();
        let (id, preparation) = registry.begin("fixture").unwrap();
        assert!(registry.start(&id, "program", true).is_err());
        {
            let mut job = preparation.0.lock().unwrap();
            job.report.phase = TerminalUpgradePhase::Ready;
            job.report.actions = vec![ApplicationUpdateAction {
                id: "program".into(),
                label: "Codex".into(),
                eligible: true,
                message: "fixture".into(),
                tmux_server: None,
                program: None,
            }];
            job.plans
                .insert("program".into(), serde_json::json!({"fixture":true}));
        }
        assert!(registry.start(&id, "program", false).is_err());
        assert!(registry.start(&id, "unknown", true).is_err());
        let work = registry.start(&id, "program", true).unwrap().unwrap();
        assert!(registry.start(&id, "program", true).unwrap().is_none());
        assert!(registry.start(&id, "tmux_server", true).is_err());
        drop(work);
        assert_eq!(
            registry.report(&id).unwrap().phase,
            TerminalUpgradePhase::Failed
        );
        // No Python worker, signals or session bus are used by this test.
    }

    #[test]
    fn changed_or_expired_restart_preview_cannot_be_committed() {
        let mut registry = Registry::default();
        let (id, preparation) = registry.begin("fixture").unwrap();
        let mut job = preparation.0.lock().unwrap();
        job.report.phase = TerminalUpgradePhase::Ready;
        job.created = Instant::now() - Duration::from_secs(301);
        drop(job);
        assert!(registry.start(&id, "program", true).is_err());
        assert!(registry.report("missing").is_err());
    }

    #[test]
    fn backup_is_private_durable_and_never_overwrites_an_existing_file_or_symlink() {
        let root = std::env::temp_dir().join(format!(
            "launcher-update-backup-test-{}-{}",
            std::process::id(),
            crate::operation::next_id()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("plan.json");
        write_private(&path, &serde_json::json!({"fixture":true})).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(write_private(&path, &serde_json::json!({"overwrite":true})).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"fixture":true}"#
        );
        let link = root.join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(write_private(&link, &serde_json::json!({"overwrite":true})).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inspection_workers_are_bounded_without_evicting_running_jobs() {
        let mut registry = Registry::default();
        let (_, first) = registry.begin("first").unwrap();
        let (_, _second) = registry.begin("second").unwrap();
        assert!(registry.begin("third").is_err());
        drop(first);
        assert!(registry.begin("third").is_ok());
    }
}
