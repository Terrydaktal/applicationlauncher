use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::observability::{
    self, Component, DIAGNOSTIC_CAPTURE_BUDGET_SECS, DIAGNOSTIC_OUTPUT_BUDGET_BYTES,
};

const COMMAND_OUTPUT_LIMIT_BYTES: usize = 4 * 1024 * 1024;
const STACK_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
const DEFAULT_STACK_SAMPLES: usize = 3;
const MAX_ENDPOINT_CANDIDATES: usize = 32;
const MAX_DIAGNOSTIC_TARGETS: usize = 4;
const MAX_CHECKSUM_FILES: usize = 256;
const MAX_CHECKSUM_DEPTH: usize = 8;

#[derive(Clone, Copy, Debug)]
pub struct CaptureOptions {
    pub include_perf: bool,
    pub include_core: bool,
    pub stack_samples: usize,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            include_perf: false,
            include_core: false,
            stack_samples: DEFAULT_STACK_SAMPLES,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct TargetIdentity {
    component: String,
    pid: u32,
    application_build_id: String,
    executable: String,
    executable_device_inode: String,
    elf_build_id: Option<String>,
}

#[derive(Clone, Debug)]
struct DiagnosticTarget {
    component: Component,
    pid: u32,
    socket: PathBuf,
    application_build_id: String,
}

#[derive(Debug, Serialize)]
struct CaptureIndex {
    schema_version: u32,
    captured_unix_time_ms: u64,
    collector_pid: u32,
    collector_build_id: &'static str,
    stack_samples: usize,
    include_perf: bool,
    include_core: bool,
    output_budget_bytes: u64,
    targets: Vec<TargetIdentity>,
    errors: Vec<String>,
}

struct OutputBudget {
    used: u64,
    limit: u64,
}

impl OutputBudget {
    fn new(limit: u64) -> Self {
        Self { used: 0, limit }
    }

    fn write(&mut self, path: &Path, contents: &[u8]) -> Result<(), String> {
        let bounded = &contents[..contents.len().min(COMMAND_OUTPUT_LIMIT_BYTES)];
        if self.used.saturating_add(bounded.len() as u64) > self.limit {
            return Err(format!(
                "capture output budget exhausted before writing {}",
                path.display()
            ));
        }
        observability::write_private(path, bounded)?;
        self.used += bounded.len() as u64;
        Ok(())
    }

    fn write_text(&mut self, path: &Path, contents: &str) -> Result<(), String> {
        self.write(path, redact_text(contents).as_bytes())
    }
}

struct AttachLease<'a> {
    target: &'a DiagnosticTarget,
    token: Option<u64>,
}

impl Drop for AttachLease<'_> {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            let _ = socket_request(&self.target.socket, &format!("done {token}\n"));
        }
    }
}

pub fn capture_auto(options: CaptureOptions) -> Result<PathBuf, String> {
    let started = Instant::now();
    let targets = discover_targets()?;
    if targets.is_empty() {
        return Err(
            "no live Application Launcher GUI or daemon diagnostic endpoint was found".into(),
        );
    }

    let capture_root = observability::state_dir().join("diagnostics");
    observability::ensure_private_directory(&capture_root)?;
    let capture_dir = capture_root.join(format!(
        "capture-{}-{}-{}",
        now_ms(),
        std::process::id(),
        observability::next_operation_id()
    ));
    observability::ensure_private_directory(&capture_dir)?;

    let mut budget = OutputBudget::new(DIAGNOSTIC_OUTPUT_BUDGET_BYTES);
    let mut identities = Vec::new();
    let mut errors = Vec::new();
    for target in &targets {
        if started.elapsed() >= Duration::from_secs(DIAGNOSTIC_CAPTURE_BUDGET_SECS)
            && !options.include_core
        {
            errors.push(format!(
                "{} capture skipped after the global capture deadline",
                target.component.as_str()
            ));
            continue;
        }

        let target_dir = capture_dir.join(format!("{}-{}", target.component.as_str(), target.pid));
        if let Err(err) = observability::ensure_private_directory(&target_dir) {
            errors.push(err);
            continue;
        }
        match capture_target(target, &target_dir, options, &mut budget, &mut errors) {
            Ok(identity) => identities.push(identity),
            Err(err) => errors.push(format!(
                "{} pid {}: {err}",
                target.component.as_str(),
                target.pid
            )),
        }
    }

    let errors = errors
        .into_iter()
        .map(|error| redact_text(&error).trim().to_string())
        .collect();
    let index = CaptureIndex {
        schema_version: 1,
        captured_unix_time_ms: now_ms(),
        collector_pid: std::process::id(),
        collector_build_id: crate::BUILD_ID,
        stack_samples: options.stack_samples,
        include_perf: options.include_perf,
        include_core: options.include_core,
        output_budget_bytes: DIAGNOSTIC_OUTPUT_BUDGET_BYTES,
        targets: identities,
        errors,
    };
    let index_json = serde_json::to_string_pretty(&index).map_err(|err| err.to_string())?;
    budget.write(&capture_dir.join("capture.json"), index_json.as_bytes())?;
    write_checksum_manifest(&capture_dir)?;
    Ok(capture_dir)
}

fn capture_target(
    target: &DiagnosticTarget,
    directory: &Path,
    options: CaptureOptions,
    budget: &mut OutputBudget,
    errors: &mut Vec<String>,
) -> Result<TargetIdentity, String> {
    let semantic = socket_request(&target.socket, "snapshot\n")?;
    budget.write(
        &directory.join("semantic-snapshot.json"),
        semantic.as_bytes(),
    )?;

    let authorization = socket_request(
        &target.socket,
        &format!("authorize {}\n", std::process::id()),
    )?;
    let token = authorization
        .strip_prefix("authorized ")
        .and_then(|token| token.trim().parse::<u64>().ok())
        .ok_or_else(|| format!("target rejected diagnostic attachment: {authorization}"))?;
    let _lease = AttachLease {
        target,
        token: Some(token),
    };

    capture_proc_state(target.pid, directory, budget, errors);
    capture_process_summary(target.pid, directory, budget, errors);
    capture_repeated_stacks(target.pid, directory, options.stack_samples, budget, errors);
    capture_journal(target, directory, budget, errors);
    if options.include_perf {
        capture_perf(target.pid, directory, budget, errors);
    }
    if options.include_core {
        capture_core(target.pid, directory, budget, errors);
    }

    let executable = std::fs::read_link(format!("/proc/{}/exe", target.pid))
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("unavailable: {err}"));
    let executable_device_inode = std::fs::metadata(format!("/proc/{}/exe", target.pid))
        .map(|metadata| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                format!("{}:{}", metadata.dev(), metadata.ino())
            }
            #[cfg(not(unix))]
            {
                "unsupported".to_string()
            }
        })
        .unwrap_or_else(|err| format!("unavailable: {err}"));
    let elf_build_id = elf_build_id(Path::new(&format!("/proc/{}/exe", target.pid))).ok();
    Ok(TargetIdentity {
        component: target.component.as_str().to_string(),
        pid: target.pid,
        application_build_id: target.application_build_id.clone(),
        executable,
        executable_device_inode,
        elf_build_id,
    })
}

fn capture_proc_state(
    pid: u32,
    directory: &Path,
    budget: &mut OutputBudget,
    errors: &mut Vec<String>,
) {
    let proc_dir = directory.join("proc");
    if let Err(err) = observability::ensure_private_directory(&proc_dir) {
        errors.push(err);
        return;
    }
    for name in [
        "status",
        "stat",
        "sched",
        "io",
        "limits",
        "cgroup",
        "maps",
        "smaps_rollup",
        "mountinfo",
    ] {
        let source = format!("/proc/{pid}/{name}");
        match std::fs::read_to_string(&source) {
            Ok(contents) => {
                if let Err(err) =
                    budget.write_text(&proc_dir.join(format!("{name}.txt")), &contents)
                {
                    errors.push(err);
                }
            }
            Err(err) => errors.push(format!("could not read {source}: {err}")),
        }
    }

    let mut tasks = String::new();
    if let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/task")) {
        let mut tids = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .collect::<Vec<_>>();
        tids.sort_unstable();
        for tid in tids {
            tasks.push_str(&format!("\n==== tid {tid} ====\n"));
            for name in ["comm", "stat", "status", "wchan"] {
                let path = format!("/proc/{pid}/task/{tid}/{name}");
                if let Ok(value) = std::fs::read_to_string(path) {
                    tasks.push_str(&format!("-- {name} --\n{value}"));
                }
                if tasks.len() >= COMMAND_OUTPUT_LIMIT_BYTES {
                    break;
                }
            }
            if tasks.len() >= COMMAND_OUTPUT_LIMIT_BYTES {
                break;
            }
        }
    }
    if let Err(err) = budget.write_text(&proc_dir.join("threads.txt"), &tasks) {
        errors.push(err);
    }
}

fn capture_process_summary(
    pid: u32,
    directory: &Path,
    budget: &mut OutputBudget,
    errors: &mut Vec<String>,
) {
    let mut ps = Command::new("ps");
    ps.args([
        "-L",
        "-p",
        &pid.to_string(),
        "-o",
        "pid=,tid=,ppid=,psr=,stat=,pcpu=,pmem=,rss=,vsz=,etime=,time=,wchan:32=,comm=",
    ]);
    capture_command_text("ps", ps, Duration::from_secs(5), directory, budget, errors);

    let mut readelf = Command::new("readelf");
    readelf.args(["-n", &format!("/proc/{pid}/exe")]);
    capture_command_text(
        "elf-notes",
        readelf,
        Duration::from_secs(5),
        directory,
        budget,
        errors,
    );
}

fn capture_repeated_stacks(
    pid: u32,
    directory: &Path,
    requested_samples: usize,
    budget: &mut OutputBudget,
    errors: &mut Vec<String>,
) {
    let samples = requested_samples.clamp(1, 10);
    for sample in 1..=samples {
        let started = Instant::now();
        let mut eu_stack = Command::new("eu-stack");
        eu_stack.args(["-p", &pid.to_string(), "-n", "64", "-s"]);
        let output = crate::process::output_with_timeout(eu_stack, Duration::from_secs(8));
        let (tool, output) = match output {
            Ok(output) if output.status.success() || !output.stdout.is_empty() => {
                ("eu-stack", Ok(output))
            }
            _ => {
                let mut gdb = Command::new("gdb");
                gdb.env("DEBUGINFOD_URLS", "").args([
                    "-q",
                    "-batch",
                    "-iex",
                    "set pagination off",
                    "-iex",
                    "set debuginfod enabled off",
                    "-iex",
                    "set print frame-arguments none",
                    "-ex",
                    "set print thread-events off",
                    "-ex",
                    "info threads",
                    "-ex",
                    "thread apply all bt 64",
                    &format!("/proc/{pid}/exe"),
                    "-p",
                    &pid.to_string(),
                ]);
                (
                    "gdb",
                    crate::process::output_with_timeout(gdb, Duration::from_secs(12)),
                )
            }
        };

        let path = directory.join(format!("stack-{sample:02}.txt"));
        match output {
            Ok(output) => {
                let mut contents = format!(
                    "tool: {tool}\nelapsed_ms: {}\nstatus: {}\n\n",
                    started.elapsed().as_millis(),
                    output.status
                );
                contents.push_str(&String::from_utf8_lossy(&output.stdout));
                contents.push_str(&String::from_utf8_lossy(&output.stderr));
                if let Err(err) = budget.write_text(&path, &contents) {
                    errors.push(err);
                }
            }
            Err(err) => errors.push(format!("stack sample {sample} failed: {err}")),
        }
        if sample < samples {
            std::thread::sleep(STACK_SAMPLE_INTERVAL);
        }
    }
}

fn capture_journal(
    target: &DiagnosticTarget,
    directory: &Path,
    budget: &mut OutputBudget,
    errors: &mut Vec<String>,
) {
    let mut journal = Command::new("journalctl");
    journal.args(["--user", "--no-pager", "--since", "-10min", "-n", "1000"]);
    if target.component == Component::Daemon {
        journal.args(["-u", "applicationlauncherd.service"]);
    } else {
        journal.arg(format!("_PID={}", target.pid));
    }
    capture_command_text(
        "journal",
        journal,
        Duration::from_secs(8),
        directory,
        budget,
        errors,
    );
}

fn capture_perf(pid: u32, directory: &Path, budget: &mut OutputBudget, errors: &mut Vec<String>) {
    let output = directory.join("perf.data");
    let mut perf = Command::new("perf");
    perf.args([
        "record",
        "--quiet",
        "--call-graph",
        "dwarf",
        "--pid",
        &pid.to_string(),
        "--output",
    ])
    .arg(&output)
    .args(["--", "sleep", "3"]);
    capture_command_text(
        "perf",
        perf,
        Duration::from_secs(8),
        directory,
        budget,
        errors,
    );
    if let Ok(metadata) = output.metadata() {
        budget.used = budget.used.saturating_add(metadata.len());
        if budget.used > budget.limit {
            errors.push("perf.data caused the diagnostic output budget to be exceeded".into());
        }
    }
}

fn capture_core(pid: u32, directory: &Path, budget: &mut OutputBudget, errors: &mut Vec<String>) {
    let prefix = directory.join("core");
    let mut gcore = Command::new("gcore");
    gcore.arg("-o").arg(&prefix).arg(pid.to_string());
    capture_command_text(
        "gcore",
        gcore,
        Duration::from_secs(30),
        directory,
        budget,
        errors,
    );
    // Explicit full cores may exceed the normal bounded text/perf allowance and
    // can contain secrets. They are never captured by --diagnose auto alone.
}

fn capture_command_text(
    name: &str,
    command: Command,
    timeout: Duration,
    directory: &Path,
    budget: &mut OutputBudget,
    errors: &mut Vec<String>,
) {
    match crate::process::output_with_timeout(command, timeout) {
        Ok(output) => {
            let mut contents = format!("status: {}\n\n", output.status);
            contents.push_str(&String::from_utf8_lossy(&output.stdout));
            contents.push_str(&String::from_utf8_lossy(&output.stderr));
            if let Err(err) = budget.write_text(&directory.join(format!("{name}.txt")), &contents) {
                errors.push(err);
            }
        }
        Err(err) => errors.push(format!("{name} failed: {err}")),
    }
}

fn discover_targets() -> Result<Vec<DiagnosticTarget>, String> {
    discover_targets_in(&observability::diagnostic_runtime_dir())
}

fn discover_targets_in(directory: &Path) -> Result<Vec<DiagnosticTarget>, String> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(format!(
                "could not inspect diagnostic endpoints in {}: {err}",
                directory.display()
            ));
        }
    };

    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());

    let mut targets = Vec::new();
    for entry in entries.into_iter().take(MAX_ENDPOINT_CANDIDATES) {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(stem) = file_name.strip_suffix(".sock") else {
            continue;
        };
        let Some((component, pid)) = stem.rsplit_once('-') else {
            continue;
        };
        let component = match component {
            "gui" => Component::Gui,
            "daemon" => Component::Daemon,
            _ => continue,
        };
        let Some(pid) = pid.parse::<u32>().ok().filter(|pid| *pid > 0) else {
            continue;
        };
        if !Path::new(&format!("/proc/{pid}")).exists() {
            let _ = std::fs::remove_file(path);
            continue;
        }
        let Ok(ping) = socket_request_with_timeout(&path, "ping\n", Duration::from_millis(250))
        else {
            continue;
        };
        let fields = ping.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 4
            || fields[0] != "ok"
            || fields[1] != component.as_str()
            || fields[2].parse::<u32>().ok() != Some(pid)
        {
            continue;
        }
        targets.push(DiagnosticTarget {
            component,
            pid,
            socket: path,
            application_build_id: fields[3].to_string(),
        });
        if targets.len() == MAX_DIAGNOSTIC_TARGETS {
            break;
        }
    }
    targets.sort_by_key(|target| (target.component.as_str(), target.pid));
    targets.dedup_by_key(|target| (target.component.as_str(), target.pid));
    Ok(targets)
}

fn socket_request(path: &Path, request: &str) -> Result<String, String> {
    socket_request_with_timeout(path, request, Duration::from_secs(5))
}

fn socket_request_with_timeout(
    path: &Path,
    request: &str,
    timeout: Duration,
) -> Result<String, String> {
    let mut stream = std::os::unix::net::UnixStream::connect(path)
        .map_err(|err| format!("could not connect to {}: {err}", path.display()))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|err| err.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|err| err.to_string())?;
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("could not send diagnostic request: {err}"))?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut response = Vec::new();
    stream
        .take((COMMAND_OUTPUT_LIMIT_BYTES + 1) as u64)
        .read_to_end(&mut response)
        .map_err(|err| format!("could not read diagnostic response: {err}"))?;
    if response.len() > COMMAND_OUTPUT_LIMIT_BYTES {
        return Err("diagnostic endpoint response exceeded its hard bound".into());
    }
    Ok(String::from_utf8_lossy(&response).trim().to_string())
}

pub fn elf_build_id(path: &Path) -> Result<String, String> {
    let mut command = Command::new("readelf");
    command.arg("-n").arg(path);
    let output = crate::process::output_with_timeout(command, Duration::from_secs(5))
        .map_err(|err| format!("could not run readelf: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "readelf failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.trim().strip_prefix("Build ID:"))
        .map(str::trim)
        .map(str::to_string)
        .filter(|build_id| !build_id.is_empty())
        .ok_or_else(|| format!("{} has no ELF build ID", path.display()))
}

fn write_checksum_manifest(directory: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    collect_regular_files(directory, directory, &mut files, 0)?;
    files.sort();
    let mut manifest = String::new();
    for relative in files {
        if relative == Path::new("manifest.sha256") {
            continue;
        }
        let digest = sha256_file(&directory.join(&relative))?;
        manifest.push_str(&format!("{digest}  {}\n", relative.display()));
    }
    observability::write_private(&directory.join("manifest.sha256"), manifest.as_bytes())
}

fn collect_regular_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_CHECKSUM_DEPTH {
        return Err(format!(
            "checksum traversal exceeded its maximum depth below {}",
            root.display()
        ));
    }
    for entry in std::fs::read_dir(directory)
        .map_err(|err| format!("could not enumerate {}: {err}", directory.display()))?
    {
        let entry = entry.map_err(|err| err.to_string())?;
        let file_type = entry.file_type().map_err(|err| err.to_string())?;
        if file_type.is_dir() {
            collect_regular_files(root, &entry.path(), files, depth + 1)?;
        } else if file_type.is_file() {
            if files.len() == MAX_CHECKSUM_FILES {
                return Err(format!(
                    "checksum traversal exceeded its {}-file bound",
                    MAX_CHECKSUM_FILES
                ));
            }
            files.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|err| err.to_string())?
                    .to_path_buf(),
            );
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut input = std::fs::File::open(path)
        .map_err(|err| format!("could not open {} for hashing: {err}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|err| format!("could not hash {}: {err}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub fn redact_text(input: &str) -> String {
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

#[derive(Debug, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub required: bool,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub schema_version: u32,
    pub captured_unix_time_ms: u64,
    pub build_id: &'static str,
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn passed(&self) -> bool {
        self.checks
            .iter()
            .all(|check| !check.required || check.passed)
    }
}

pub fn run_debug_doctor() -> Result<(PathBuf, DoctorReport), String> {
    observability::initialize(Component::Test);
    observability::enable_observable_for_doctor();
    let mut checks = Vec::new();
    let executable = std::env::current_exe().map_err(|err| err.to_string())?;

    let build_id = elf_build_id(&executable);
    checks.push(DoctorCheck {
        name: "elf-build-id".into(),
        required: true,
        passed: build_id.is_ok(),
        detail: build_id.clone().unwrap_or_else(|err| err),
    });

    let debug_sections = command_output(Command::new("readelf").args(["-S"]).arg(&executable));
    let has_debug = debug_sections
        .as_ref()
        .is_ok_and(|output| output.contains(".debug_info") || output.contains(".gnu_debuglink"));
    checks.push(DoctorCheck {
        name: "symbolization-data".into(),
        required: true,
        passed: has_debug,
        detail: if has_debug {
            "ELF contains debug information or a GNU debug link".into()
        } else {
            debug_sections.unwrap_or_else(|err| err)
        },
    });

    for (tool, required) in [
        ("readelf", true),
        ("eu-stack", false),
        ("gdb", true),
        ("journalctl", true),
        ("perf", false),
        ("gcore", false),
    ] {
        let available = crate::process::executable_path(tool).is_some();
        checks.push(DoctorCheck {
            name: format!("tool-{tool}"),
            required,
            passed: available,
            detail: if available {
                "available".into()
            } else {
                "not found in PATH".into()
            },
        });
    }

    let rss_before_events = process_rss_bytes();
    const LATENCY_SAMPLE_COUNT: u64 = 2_000;
    let mut event_latencies = Vec::with_capacity(LATENCY_SAMPLE_COUNT as usize);
    for index in 0..LATENCY_SAMPLE_COUNT {
        let event_started = Instant::now();
        observability::record(
            observability::Event::new("doctor", "latency-sample").operation(index),
        );
        event_latencies.push(event_started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
    }
    event_latencies.sort_unstable();
    let p99_index = ((event_latencies.len() * 99) / 100).min(event_latencies.len() - 1);
    let p99_ns = event_latencies[p99_index];
    checks.push(DoctorCheck {
        name: "flight-recorder-latency".into(),
        required: true,
        passed: p99_ns <= observability::EVENT_P99_LATENCY_BUDGET_NS,
        detail: format!(
            "measured p99 {p99_ns} ns/event; contract p99 budget {} ns",
            observability::EVENT_P99_LATENCY_BUDGET_NS
        ),
    });
    let rss_after_events = process_rss_bytes();
    let rss_growth = rss_before_events
        .zip(rss_after_events)
        .map(|(before, after)| after.saturating_sub(before));
    checks.push(DoctorCheck {
        name: "flight-recorder-rss".into(),
        required: true,
        passed: rss_growth
            .is_some_and(|growth| growth <= observability::OBSERVABLE_RSS_BUDGET_BYTES),
        detail: rss_growth.map_or_else(
            || "VmRSS was unavailable".into(),
            |growth| {
                format!(
                    "measured retained growth {growth} bytes; budget {} bytes",
                    observability::OBSERVABLE_RSS_BUDGET_BYTES
                )
            },
        ),
    });

    let server = observability::start_diagnostic_server(Component::Test)?;
    let cpu_before = process_cpu_time_us();
    let idle_started = Instant::now();
    std::thread::sleep(Duration::from_millis(250));
    let idle_elapsed_us = idle_started.elapsed().as_micros().max(1) as u64;
    let idle_cpu_ppm = cpu_before
        .zip(process_cpu_time_us())
        .map(|(before, after)| {
            after.saturating_sub(before).saturating_mul(1_000_000) / idle_elapsed_us
        });
    checks.push(DoctorCheck {
        name: "idle-observability-cpu".into(),
        required: true,
        passed: idle_cpu_ppm.is_some_and(|ppm| ppm <= observability::OBSERVABLE_CPU_BUDGET_PPM),
        detail: idle_cpu_ppm.map_or_else(
            || "getrusage was unavailable".into(),
            |ppm| {
                format!(
                    "measured {ppm} ppm while idle; budget {} ppm",
                    observability::OBSERVABLE_CPU_BUDGET_PPM
                )
            },
        ),
    });
    let socket = observability::diagnostic_socket_path(Component::Test, std::process::id());
    let ping = socket_request(&socket, "ping\n");
    let snapshot = socket_request(&socket, "snapshot\n");
    let authorization = socket_request(&socket, &format!("authorize {}\n", std::process::id()));
    let authorization_ok = authorization
        .as_ref()
        .ok()
        .and_then(|response| response.strip_prefix("authorized "))
        .and_then(|token| token.trim().parse::<u64>().ok())
        .is_some_and(|token| socket_request(&socket, &format!("done {token}\n")).is_ok());
    checks.push(DoctorCheck {
        name: "independent-endpoint".into(),
        required: true,
        passed: ping.is_ok() && snapshot.is_ok(),
        detail: ping.unwrap_or_else(|err| err),
    });
    checks.push(DoctorCheck {
        name: "ptrace-authorization-handshake".into(),
        required: true,
        passed: authorization_ok,
        detail: authorization.unwrap_or_else(|err| err),
    });
    drop(server);

    let artifact_validation = build_id.as_ref().map_or_else(
        |err| Err(err.clone()),
        |id| {
            validate_archived_build(
                &build_artifact_root()
                    .join("by-build-id")
                    .join(id)
                    .join("build-info.json"),
                &executable,
                crate::BUILD_ID,
                id,
            )
        },
    );
    checks.push(DoctorCheck {
        name: "archived-exact-build".into(),
        required: false,
        passed: artifact_validation.is_ok(),
        detail: artifact_validation.unwrap_or_else(|err| {
            format!("{err}; run scripts/build-debuggable-release for a release binary")
        }),
    });

    let core_pattern = std::fs::read_to_string("/proc/sys/kernel/core_pattern");
    checks.push(DoctorCheck {
        name: "native-core-routing".into(),
        required: false,
        passed: core_pattern.is_ok(),
        detail: core_pattern
            .map(|pattern| pattern.trim().to_string())
            .unwrap_or_else(|err| err.to_string()),
    });

    let state_dir = observability::state_dir();
    observability::ensure_private_directory(&state_dir)?;
    let probe = state_dir.join("debug-doctor-write-probe");
    let private_write = observability::write_private(&probe, b"probe").and_then(|()| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&probe)
                .map_err(|err| err.to_string())?
                .permissions()
                .mode()
                & 0o777;
            (mode == 0o600)
                .then_some(())
                .ok_or_else(|| format!("write probe mode was {mode:o}, expected 600"))
        }
        #[cfg(not(unix))]
        Ok(())
    });
    let _ = std::fs::remove_file(&probe);
    checks.push(DoctorCheck {
        name: "private-output".into(),
        required: true,
        passed: private_write.is_ok(),
        detail: private_write
            .map(|()| "mode 0600".into())
            .unwrap_or_else(|err| err),
    });

    let report = DoctorReport {
        schema_version: 1,
        captured_unix_time_ms: now_ms(),
        build_id: crate::BUILD_ID,
        checks,
    };
    let path = state_dir.join(format!("debug-doctor-{}.json", now_ms()));
    observability::write_private(
        &path,
        serde_json::to_string_pretty(&report)
            .map_err(|err| err.to_string())?
            .as_bytes(),
    )?;
    Ok((path, report))
}

fn command_output(command: &mut Command) -> Result<String, String> {
    let command = std::mem::replace(command, Command::new("true"));
    let output = crate::process::output_with_timeout(command, Duration::from_secs(5))
        .map_err(|err| err.to_string())?;
    let mut result = String::from_utf8_lossy(&output.stdout).into_owned();
    result.push_str(&String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(result)
    } else {
        Err(result)
    }
}

pub fn build_artifact_root() -> PathBuf {
    std::env::var_os("APPLICATIONLAUNCHER_BUILD_ARCHIVE")
        .map(PathBuf::from)
        .unwrap_or_else(|| observability::state_dir().join("builds"))
}

pub fn validate_archived_build(
    build_info_path: &Path,
    executable: &Path,
    expected_application_build_id: &str,
    expected_elf_build_id: &str,
) -> Result<String, String> {
    let build_info = std::fs::read_to_string(build_info_path).map_err(|err| {
        format!(
            "could not read archived build metadata {}: {err}",
            build_info_path.display()
        )
    })?;
    let value: serde_json::Value = serde_json::from_str(&build_info)
        .map_err(|err| format!("invalid build-info.json: {err}"))?;
    let application_build_id = value
        .get("application_build_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "build-info.json has no application_build_id".to_string())?;
    let elf_build_id = value
        .get("elf_build_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "build-info.json has no elf_build_id".to_string())?;
    if application_build_id != expected_application_build_id {
        return Err(format!(
            "archived application build ID is stale: {application_build_id} != {expected_application_build_id}"
        ));
    }
    if elf_build_id != expected_elf_build_id {
        return Err(format!(
            "archived ELF build ID is stale: {elf_build_id} != {expected_elf_build_id}"
        ));
    }

    let executable_hash = sha256_file(executable)?;
    let artifacts = value
        .get("artifacts")
        .ok_or_else(|| "build-info.json has no artifacts object".to_string())?;
    let exact_hash = artifacts
        .get("exact_unstripped")
        .and_then(|artifact| artifact.get("sha256"))
        .and_then(serde_json::Value::as_str);
    let stripped_hash = artifacts
        .get("stripped")
        .and_then(|artifact| artifact.get("sha256"))
        .and_then(serde_json::Value::as_str);
    if exact_hash != Some(executable_hash.as_str())
        && stripped_hash != Some(executable_hash.as_str())
    {
        return Err("running executable hash does not match an archived exact binary".into());
    }
    Ok(format!(
        "ELF build ID {elf_build_id} and executable SHA-256 match the archive"
    ))
}

pub fn run_internal_probe(kind: &str) -> Result<(), String> {
    match kind {
        "panic" => panic!("intentional diagnostic panic probe"),
        "native-crash" => {
            #[cfg(unix)]
            unsafe {
                let limits = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                libc::setrlimit(libc::RLIMIT_CORE, &limits);
                libc::signal(libc::SIGSEGV, libc::SIG_DFL);
                let mut signals = std::mem::zeroed();
                libc::sigemptyset(&mut signals);
                libc::sigaddset(&mut signals, libc::SIGSEGV);
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &signals, std::ptr::null_mut());
                libc::kill(libc::getpid(), libc::SIGSEGV);
                libc::_exit(128 + libc::SIGSEGV);
            }
            #[cfg(not(unix))]
            return Err("native crash probe is only supported on Unix".into());
        }
        "hang" => {
            observability::initialize(Component::Gui);
            let _server = observability::start_diagnostic_server(Component::Gui)?;
            println!(
                "ready {}",
                observability::diagnostic_socket_path(Component::Gui, std::process::id()).display()
            );
            std::io::stdout().flush().map_err(|err| err.to_string())?;
            std::thread::sleep(Duration::from_secs(60));
        }
        _ => return Err(format!("unknown internal diagnostic probe {kind}")),
    }
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn process_rss_bytes() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            if fields.next()? != "VmRSS:" {
                return None;
            }
            Some(fields.next()?.parse::<u64>().ok()?.saturating_mul(1024))
        })
}

#[cfg(unix)]
fn process_cpu_time_us() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    let usage = unsafe { usage.assume_init() };
    let user = u64::try_from(usage.ru_utime.tv_sec)
        .ok()?
        .saturating_mul(1_000_000)
        + u64::try_from(usage.ru_utime.tv_usec).ok()?;
    let system = u64::try_from(usage.ru_stime.tv_sec)
        .ok()?
        .saturating_mul(1_000_000)
        + u64::try_from(usage.ru_stime.tv_usec).ok()?;
    Some(user.saturating_add(system))
}

#[cfg(not(unix))]
fn process_cpu_time_us() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_removes_likely_credentials_but_preserves_diagnostics() {
        let input = "Name:\tapplicationlauncher\nAuthorization: Bearer abc\npassword=hunter2\n";
        let redacted = redact_text(input);
        assert!(redacted.contains("Name:\tapplicationlauncher"));
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("hunter2"));
        assert_eq!(redacted.matches("[redacted sensitive line]").count(), 2);
    }

    #[test]
    fn target_discovery_treats_a_missing_runtime_directory_as_empty() {
        let path = PathBuf::from(format!(
            "/tmp/applicationlauncher-missing-diagnostics-{}-{}",
            std::process::id(),
            now_ms()
        ));
        assert!(discover_targets_in(&path).unwrap().is_empty());
    }

    #[test]
    fn output_budget_rejects_unbounded_capture_growth() {
        let root = PathBuf::from(format!(
            "/tmp/applicationlauncher-output-budget-{}-{}",
            std::process::id(),
            now_ms()
        ));
        observability::ensure_private_directory(&root).unwrap();
        let mut budget = OutputBudget::new(4);
        assert!(budget.write(&root.join("one"), b"1234").is_ok());
        assert!(budget.write(&root.join("two"), b"5").is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn checksum_manifest_is_deterministic_and_excludes_itself() {
        let root = PathBuf::from(format!(
            "/tmp/applicationlauncher-checksums-{}-{}",
            std::process::id(),
            now_ms()
        ));
        observability::ensure_private_directory(&root).unwrap();
        observability::write_private(&root.join("evidence.txt"), b"evidence").unwrap();
        write_checksum_manifest(&root).unwrap();
        let first = std::fs::read_to_string(root.join("manifest.sha256")).unwrap();
        write_checksum_manifest(&root).unwrap();
        let second = std::fs::read_to_string(root.join("manifest.sha256")).unwrap();
        assert_eq!(first, second);
        assert!(first.ends_with("  evidence.txt\n"));
        assert!(!first.contains("manifest.sha256"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn checksum_manifest_rejects_too_many_files() {
        let root = PathBuf::from(format!(
            "/tmp/applicationlauncher-checksum-files-{}-{}",
            std::process::id(),
            now_ms()
        ));
        observability::ensure_private_directory(&root).unwrap();
        for index in 0..=MAX_CHECKSUM_FILES {
            std::fs::write(root.join(format!("evidence-{index}")), b"evidence").unwrap();
        }
        assert!(
            write_checksum_manifest(&root)
                .unwrap_err()
                .contains("file bound")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn checksum_manifest_rejects_excessive_directory_depth() {
        let root = PathBuf::from(format!(
            "/tmp/applicationlauncher-checksum-depth-{}-{}",
            std::process::id(),
            now_ms()
        ));
        observability::ensure_private_directory(&root).unwrap();
        let mut nested = root.clone();
        for _ in 0..=MAX_CHECKSUM_DEPTH {
            nested.push("nested");
            std::fs::create_dir(&nested).unwrap();
        }
        std::fs::write(nested.join("evidence"), b"evidence").unwrap();
        assert!(
            write_checksum_manifest(&root)
                .unwrap_err()
                .contains("maximum depth")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stack_sampling_and_capture_options_are_bounded() {
        let options = CaptureOptions {
            include_perf: false,
            include_core: false,
            stack_samples: usize::MAX,
        };
        assert_eq!(options.stack_samples.clamp(1, 10), 10);
        assert!(COMMAND_OUTPUT_LIMIT_BYTES as u64 <= DIAGNOSTIC_OUTPUT_BUDGET_BYTES);
    }

    #[test]
    fn elf_build_id_parser_rejects_non_elf_input() {
        let path = PathBuf::from(format!(
            "/tmp/applicationlauncher-not-elf-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::write(&path, b"not an elf").unwrap();
        assert!(elf_build_id(&path).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn archived_build_validation_rejects_stale_application_identity() {
        let root = PathBuf::from(format!(
            "/tmp/applicationlauncher-stale-build-{}-{}",
            std::process::id(),
            now_ms()
        ));
        observability::ensure_private_directory(&root).unwrap();
        let executable = root.join("binary");
        std::fs::write(&executable, b"exact binary").unwrap();
        let hash = format!("{:x}", Sha256::digest(b"exact binary"));
        let build_info = root.join("build-info.json");
        std::fs::write(
            &build_info,
            serde_json::json!({
                "application_build_id": "old-build",
                "elf_build_id": "elf-id",
                "artifacts": {
                    "exact_unstripped": {"sha256": hash},
                    "stripped": {"sha256": "none"}
                }
            })
            .to_string(),
        )
        .unwrap();
        let error =
            validate_archived_build(&build_info, &executable, "new-build", "elf-id").unwrap_err();
        assert!(error.contains("stale"));
        let _ = std::fs::remove_dir_all(root);
    }
}
