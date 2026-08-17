use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU8, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub const FLIGHT_RECORDER_CAPACITY: usize = 512;
pub const MAX_NAMED_WORKERS: usize = 64;
pub const MAX_EVENT_FIELD_BYTES: usize = 96;
pub const MAX_SEMANTIC_SNAPSHOT_BYTES: usize = 1024 * 1024;
pub const OBSERVABLE_CPU_BUDGET_PPM: u64 = 2_500;
pub const EVENT_P99_LATENCY_BUDGET_NS: u64 = 50_000;
pub const OBSERVABLE_RSS_BUDGET_BYTES: u64 = 1024 * 1024;
pub const PRODUCTION_BINARY_SIZE_BUDGET_BYTES: u64 = 64 * 1024 * 1024;
pub const DEBUG_ARTIFACT_SIZE_BUDGET_BYTES: u64 = 512 * 1024 * 1024;
pub const DIAGNOSTIC_CAPTURE_BUDGET_SECS: u64 = 45;
pub const DIAGNOSTIC_OUTPUT_BUDGET_BYTES: u64 = 32 * 1024 * 1024;

const DIAGNOSTIC_REQUEST_LIMIT: usize = 1024;
const DEBUG_ATTACH_TIMEOUT: Duration = Duration::from_secs(60);
const PANIC_LOG_LIMIT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Component {
    Gui,
    Daemon,
    Test,
}

impl Component {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gui => "gui",
            Self::Daemon => "daemon",
            Self::Test => "test",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[repr(u8)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeMode {
    Minimal = 0,
    Observable = 1,
    RuntimeActivated = 2,
}

impl RuntimeMode {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Minimal,
            2 => Self::RuntimeActivated,
            _ => Self::Observable,
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub enum Counter {
    DiagnosticRequests,
    DiagnosticFailures,
    GuiControlRequests,
    FocusRequests,
    KwinSnapshots,
    KwinUpserts,
    KwinRemovals,
    KwinActivations,
    KwinFeedRecoveries,
    PersistenceWrites,
    AttentionAttempts,
    AttentionFailures,
    WorkerPanics,
}

impl Counter {
    const COUNT: usize = 13;
    const ALL: [(Self, &'static str); Self::COUNT] = [
        (Self::DiagnosticRequests, "diagnostic_requests"),
        (Self::DiagnosticFailures, "diagnostic_failures"),
        (Self::GuiControlRequests, "gui_control_requests"),
        (Self::FocusRequests, "focus_requests"),
        (Self::KwinSnapshots, "kwin_snapshots"),
        (Self::KwinUpserts, "kwin_upserts"),
        (Self::KwinRemovals, "kwin_removals"),
        (Self::KwinActivations, "kwin_activations"),
        (Self::KwinFeedRecoveries, "kwin_feed_recoveries"),
        (Self::PersistenceWrites, "persistence_writes"),
        (Self::AttentionAttempts, "attention_attempts"),
        (Self::AttentionFailures, "attention_failures"),
        (Self::WorkerPanics, "worker_panics"),
    ];
}

#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub enum Gauge {
    TrackedWindows,
    GuiWindows,
    InstalledApps,
    AttentionPending,
    PendingFeedEvents,
}

impl Gauge {
    const COUNT: usize = 5;
    const ALL: [(Self, &'static str); Self::COUNT] = [
        (Self::TrackedWindows, "tracked_windows"),
        (Self::GuiWindows, "gui_windows"),
        (Self::InstalledApps, "installed_apps"),
        (Self::AttentionPending, "attention_pending"),
        (Self::PendingFeedEvents, "pending_feed_events"),
    ];
}

#[derive(Clone, Debug, Serialize)]
pub struct FlightEvent {
    pub sequence: u64,
    pub monotonic_us: u64,
    pub unix_time_ms: u64,
    pub thread_id: String,
    pub thread_name: String,
    pub category: String,
    pub action: String,
    pub operation_id: Option<u64>,
    pub parent_event_id: Option<u64>,
    pub object_id: String,
    pub reason: String,
    pub old_state: String,
    pub new_state: String,
    pub duration_us: Option<u64>,
}

pub struct Event<'a> {
    category: &'a str,
    action: &'a str,
    operation_id: Option<u64>,
    parent_event_id: Option<u64>,
    object_id: &'a str,
    reason: &'a str,
    old_state: &'a str,
    new_state: &'a str,
    duration: Option<Duration>,
}

impl<'a> Event<'a> {
    pub const fn new(category: &'a str, action: &'a str) -> Self {
        Self {
            category,
            action,
            operation_id: None,
            parent_event_id: None,
            object_id: "",
            reason: "",
            old_state: "",
            new_state: "",
            duration: None,
        }
    }

    pub const fn operation(mut self, operation_id: u64) -> Self {
        self.operation_id = Some(operation_id);
        self
    }

    pub const fn parent(mut self, parent_event_id: u64) -> Self {
        self.parent_event_id = Some(parent_event_id);
        self
    }

    pub const fn object(mut self, object_id: &'a str) -> Self {
        self.object_id = object_id;
        self
    }

    pub const fn reason(mut self, reason: &'a str) -> Self {
        self.reason = reason;
        self
    }

    pub const fn transition(mut self, old_state: &'a str, new_state: &'a str) -> Self {
        self.old_state = old_state;
        self.new_state = new_state;
        self
    }

    pub const fn duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkerSnapshot {
    pub id: u64,
    pub name: String,
    pub thread_id: String,
    pub started_monotonic_us: u64,
    pub last_transition_monotonic_us: u64,
    pub state: String,
}

#[derive(Debug)]
struct BoundedFlightRecorder {
    capacity: usize,
    events: VecDeque<FlightEvent>,
}

impl BoundedFlightRecorder {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            // Minimal mode never records, so defer the bounded allocation until
            // the first observable or runtime-activated event.
            events: VecDeque::new(),
        }
    }

    fn push(&mut self, event: FlightEvent) {
        if self.capacity == 0 {
            return;
        }
        if self.events.len() == self.capacity {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn snapshot(&self) -> Vec<FlightEvent> {
        self.events.iter().cloned().collect()
    }
}

struct State {
    started: Instant,
    component: OnceLock<Component>,
    configured_mode: AtomicU8,
    mode: AtomicU8,
    sequence: AtomicU64,
    operation_sequence: AtomicU64,
    worker_sequence: AtomicU64,
    counters: [AtomicU64; Counter::COUNT],
    gauges: [AtomicI64; Gauge::COUNT],
    events: Mutex<BoundedFlightRecorder>,
    workers: Mutex<BTreeMap<u64, WorkerSnapshot>>,
    diagnostic_token: AtomicU64,
}

impl State {
    fn new() -> Self {
        let configured_mode = configured_mode();
        Self {
            started: Instant::now(),
            component: OnceLock::new(),
            configured_mode: AtomicU8::new(configured_mode as u8),
            mode: AtomicU8::new(configured_mode as u8),
            sequence: AtomicU64::new(0),
            operation_sequence: AtomicU64::new(0),
            worker_sequence: AtomicU64::new(0),
            counters: std::array::from_fn(|_| AtomicU64::new(0)),
            gauges: std::array::from_fn(|_| AtomicI64::new(0)),
            events: Mutex::new(BoundedFlightRecorder::new(FLIGHT_RECORDER_CAPACITY)),
            workers: Mutex::new(BTreeMap::new()),
            diagnostic_token: AtomicU64::new(0),
        }
    }
}

static STATE: OnceLock<State> = OnceLock::new();

struct AuthorizationTimer {
    deadline: Mutex<Option<(u64, Instant)>>,
    changed: Condvar,
}

static AUTHORIZATION_TIMER: OnceLock<std::sync::Arc<AuthorizationTimer>> = OnceLock::new();

fn state() -> &'static State {
    STATE.get_or_init(State::new)
}

fn configured_mode() -> RuntimeMode {
    match std::env::var("APPLICATIONLAUNCHER_DIAGNOSTICS")
        .unwrap_or_else(|_| "observable".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "minimal" | "off" | "0" => RuntimeMode::Minimal,
        _ => RuntimeMode::Observable,
    }
}

pub fn initialize(component: Component) {
    let state = state();
    let _ = state.component.set(component);
    state
        .configured_mode
        .store(configured_mode() as u8, Ordering::Release);
    if state.mode.load(Ordering::Acquire) != RuntimeMode::RuntimeActivated as u8 {
        state.mode.store(
            state.configured_mode.load(Ordering::Acquire),
            Ordering::Release,
        );
    }
    record(
        Event::new("process", "started")
            .object(component.as_str())
            .reason(crate::BUILD_ID),
    );
}

pub fn mode() -> RuntimeMode {
    RuntimeMode::from_u8(state().mode.load(Ordering::Acquire))
}

pub fn enable_observable_for_doctor() {
    state()
        .configured_mode
        .store(RuntimeMode::Observable as u8, Ordering::Release);
    state()
        .mode
        .store(RuntimeMode::Observable as u8, Ordering::Release);
}

pub fn next_operation_id() -> u64 {
    state().operation_sequence.fetch_add(1, Ordering::Relaxed) + 1
}

pub fn increment(counter: Counter) {
    if mode() == RuntimeMode::Minimal {
        return;
    }
    state().counters[counter as usize].fetch_add(1, Ordering::Relaxed);
}

pub fn add(counter: Counter, value: u64) {
    if mode() == RuntimeMode::Minimal {
        return;
    }
    state().counters[counter as usize].fetch_add(value, Ordering::Relaxed);
}

pub fn set_gauge(gauge: Gauge, value: usize) {
    if mode() == RuntimeMode::Minimal {
        return;
    }
    state().gauges[gauge as usize].store(value.min(i64::MAX as usize) as i64, Ordering::Relaxed);
}

pub fn record(event: Event<'_>) -> Option<u64> {
    if mode() == RuntimeMode::Minimal {
        return None;
    }

    let state = state();
    let sequence = state.sequence.fetch_add(1, Ordering::Relaxed) + 1;
    let thread = std::thread::current();
    let entry = FlightEvent {
        sequence,
        monotonic_us: elapsed_us(state.started),
        unix_time_ms: now_ms(),
        thread_id: truncate_field(&format!("{:?}", thread.id())),
        thread_name: truncate_field(thread.name().unwrap_or("unnamed")),
        category: truncate_field(event.category),
        action: truncate_field(event.action),
        operation_id: event.operation_id,
        parent_event_id: event.parent_event_id,
        object_id: truncate_field(event.object_id),
        reason: redact_event_field(event.reason),
        old_state: truncate_field(event.old_state),
        new_state: truncate_field(event.new_state),
        duration_us: event
            .duration
            .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64),
    };
    state
        .events
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(entry);
    Some(sequence)
}

pub struct WorkerGuard {
    id: u64,
    registered: bool,
}

impl WorkerGuard {
    pub fn set_state(&self, worker_state: &str) {
        if !self.registered || mode() == RuntimeMode::Minimal {
            return;
        }
        let state = state();
        if let Some(worker) = state
            .workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&self.id)
        {
            worker.last_transition_monotonic_us = elapsed_us(state.started);
            worker.state = truncate_field(worker_state);
        }
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        state()
            .workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.id);
        record(Event::new("worker", "stopped").object(&self.id.to_string()));
    }
}

pub fn register_worker(name: &str) -> WorkerGuard {
    if mode() == RuntimeMode::Minimal {
        return WorkerGuard {
            id: 0,
            registered: false,
        };
    }

    let state = state();
    let id = state.worker_sequence.fetch_add(1, Ordering::Relaxed) + 1;
    let thread = std::thread::current();
    let mut workers = state
        .workers
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if workers.len() >= MAX_NAMED_WORKERS {
        return WorkerGuard {
            id,
            registered: false,
        };
    }
    workers.insert(
        id,
        WorkerSnapshot {
            id,
            name: truncate_field(name),
            thread_id: truncate_field(&format!("{:?}", thread.id())),
            started_monotonic_us: elapsed_us(state.started),
            last_transition_monotonic_us: elapsed_us(state.started),
            state: "started".to_string(),
        },
    );
    drop(workers);
    record(
        Event::new("worker", "started")
            .object(&id.to_string())
            .reason(name),
    );
    WorkerGuard {
        id,
        registered: true,
    }
}

pub fn spawn_named<F, T>(name: &'static str, operation: F) -> JoinHandle<T>
where
    F: FnOnce(&WorkerGuard) -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let worker = register_worker(name);
            operation(&worker)
        })
        .unwrap_or_else(|err| panic!("could not spawn {name}: {err}"))
}

#[derive(Serialize)]
pub struct Budgets {
    observable_cpu_ppm: u64,
    event_p99_latency_ns: u64,
    observable_rss_bytes: u64,
    production_binary_bytes: u64,
    debug_artifact_bytes: u64,
    diagnostic_capture_seconds: u64,
    diagnostic_output_bytes: u64,
    flight_recorder_events: usize,
    named_workers: usize,
}

#[derive(Serialize)]
pub struct RuntimeSnapshot {
    schema_version: u32,
    component: &'static str,
    pid: u32,
    build_id: &'static str,
    package_version: &'static str,
    mode: RuntimeMode,
    captured_unix_time_ms: u64,
    uptime_ms: u64,
    counters: BTreeMap<&'static str, u64>,
    gauges: BTreeMap<&'static str, i64>,
    workers: Vec<WorkerSnapshot>,
    events: Vec<FlightEvent>,
    budgets: Budgets,
}

pub fn runtime_snapshot() -> RuntimeSnapshot {
    let state = state();
    RuntimeSnapshot {
        schema_version: 1,
        component: state
            .component
            .get()
            .copied()
            .unwrap_or(Component::Test)
            .as_str(),
        pid: std::process::id(),
        build_id: crate::BUILD_ID,
        package_version: env!("CARGO_PKG_VERSION"),
        mode: mode(),
        captured_unix_time_ms: now_ms(),
        uptime_ms: state
            .started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64,
        counters: Counter::ALL
            .iter()
            .map(|(counter, name)| {
                (
                    *name,
                    state.counters[*counter as usize].load(Ordering::Relaxed),
                )
            })
            .collect(),
        gauges: Gauge::ALL
            .iter()
            .map(|(gauge, name)| (*name, state.gauges[*gauge as usize].load(Ordering::Relaxed)))
            .collect(),
        workers: state
            .workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect(),
        events: state
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .snapshot(),
        budgets: Budgets {
            observable_cpu_ppm: OBSERVABLE_CPU_BUDGET_PPM,
            event_p99_latency_ns: EVENT_P99_LATENCY_BUDGET_NS,
            observable_rss_bytes: OBSERVABLE_RSS_BUDGET_BYTES,
            production_binary_bytes: PRODUCTION_BINARY_SIZE_BUDGET_BYTES,
            debug_artifact_bytes: DEBUG_ARTIFACT_SIZE_BUDGET_BYTES,
            diagnostic_capture_seconds: DIAGNOSTIC_CAPTURE_BUDGET_SECS,
            diagnostic_output_bytes: DIAGNOSTIC_OUTPUT_BUDGET_BYTES,
            flight_recorder_events: FLIGHT_RECORDER_CAPACITY,
            named_workers: MAX_NAMED_WORKERS,
        },
    }
}

pub fn runtime_snapshot_json() -> Result<String, String> {
    let mut snapshot = runtime_snapshot();
    loop {
        let json = serde_json::to_string_pretty(&snapshot).map_err(|err| err.to_string())?;
        if json.len() <= MAX_SEMANTIC_SNAPSHOT_BYTES {
            return Ok(json);
        }
        if snapshot.events.is_empty() {
            return Err("semantic diagnostic snapshot exceeded its hard size bound".to_string());
        }
        let remove = (snapshot.events.len() / 4).max(1);
        snapshot.events.drain(..remove);
    }
}

pub struct DiagnosticServer {
    path: PathBuf,
    wake: Option<std::os::unix::net::UnixStream>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for DiagnosticServer {
    fn drop(&mut self) {
        if let Some(mut wake) = self.wake.take() {
            let _ = wake.write_all(&[1]);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn start_diagnostic_server(component: Component) -> Result<DiagnosticServer, String> {
    let runtime_dir = diagnostic_runtime_dir();
    ensure_private_directory(&runtime_dir)?;
    let path = diagnostic_socket_path(component, std::process::id());
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|err| format!("could not remove stale diagnostic socket: {err}"))?;
    }
    let listener = std::os::unix::net::UnixListener::bind(&path)
        .map_err(|err| format!("could not bind diagnostic socket {}: {err}", path.display()))?;
    #[cfg(unix)]
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|err| format!("could not secure diagnostic socket: {err}"))?;
    let (wake_reader, wake_writer) = std::os::unix::net::UnixStream::pair()
        .map_err(|err| format!("could not create diagnostic wake channel: {err}"))?;
    let thread = std::thread::Builder::new()
        .name(format!("diag-{}", component.as_str()))
        .spawn(move || {
            let worker = register_worker("diagnostic-server");
            worker.set_state("accepting");
            while diagnostic_listener_ready(&listener, &wake_reader) {
                if let Ok((stream, _)) = listener.accept() {
                    handle_diagnostic_connection(stream);
                }
            }
        })
        .map_err(|err| format!("could not start diagnostic server: {err}"))?;
    Ok(DiagnosticServer {
        path,
        wake: Some(wake_writer),
        thread: Some(thread),
    })
}

#[cfg(unix)]
fn diagnostic_listener_ready(
    listener: &std::os::unix::net::UnixListener,
    wake: &std::os::unix::net::UnixStream,
) -> bool {
    let mut descriptors = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let result = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                -1,
            )
        };
        if result > 0 {
            return descriptors[0].revents & libc::POLLIN != 0
                && descriptors[1].revents & libc::POLLIN == 0;
        }
        if result < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return false;
    }
}

fn handle_diagnostic_connection(mut stream: std::os::unix::net::UnixStream) {
    increment(Counter::DiagnosticRequests);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    if !peer_is_current_user(&stream) {
        increment(Counter::DiagnosticFailures);
        let _ = stream.write_all(b"error unauthorized peer\n");
        return;
    }

    let mut request = [0_u8; DIAGNOSTIC_REQUEST_LIMIT];
    let request_len = stream.read(&mut request).unwrap_or(0);
    let request = std::str::from_utf8(&request[..request_len])
        .unwrap_or_default()
        .trim();
    let mut fields = request.split_whitespace();
    match fields.next().unwrap_or_default() {
        "ping" => {
            let component = state()
                .component
                .get()
                .copied()
                .unwrap_or(Component::Test)
                .as_str();
            let _ = writeln!(
                stream,
                "ok {component} {} {}",
                std::process::id(),
                crate::BUILD_ID
            );
        }
        "snapshot" => match runtime_snapshot_json() {
            Ok(snapshot) => {
                let _ = stream.write_all(snapshot.as_bytes());
            }
            Err(err) => {
                increment(Counter::DiagnosticFailures);
                let _ = writeln!(stream, "error {err}");
            }
        },
        "authorize" => {
            let requested_pid = fields.next().and_then(|pid| pid.parse::<u32>().ok());
            let peer_pid = peer_pid(&stream).map(|pid| pid as u32);
            let response = requested_pid
                .filter(|pid| Some(*pid) == peer_pid)
                .ok_or_else(|| "authorization PID did not match the socket peer".to_string())
                .and_then(authorize_debugger);
            match response {
                Ok(token) => {
                    let _ = writeln!(stream, "authorized {token}");
                }
                Err(err) => {
                    increment(Counter::DiagnosticFailures);
                    let _ = writeln!(stream, "error {err}");
                }
            }
        }
        "done" => {
            let token = fields.next().and_then(|token| token.parse::<u64>().ok());
            if token.is_some_and(revoke_debugger) {
                let _ = stream.write_all(b"disabled\n");
            } else {
                let _ = stream.write_all(b"error invalid diagnostic token\n");
            }
        }
        _ => {
            increment(Counter::DiagnosticFailures);
            let _ = stream.write_all(b"error unsupported diagnostic request\n");
        }
    }
}

fn authorize_debugger(pid: u32) -> Result<u64, String> {
    set_debugger(pid)?;
    let token = state().diagnostic_token.fetch_add(1, Ordering::AcqRel) + 1;
    state()
        .mode
        .store(RuntimeMode::RuntimeActivated as u8, Ordering::Release);
    record(
        Event::new("diagnostics", "activated")
            .object(&pid.to_string())
            .reason("authorized-peer"),
    );
    schedule_debugger_revocation(token);
    Ok(token)
}

fn revoke_debugger(token: u64) -> bool {
    if state()
        .diagnostic_token
        .compare_exchange(token, token + 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    cancel_debugger_revocation(token);
    let _ = clear_debugger();
    state().mode.store(
        state().configured_mode.load(Ordering::Acquire),
        Ordering::Release,
    );
    record(Event::new("diagnostics", "deactivated").reason("capture-complete"));
    true
}

fn authorization_timer() -> &'static std::sync::Arc<AuthorizationTimer> {
    AUTHORIZATION_TIMER.get_or_init(|| {
        let timer = std::sync::Arc::new(AuthorizationTimer {
            deadline: Mutex::new(None),
            changed: Condvar::new(),
        });
        let worker_timer = std::sync::Arc::clone(&timer);
        spawn_named("diagnostic-auth-timeout", move |worker| {
            worker.set_state("waiting");
            let mut deadline = worker_timer
                .deadline
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                while deadline.is_none() {
                    deadline = worker_timer
                        .changed
                        .wait(deadline)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                let (token, expires_at) = deadline.expect("authorization deadline initialized");
                let now = Instant::now();
                if now < expires_at {
                    let (updated, wait_result) = worker_timer
                        .changed
                        .wait_timeout(deadline, expires_at.saturating_duration_since(now))
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    deadline = updated;
                    if !wait_result.timed_out() {
                        continue;
                    }
                }
                let expired = deadline
                    .take()
                    .is_some_and(|(current_token, _)| current_token == token);
                drop(deadline);
                if expired {
                    let _ = revoke_debugger(token);
                }
                deadline = worker_timer
                    .deadline
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        });
        timer
    })
}

fn schedule_debugger_revocation(token: u64) {
    let timer = authorization_timer();
    *timer
        .deadline
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
        Some((token, Instant::now() + DEBUG_ATTACH_TIMEOUT));
    timer.changed.notify_one();
}

fn cancel_debugger_revocation(token: u64) {
    let Some(timer) = AUTHORIZATION_TIMER.get() else {
        return;
    };
    let mut deadline = timer
        .deadline
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if deadline.is_some_and(|(current_token, _)| current_token == token) {
        *deadline = None;
        timer.changed.notify_one();
    }
}

#[cfg(target_os = "linux")]
fn set_debugger(pid: u32) -> Result<(), String> {
    use rustix::process::{PTracer, Pid, set_ptracer};

    let pid = Pid::from_raw(pid as i32).ok_or_else(|| "invalid diagnostic PID".to_string())?;
    set_ptracer(PTracer::ProcessID(pid))
        .map_err(|err| format!("could not authorize diagnostic attachment: {err}"))
}

#[cfg(not(target_os = "linux"))]
fn set_debugger(_pid: u32) -> Result<(), String> {
    Err("diagnostic attachment is only supported on Linux".to_string())
}

#[cfg(target_os = "linux")]
fn clear_debugger() -> Result<(), String> {
    rustix::process::set_ptracer(rustix::process::PTracer::None)
        .map_err(|err| format!("could not revoke diagnostic attachment: {err}"))
}

#[cfg(not(target_os = "linux"))]
fn clear_debugger() -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn peer_pid(stream: &std::os::unix::net::UnixStream) -> Option<i32> {
    peer_credentials(stream).map(|credentials| credentials.pid)
}

#[cfg(not(target_os = "linux"))]
fn peer_pid(_stream: &std::os::unix::net::UnixStream) -> Option<i32> {
    None
}

#[cfg(target_os = "linux")]
fn peer_is_current_user(stream: &std::os::unix::net::UnixStream) -> bool {
    peer_credentials(stream)
        .is_some_and(|credentials| credentials.uid == rustix::process::getuid().as_raw())
}

#[cfg(not(target_os = "linux"))]
fn peer_is_current_user(_stream: &std::os::unix::net::UnixStream) -> bool {
    true
}

#[cfg(target_os = "linux")]
fn peer_credentials(stream: &std::os::unix::net::UnixStream) -> Option<libc::ucred> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    (result == 0 && length >= std::mem::size_of::<libc::ucred>() as libc::socklen_t)
        .then_some(credentials)
}

pub fn diagnostic_runtime_dir() -> PathBuf {
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return diagnostic_runtime_dir_for(PathBuf::from(runtime_dir));
    }
    PathBuf::from(format!(
        "/tmp/applicationlauncher-diagnostics-{}",
        rustix::process::getuid().as_raw()
    ))
}

fn diagnostic_runtime_dir_for(runtime_dir: PathBuf) -> PathBuf {
    let candidate = runtime_dir.join("applicationlauncher-diagnostics");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        // sockaddr_un.sun_path is normally 108 bytes including its NUL.
        // Reserve enough room for "/daemon-4294967295.sock".
        if candidate.as_os_str().as_bytes().len() <= 78 {
            return candidate;
        }
        let mut hash = 14_695_981_039_346_656_037_u64;
        for byte in candidate.as_os_str().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(1_099_511_628_211);
        }
        PathBuf::from(format!(
            "/tmp/applicationlauncher-diag-{}-{:016x}",
            rustix::process::getuid().as_raw(),
            hash
        ))
    }
    #[cfg(not(unix))]
    candidate
}

pub fn diagnostic_socket_path(component: Component, pid: u32) -> PathBuf {
    diagnostic_runtime_dir().join(format!("{}-{pid}.sock", component.as_str()))
}

pub fn state_dir() -> PathBuf {
    if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(state_home).join("applicationlauncher");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/state/applicationlauncher");
    }
    diagnostic_runtime_dir()
}

pub fn ensure_private_directory(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path)
        .map_err(|err| format!("could not create {}: {err}", path.display()))?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|err| format!("could not secure {}: {err}", path.display()))?;
    Ok(())
}

pub fn write_private(path: &Path, contents: &[u8]) -> Result<(), String> {
    crate::process::atomic_write(path, contents)
        .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|err| format!("could not secure {}: {err}", path.display()))?;
    Ok(())
}

pub fn install_panic_hook(component: Component) {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let report = redact_diagnostic_text(&format_panic_report(component, panic_info));
        let report = truncate_text(&report, PANIC_LOG_LIMIT_BYTES as usize);
        let directory = state_dir();
        if ensure_private_directory(&directory).is_ok() {
            let latest = directory.join(format!("panic-{}-latest.log", component.as_str()));
            let _ = write_private(&latest, report.as_bytes());

            let history = directory.join(format!("panic-{}.log", component.as_str()));
            append_bounded_private(&history, report.as_bytes(), PANIC_LOG_LIMIT_BYTES);
        }
        let _ = std::io::stderr().lock().write_all(report.as_bytes());
        previous_hook(panic_info);
    }));
}

fn format_panic_report(component: Component, panic_info: &std::panic::PanicHookInfo<'_>) -> String {
    let mut report = format!(
        "\n==== applicationlauncher {} panic ====\nunix_time_ms: {}\nbuild_id: {}\npid: {}\npanic: {}\n",
        component.as_str(),
        now_ms(),
        crate::BUILD_ID,
        std::process::id(),
        panic_info
    );
    if let Some(location) = panic_info.location() {
        report.push_str(&format!(
            "location: {}:{}:{}\n",
            location.file(),
            location.line(),
            location.column()
        ));
    }
    report.push_str(&format!(
        "backtrace:\n{}\n",
        std::backtrace::Backtrace::force_capture()
    ));
    report
}

fn append_bounded_private(path: &Path, entry: &[u8], limit: u64) {
    let entry = &entry[..entry.len().min(limit as usize)];
    let current_size = std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if current_size.saturating_add(entry.len() as u64) > limit {
        let _ = write_private(path, entry);
        return;
    }

    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    if let Ok(mut output) = options.open(path) {
        let _ = output.write_all(entry);
    }
}

fn redact_event_field(value: &str) -> String {
    let lowercase = value.to_ascii_lowercase();
    if [
        "password",
        "passwd",
        "authorization",
        "api_key",
        "apikey",
        "secret",
        "token=",
    ]
    .iter()
    .any(|needle| lowercase.contains(needle))
    {
        "[redacted]".to_string()
    } else {
        truncate_field(value)
    }
}

fn truncate_field(value: &str) -> String {
    truncate_text(value, MAX_EVENT_FIELD_BYTES)
}

fn truncate_text(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut boundary = limit;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_string()
}

fn redact_diagnostic_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for line in input.lines() {
        let lowercase = line.to_ascii_lowercase();
        let sensitive = [
            "authorization:",
            "password=",
            "password:",
            "passwd=",
            "api_key=",
            "apikey=",
            "secret=",
            "access_token=",
            "refresh_token=",
            "bearer ",
        ]
        .iter()
        .any(|needle| lowercase.contains(needle));
        if sensitive {
            output.push_str("[redacted sensitive line]");
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    output
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_event(sequence: u64, value: &str) -> FlightEvent {
        FlightEvent {
            sequence,
            monotonic_us: sequence,
            unix_time_ms: sequence,
            thread_id: "test".into(),
            thread_name: "test".into(),
            category: value.into(),
            action: value.into(),
            operation_id: None,
            parent_event_id: None,
            object_id: value.into(),
            reason: value.into(),
            old_state: value.into(),
            new_state: value.into(),
            duration_us: None,
        }
    }

    #[test]
    fn recorder_evicts_oldest_event_at_its_hard_bound() {
        let mut recorder = BoundedFlightRecorder::new(3);
        for sequence in 1..=5 {
            recorder.push(test_event(sequence, "event"));
        }
        let sequences = recorder
            .snapshot()
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>();
        assert_eq!(sequences, [3, 4, 5]);
    }

    #[test]
    fn event_fields_are_utf8_safe_and_bounded() {
        let value = "x".repeat(MAX_EVENT_FIELD_BYTES - 1) + "⠇";
        let truncated = truncate_field(&value);
        assert!(truncated.len() <= MAX_EVENT_FIELD_BYTES);
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    #[test]
    fn event_fields_redact_likely_credentials() {
        assert_eq!(
            redact_event_field("Authorization: bearer abc"),
            "[redacted]"
        );
        assert_eq!(redact_event_field("window-feed"), "window-feed");
    }

    #[test]
    fn panic_diagnostic_text_is_redacted_and_hard_bounded() {
        let input = format!("panic\nAuthorization: Bearer abc\n{}", "⠇".repeat(64));
        let redacted = redact_diagnostic_text(&input);
        assert!(!redacted.contains("abc"));
        assert!(redacted.contains("[redacted sensitive line]"));

        let truncated = truncate_text(&redacted, 47);
        assert!(truncated.len() <= 47);
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    #[test]
    fn semantic_snapshot_has_a_fixed_serialized_bound() {
        initialize(Component::Test);
        for index in 0..(FLIGHT_RECORDER_CAPACITY * 2) {
            record(Event::new("test", "bounded").object(&index.to_string()));
        }
        let snapshot = runtime_snapshot_json().unwrap();
        assert!(snapshot.len() <= MAX_SEMANTIC_SNAPSHOT_BYTES);
    }

    #[test]
    fn observable_budget_constants_fit_the_contract() {
        let budgets = std::hint::black_box([
            OBSERVABLE_CPU_BUDGET_PPM,
            EVENT_P99_LATENCY_BUDGET_NS,
            OBSERVABLE_RSS_BUDGET_BYTES,
            PRODUCTION_BINARY_SIZE_BUDGET_BYTES,
            DEBUG_ARTIFACT_SIZE_BUDGET_BYTES,
            FLIGHT_RECORDER_CAPACITY as u64,
            MAX_NAMED_WORKERS as u64,
        ]);
        assert!(budgets[0] <= 2_500);
        assert!(budgets[1] <= 50_000);
        assert!(budgets[2] <= 1024 * 1024);
        assert!(budgets[3] <= 64 * 1024 * 1024);
        assert!(budgets[4] <= 512 * 1024 * 1024);
        assert!(budgets[5] <= 512);
        assert!(budgets[6] <= 64);
    }

    #[test]
    fn diagnostic_socket_directory_is_stable_and_bounded_for_long_runtime_paths() {
        let runtime = PathBuf::from(format!("/tmp/{}", "long-runtime-path/".repeat(10)));
        let first = diagnostic_runtime_dir_for(runtime.clone());
        let second = diagnostic_runtime_dir_for(runtime);
        assert_eq!(first, second);
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            assert!(first.as_os_str().as_bytes().len() <= 78);
        }
    }
}
