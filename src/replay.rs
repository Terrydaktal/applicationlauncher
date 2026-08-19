use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::observability::FlightEvent;

pub const TRACE_SCHEMA_VERSION: u32 = 1;
pub const PERSISTENT_RING_SLOT_BYTES: usize = 4096;
pub const PERSISTENT_RING_SLOTS: usize = 512;
// magic (8) + sequence (8) + payload length (4) + SHA-256 (32).
pub const PERSISTENT_RING_HEADER_BYTES: usize = 52;
pub const PERSISTENT_RING_MAGIC: &[u8; 8] = b"ALTRC001";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BoundaryKind {
    WindowFeed,
    WindowIdentity,
    TerminalAction,
    Attention,
    TrackerMutation,
    SearchDecision,
    IconResolution,
    Timer,
    FaultInjection,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BoundaryRecord {
    pub schema_version: u32,
    pub kind: BoundaryKind,
    pub action: String,
    pub payload: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub schema_version: u32,
    pub subject: String,
    pub decision: String,
    pub evidence: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FaultPoint {
    WindowFeedDrop,
    WindowFeedDuplicate,
    TerminalSendFailure,
    TrackerWriteBusy,
    DiagnosticResponseDelay,
}

impl FaultPoint {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WindowFeedDrop => "window-feed-drop",
            Self::WindowFeedDuplicate => "window-feed-duplicate",
            Self::TerminalSendFailure => "terminal-send-failure",
            Self::TrackerWriteBusy => "tracker-write-busy",
            Self::DiagnosticResponseDelay => "diagnostic-response-delay",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|point| point.as_str() == value)
    }

    pub const ALL: [Self; 5] = [
        Self::WindowFeedDrop,
        Self::WindowFeedDuplicate,
        Self::TerminalSendFailure,
        Self::TrackerWriteBusy,
        Self::DiagnosticResponseDelay,
    ];
}

#[derive(Clone, Debug, Default)]
pub struct FaultPlan {
    points: BTreeSet<FaultPoint>,
    pub seed: u64,
}

impl FaultPlan {
    pub fn from_points(points: impl IntoIterator<Item = FaultPoint>, seed: u64) -> Self {
        Self {
            points: points.into_iter().collect(),
            seed,
        }
    }

    pub fn from_environment() -> Self {
        if std::env::var("APPLICATIONLAUNCHER_ALLOW_FAULT_INJECTION").as_deref() != Ok("1") {
            return Self::default();
        }

        let points = std::env::var("APPLICATIONLAUNCHER_FAULT_INJECTION")
            .unwrap_or_default()
            .split(',')
            .filter_map(|value| FaultPoint::parse(value.trim()))
            .collect::<Vec<_>>();
        let seed = std::env::var("APPLICATIONLAUNCHER_FAULT_SEED")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        Self::from_points(points, seed)
    }

    pub fn should_inject(&self, point: FaultPoint, ordinal: u64) -> bool {
        self.points.contains(&point) && (self.seed == 0 || ordinal % self.seed == 0)
    }

    pub fn enabled_points(&self) -> Vec<&'static str> {
        self.points.iter().map(|point| point.as_str()).collect()
    }
}

static ENVIRONMENT_FAULT_PLAN: std::sync::OnceLock<FaultPlan> = std::sync::OnceLock::new();

pub fn environment_should_inject(point: FaultPoint, ordinal: u64) -> bool {
    ENVIRONMENT_FAULT_PLAN
        .get_or_init(FaultPlan::from_environment)
        .should_inject(point, ordinal)
}

pub fn environment_faults_enabled() -> bool {
    !ENVIRONMENT_FAULT_PLAN
        .get_or_init(FaultPlan::from_environment)
        .points
        .is_empty()
}

pub fn ring_slot_offset(sequence: u64) -> u64 {
    ((sequence as usize % PERSISTENT_RING_SLOTS) * PERSISTENT_RING_SLOT_BYTES) as u64
}

pub fn encode_ring_header(
    sequence: u64,
    payload: &[u8],
) -> Option<[u8; PERSISTENT_RING_HEADER_BYTES]> {
    if payload.len() > PERSISTENT_RING_SLOT_BYTES - PERSISTENT_RING_HEADER_BYTES {
        return None;
    }
    let digest = Sha256::digest(payload);
    let mut header = [0_u8; PERSISTENT_RING_HEADER_BYTES];
    header[..PERSISTENT_RING_MAGIC.len()].copy_from_slice(PERSISTENT_RING_MAGIC);
    header[8..16].copy_from_slice(&sequence.to_le_bytes());
    header[16..20].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    header[20..].copy_from_slice(&digest);
    Some(header)
}

pub fn read_ring_file(path: &Path) -> Result<Vec<FlightEvent>, String> {
    let mut file = File::open(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let mut events = Vec::new();
    let mut header = [0_u8; PERSISTENT_RING_HEADER_BYTES];
    for slot in 0..PERSISTENT_RING_SLOTS {
        file.seek(SeekFrom::Start((slot * PERSISTENT_RING_SLOT_BYTES) as u64))
            .map_err(|err| format!("{}: {err}", path.display()))?;
        if file.read_exact(&mut header).is_err() {
            break;
        }
        if &header[..PERSISTENT_RING_MAGIC.len()] != PERSISTENT_RING_MAGIC {
            continue;
        }
        let sequence = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let length = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
        if length > PERSISTENT_RING_SLOT_BYTES - PERSISTENT_RING_HEADER_BYTES {
            continue;
        }
        let mut payload = vec![0_u8; length];
        if file.read_exact(&mut payload).is_err() {
            continue;
        }
        let digest = Sha256::digest(&payload);
        if header[20..] != digest[..] {
            continue;
        }
        let Ok(event) = serde_json::from_slice::<FlightEvent>(&payload) else {
            continue;
        };
        if event.sequence == sequence {
            events.push(event);
        }
    }
    events.sort_by_key(|event| (event.unix_time_ms, event.instance_id, event.sequence));
    events.dedup_by_key(|event| (event.instance_id, event.sequence));
    Ok(events)
}

pub fn replay_path(path: &Path) -> Result<ReplayReport, String> {
    let mut events = if path.is_file() {
        read_ring_file(path)?
    } else if path.is_dir() {
        let mut ring_paths = Vec::new();
        collect_ring_paths(path, 0, &mut ring_paths)?;
        if ring_paths.is_empty() {
            return Err(format!(
                "no flight-recorder ring found below {}",
                path.display()
            ));
        }
        let mut events = Vec::new();
        for ring_path in ring_paths {
            events.extend(read_ring_file(&ring_path)?);
        }
        events
    } else {
        return Err(format!("replay input does not exist: {}", path.display()));
    };
    events.sort_by_key(|event| (event.unix_time_ms, event.instance_id, event.sequence));
    Ok(replay_events(&events))
}

fn collect_ring_paths(
    directory: &Path,
    depth: usize,
    paths: &mut Vec<std::path::PathBuf>,
) -> Result<(), String> {
    if depth > 3 {
        return Ok(());
    }
    let entries =
        std::fs::read_dir(directory).map_err(|err| format!("{}: {err}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("{}: {err}", directory.display()))?;
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "ring")
        {
            paths.push(path);
        } else if path.is_dir() {
            collect_ring_paths(&path, depth + 1, paths)?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ReplayState {
    pub window_feed_generation: u64,
    pub window_count: usize,
    pub current_query: String,
    pub attention_attempts: u64,
    pub attention_failures: u64,
    pub tracker_mutations: u64,
    pub last_decisions: Vec<DecisionRecord>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ReplayReport {
    pub schema_version: u32,
    pub applied_events: usize,
    pub rejected_events: usize,
    pub stream_count: usize,
    pub errors: Vec<String>,
    pub state: ReplayState,
}

pub fn replay_events(events: &[FlightEvent]) -> ReplayReport {
    let mut report = ReplayReport {
        schema_version: TRACE_SCHEMA_VERSION,
        ..ReplayReport::default()
    };
    let mut previous_sequences = HashMap::new();
    for event in events {
        let previous_sequence = previous_sequences
            .get(&event.instance_id)
            .copied()
            .unwrap_or_default();
        if event.sequence <= previous_sequence {
            report.rejected_events += 1;
            report.errors.push(format!(
                "non-monotonic sequence {} after {}",
                event.sequence, previous_sequence
            ));
            continue;
        }
        previous_sequences.insert(event.instance_id, event.sequence);
        report.applied_events += 1;
        let Some(boundary) = event.boundary.as_ref() else {
            continue;
        };
        if boundary.schema_version != TRACE_SCHEMA_VERSION {
            report.rejected_events += 1;
            report.errors.push(format!(
                "unsupported boundary schema {} at sequence {}",
                boundary.schema_version, event.sequence
            ));
            continue;
        }
        if serde_json::from_str::<serde_json::Value>(&boundary.payload).is_err() {
            report.rejected_events += 1;
            report.errors.push(format!(
                "invalid boundary payload at sequence {}",
                event.sequence
            ));
            continue;
        }
        apply_boundary(&mut report.state, boundary);
    }
    report.stream_count = previous_sequences.len();
    report
}

fn apply_boundary(state: &mut ReplayState, boundary: &BoundaryRecord) {
    let payload = serde_json::from_str::<serde_json::Value>(&boundary.payload).ok();
    match boundary.kind {
        BoundaryKind::WindowFeed => {
            if let Some(generation) = payload
                .as_ref()
                .and_then(|value| value.get("generation"))
                .and_then(serde_json::Value::as_u64)
            {
                state.window_feed_generation = generation;
            }
            if let Some(count) = payload
                .as_ref()
                .and_then(|value| value.get("window_count"))
                .and_then(serde_json::Value::as_u64)
            {
                state.window_count = count as usize;
            }
        }
        BoundaryKind::SearchDecision => {
            if let Some(query) = payload
                .as_ref()
                .and_then(|value| value.get("query"))
                .and_then(serde_json::Value::as_str)
            {
                state.current_query = query.to_string();
            }
            if let Some(decision) = payload
                .as_ref()
                .and_then(|value| value.get("decision"))
                .and_then(serde_json::Value::as_str)
            {
                state.last_decisions.push(DecisionRecord {
                    schema_version: TRACE_SCHEMA_VERSION,
                    subject: boundary.action.clone(),
                    decision: decision.to_string(),
                    evidence: payload
                        .as_ref()
                        .and_then(|value| value.get("evidence"))
                        .and_then(serde_json::Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                });
                if state.last_decisions.len() > 32 {
                    state.last_decisions.remove(0);
                }
            }
        }
        BoundaryKind::Attention => {
            state.attention_attempts = state.attention_attempts.saturating_add(1);
            if payload
                .as_ref()
                .and_then(|value| value.get("succeeded"))
                .and_then(serde_json::Value::as_bool)
                == Some(false)
            {
                state.attention_failures = state.attention_failures.saturating_add(1);
            }
        }
        BoundaryKind::TrackerMutation => {
            state.tracker_mutations = state.tracker_mutations.saturating_add(1);
        }
        BoundaryKind::WindowIdentity
        | BoundaryKind::TerminalAction
        | BoundaryKind::IconResolution
        | BoundaryKind::Timer
        | BoundaryKind::FaultInjection => {}
    }
}

pub fn trace_payload(fields: &[(&str, &str)]) -> String {
    let object = fields
        .iter()
        .map(|(key, value)| (*key, *value))
        .collect::<HashMap<_, _>>();
    serde_json::to_string(&object).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::Event;

    #[test]
    fn fault_plan_is_disabled_without_explicit_points() {
        let plan = FaultPlan::from_points([], 0);
        assert!(!plan.should_inject(FaultPoint::TerminalSendFailure, 1));
    }

    #[test]
    fn replay_applies_typed_boundary_events_deterministically() {
        let mut event = Event::new("boundary", "query")
            .boundary(
                BoundaryKind::SearchDecision,
                "query",
                r#"{"query":"mpv","decision":"basename","evidence":["gimp"]}"#,
            )
            .build_for_test(1);
        event.sequence = 1;
        let report = replay_events(&[event]);
        assert_eq!(report.applied_events, 1);
        assert_eq!(report.state.current_query, "mpv");
        assert_eq!(report.state.last_decisions.len(), 1);
    }

    #[test]
    fn replay_keeps_independent_process_streams_independent() {
        let first = Event::new("test", "first").build_for_test(1);
        let mut second = Event::new("test", "second").build_for_test(1);
        second.instance_id = 2;
        let report = replay_events(&[first, second]);
        assert_eq!(report.applied_events, 2);
        assert_eq!(report.rejected_events, 0);
        assert_eq!(report.stream_count, 2);
    }

    #[test]
    fn persistent_ring_round_trips_a_checksummed_event() {
        let path = std::env::temp_dir().join(format!(
            "applicationlauncher-trace-test-{}-{}.ring",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let event = Event::new("boundary", "snapshot")
            .boundary(
                BoundaryKind::WindowFeed,
                "snapshot",
                r#"{"window_count":3,"generation":4}"#,
            )
            .build_for_test(9);
        let payload = serde_json::to_vec(&event).unwrap();
        let header = encode_ring_header(event.sequence, &payload).unwrap();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.set_len((PERSISTENT_RING_SLOT_BYTES * PERSISTENT_RING_SLOTS) as u64)
            .unwrap();
        use std::os::unix::fs::FileExt;
        file.write_at(&payload, PERSISTENT_RING_HEADER_BYTES as u64)
            .unwrap();
        file.write_at(&header, 0).unwrap();
        drop(file);

        let events = read_ring_file(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence, 9);
        assert_eq!(replay_events(&events).state.window_count, 3);
        std::fs::remove_file(path).unwrap();
    }
}
