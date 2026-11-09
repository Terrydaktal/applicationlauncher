use std::collections::{HashMap, HashSet};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use eframe::egui;
use serde_json::Value;

use super::process_tree::process_identity_is_current;
use crate::models::{ProcessIdentity, ProcessTreeSnapshot, WindowInfo};

const MAX_JSON_BYTES: usize = 2 * 1024 * 1024;
const MAX_RECORDS: usize = 1024;
const QUERY_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Target {
    pub socket: PathBuf,
    pub server: ProcessIdentity,
    executable_identity: Option<(u64, u64)>,
}

impl Target {
    pub fn for_window(window: &WindowInfo, snapshot: &ProcessTreeSnapshot) -> Option<Self> {
        let pane = window.tmux_pane.as_ref()?;
        let node = snapshot
            .nodes
            .iter()
            .find(|node| node.identity.pid == pane.session.server_pid)?;
        Some(Self {
            socket: PathBuf::from(&pane.session.socket_path),
            server: node.identity,
            executable_identity: node.executable_identity,
        })
    }
}

#[derive(Debug)]
pub(crate) struct Inventory {
    pub data: Value,
}

impl Inventory {
    fn parse(bytes: &[u8], target: &Target) -> Result<Self, String> {
        if bytes.len() >= MAX_JSON_BYTES {
            return Err("tmux-mosh returned oversized metadata".into());
        }
        let data: Value = serde_json::from_slice(bytes)
            .map_err(|error| format!("Invalid tmux-mosh JSON: {error}"))?;
        if data["socket"].as_str() != target.socket.to_str()
            || data["server_running"] != true
            || data["server_identity"][0].as_i64() != Some(i64::from(target.server.pid))
            || data["server_identity"][1].as_u64() != Some(target.server.start_ticks)
        {
            return Err("tmux-mosh inventory belongs to a different or replaced server".into());
        }
        let server = &data["servers"]["tmux"];
        if !server.is_null()
            && (server["pid"].as_i64() != Some(i64::from(target.server.pid))
                || server["started_ticks"].as_u64() != Some(target.server.start_ticks))
        {
            return Err("tmux-mosh returned inconsistent server identity".into());
        }
        for key in ["clients", "sessions"] {
            let records = data[key]
                .as_array()
                .ok_or_else(|| format!("Missing tmux-mosh {key} inventory"))?;
            if records.len() > MAX_RECORDS || records.iter().any(|record| !record.is_object()) {
                return Err(format!("Invalid or oversized tmux-mosh {key} inventory"));
            }
        }
        let mut clients = HashSet::new();
        for client in data["clients"].as_array().unwrap() {
            let pid = client["pid"]
                .as_i64()
                .filter(|pid| *pid > 0 && *pid <= i64::from(i32::MAX));
            let started = client["started_ticks"].as_u64();
            if pid.is_none() || !clients.insert((pid, started)) {
                return Err("tmux-mosh returned invalid or duplicate client identities".into());
            }
        }
        let mut sessions = HashSet::new();
        for session in data["sessions"].as_array().unwrap() {
            let id = session["id"].as_str().filter(|id| !id.is_empty());
            if id.is_none() || !sessions.insert(id) {
                return Err("tmux-mosh returned invalid or duplicate session identities".into());
            }
        }
        Ok(Self { data })
    }

    pub fn clients(&self) -> &[Value] {
        self.data["clients"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn sessions(&self) -> &[Value] {
        self.data["sessions"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn client(&self, identity: ProcessIdentity) -> Option<&Value> {
        let mut matches = self.clients().iter().filter(|client| {
            client["pid"].as_i64() == Some(i64::from(identity.pid))
                && client["started_ticks"].as_u64() == Some(identity.start_ticks)
        });
        let client = matches.next()?;
        matches.next().is_none().then_some(client)
    }
}

type ResultData = Result<Arc<Inventory>, String>;

#[derive(Default)]
struct Capture {
    result: Option<ResultData>,
    viewers: Vec<(egui::Context, egui::ViewportId)>,
}

type SharedCapture = Arc<Mutex<Capture>>;
type Pending = Mutex<HashMap<Target, Weak<Mutex<Capture>>>>;
static PENDING: OnceLock<Pending> = OnceLock::new();
static COLLECTOR: Mutex<()> = Mutex::new(());

pub(crate) struct Lookup {
    pub target: Target,
    capture: SharedCapture,
}

impl Lookup {
    pub fn start(target: Target, ctx: egui::Context, viewport: egui::ViewportId) -> Self {
        Self::start_with(target, ctx, viewport, collect)
    }

    fn start_with(
        target: Target,
        ctx: egui::Context,
        viewport: egui::ViewportId,
        collect: impl FnOnce(&Target) -> ResultData + Send + 'static,
    ) -> Self {
        let mut pending = PENDING
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        pending.retain(|_, capture| capture.strong_count() > 0);
        if let Some(capture) = pending.get(&target).and_then(Weak::upgrade) {
            let mut state = capture.lock().unwrap_or_else(|error| error.into_inner());
            if state.result.is_none() {
                state.viewers.push((ctx, viewport));
                drop(state);
                return Self { target, capture };
            }
        }
        let capture = Arc::new(Mutex::new(Capture {
            result: None,
            viewers: vec![(ctx, viewport)],
        }));
        let weak = Arc::downgrade(&capture);
        pending.insert(target.clone(), weak.clone());
        drop(pending);
        let request = target.clone();
        applicationlauncher::observability::spawn_named("tmux-info", move |worker| {
            worker.set_state("waiting-for-inventory");
            // Share in-flight work per server and serialize collectors across servers.
            let _permit = COLLECTOR.lock().unwrap_or_else(|error| error.into_inner());
            if weak.strong_count() == 0 {
                return;
            }
            worker.set_state("collecting-read-only-tmux-mosh-inventory");
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| collect(&request)))
                    .unwrap_or_else(
                        |_| Err("tmux-mosh metadata collector did not complete".into()),
                    );
            if let Some(capture) = weak.upgrade() {
                let viewers = {
                    let mut capture = capture.lock().unwrap_or_else(|error| error.into_inner());
                    capture.result = Some(result);
                    std::mem::take(&mut capture.viewers)
                };
                for (ctx, viewport) in viewers {
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    ctx.request_repaint_of(viewport);
                }
            }
        });
        Self { target, capture }
    }

    pub fn result(&self) -> Option<ResultData> {
        self.capture
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .result
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn completed(target: Target, bytes: &[u8]) -> Self {
        let result = Some(Inventory::parse(bytes, &target).map(Arc::new));
        Self {
            target,
            capture: Arc::new(Mutex::new(Capture {
                result,
                viewers: Vec::new(),
            })),
        }
    }
}

fn inventory_command(helper: &Path, tmux: &Path, socket: &Path) -> Command {
    let mut command = Command::new(helper);
    command
        .arg("--tmux")
        .arg(tmux)
        .arg("--socket")
        .arg(socket)
        .args(["clients", "--json"])
        .stdin(Stdio::null())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env_remove("PYTHONPATH")
        .env_remove("PYTHONHOME");
    command
}

fn validate_target(target: &Target) -> Result<(), String> {
    if !target.socket.is_absolute()
        || !process_identity_is_current(target.server, target.executable_identity)
    {
        return Err("The captured tmux server exited or changed identity".into());
    }
    let uid = rustix::process::getuid().as_raw();
    let socket = std::fs::metadata(&target.socket)
        .map_err(|error| format!("Tmux listener unavailable: {error}"))?;
    let process = std::fs::metadata(format!("/proc/{}", target.server.pid))
        .map_err(|error| format!("Tmux server unavailable: {error}"))?;
    if !socket.file_type().is_socket() || socket.uid() != uid || process.uid() != uid {
        return Err("Tmux server and listener must belong to the current user".into());
    }
    Ok(())
}

fn collect(target: &Target) -> ResultData {
    validate_target(target)?;
    let helper = applicationlauncher::process::executable_path("tmux-mosh")
        .ok_or("tmux-mosh is not installed; its read-only clients --json inventory is required")?;
    let tmux =
        applicationlauncher::process::executable_path("tmux").ok_or("tmux is not installed")?;
    let output = applicationlauncher::process::output_with_timeout(
        inventory_command(&helper, &tmux, &target.socket),
        QUERY_TIMEOUT,
    )
    .map_err(|error| format!("Could not read tmux-mosh inventory: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "tmux-mosh inventory unavailable: {}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(1024)
                .collect::<String>()
                .trim()
        ));
    }
    let inventory = Inventory::parse(&output.stdout, target)?;
    validate_target(target)?;
    Ok(Arc::new(inventory))
}

#[cfg(test)]
mod tests;
