use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::{
    HistoryEntry, OutputGeometry, RestoreOutcomeStatus, RestoreReport, RestoreSpec, SnapshotDetail,
    TrackedWindow, WindowGeometry, WindowRestoreOutcome, app_key, now_ms,
};

const GEOMETRY_TOLERANCE: i32 = 2;

#[derive(Clone, Debug)]
pub(crate) struct PendingRestore {
    pub wanted: TrackedWindow,
    pub restore: RestoreSpec,
    pub outcome_index: usize,
    pub expected_window_id: Option<String>,
    pub launched: bool,
}

#[derive(Debug)]
pub(crate) struct PreparedRestore {
    pub report: RestoreReport,
    pub pending: Vec<PendingRestore>,
    pub baseline_ids: HashSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LayoutTarget {
    pub geometry: Option<WindowGeometry>,
    pub output_name: Option<String>,
    pub adjustment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowAssignment {
    pub pending_index: usize,
    pub window_id: String,
    pub ambiguous: bool,
}

pub fn restore_snapshot(snapshot: &SnapshotDetail, current: &[TrackedWindow]) -> RestoreReport {
    prepare_restore_specs(&snapshot.windows, current).report
}

pub fn restore_entries(entries: &[HistoryEntry], current: &[TrackedWindow]) -> RestoreReport {
    let specs = entries
        .iter()
        .map(|entry| (entry.window.clone(), entry.restore.clone()))
        .collect::<Vec<_>>();
    prepare_restore_specs(&specs, current).report
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum LaunchUnavailableCode {
    UnsupportedTerminalKind,
    MissingExecutable,
    MissingDesktopEntry,
    NonLaunchableDesktopEntry,
}

#[derive(Debug)]
struct LaunchUnavailable {
    code: LaunchUnavailableCode,
    detail: String,
}

impl fmt::Display for LaunchUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

struct LaunchPlan {
    program: PathBuf,
    arguments: Vec<OsString>,
    description: String,
}

impl LaunchPlan {
    fn run(self) -> Result<(), String> {
        let mut command = Command::new(&self.program);
        command.args(self.arguments);
        crate::process::spawn_and_reap(command).map_err(|err| {
            format!(
                "Could not launch {} with {}: {err}",
                self.description,
                self.program.display()
            )
        })
    }
}

pub(crate) fn unavailable_reason(restore: &RestoreSpec) -> Option<LaunchUnavailableCode> {
    launch_plan(restore).err().map(|error| error.code)
}

pub(crate) fn prepare_restore_specs(
    specs: &[(TrackedWindow, RestoreSpec)],
    current: &[TrackedWindow],
) -> PreparedRestore {
    let baseline_ids = current.iter().map(|window| window.id.clone()).collect();
    let existing = assign_windows(specs, current, &HashSet::new());
    let existing_by_spec = existing
        .into_iter()
        .map(|assignment| (assignment.pending_index, assignment))
        .collect::<HashMap<_, _>>();
    let mut report = RestoreReport {
        started_at_ms: now_ms(),
        ..Default::default()
    };
    let mut pending = Vec::new();
    for (index, (wanted, restore)) in specs.iter().enumerate() {
        let outcome_index = report.outcomes.len();
        if let Some(existing) = existing_by_spec.get(&index) {
            report.matched += 1;
            report.outcomes.push(WindowRestoreOutcome {
                title: wanted.title.clone(),
                saved_window_id: wanted.id.clone(),
                restored_window_id: Some(existing.window_id.clone()),
                status: RestoreOutcomeStatus::Pending,
                detail: if existing.ambiguous {
                    "Matched an existing window deterministically, but multiple candidates had equal metadata"
                        .into()
                } else {
                    "Matched an existing window; verified layout is pending".into()
                },
            });
            pending.push(PendingRestore {
                wanted: wanted.clone(),
                restore: restore.clone(),
                outcome_index,
                expected_window_id: Some(existing.window_id.clone()),
                launched: false,
            });
            continue;
        }
        match launch(restore) {
            Ok(()) => {
                report.launched += 1;
                report.outcomes.push(WindowRestoreOutcome {
                    title: wanted.title.clone(),
                    saved_window_id: wanted.id.clone(),
                    restored_window_id: None,
                    status: RestoreOutcomeStatus::Pending,
                    detail: "Launch requested; waiting for a new matching KWin window".into(),
                });
                pending.push(PendingRestore {
                    wanted: wanted.clone(),
                    restore: restore.clone(),
                    outcome_index,
                    expected_window_id: None,
                    launched: true,
                });
            }
            Err(err) => {
                let failure = format!("{}: {err}", wanted.title);
                report.failures.push(failure.clone());
                report.outcomes.push(WindowRestoreOutcome {
                    title: wanted.title.clone(),
                    saved_window_id: wanted.id.clone(),
                    restored_window_id: None,
                    status: RestoreOutcomeStatus::Failed,
                    detail: failure,
                });
            }
        }
    }
    report.in_progress = !pending.is_empty();
    if !report.in_progress {
        report.finished_at_ms = Some(now_ms());
    }
    PreparedRestore {
        report,
        pending,
        baseline_ids,
    }
}

pub(crate) fn assign_pending_windows(
    pending: &[PendingRestore],
    current: &[TrackedWindow],
    baseline_ids: &HashSet<String>,
    used_ids: &HashSet<String>,
) -> Vec<WindowAssignment> {
    let mut assignments = Vec::new();
    let mut used = used_ids.clone();
    let mut unresolved = Vec::new();
    for (index, item) in pending.iter().enumerate() {
        if let Some(expected) = item.expected_window_id.as_deref() {
            if !used.contains(expected) && current.iter().any(|window| window.id == expected) {
                used.insert(expected.to_string());
                assignments.push(WindowAssignment {
                    pending_index: index,
                    window_id: expected.to_string(),
                    ambiguous: false,
                });
            }
        } else {
            unresolved.push(index);
        }
    }
    let specs = unresolved
        .iter()
        .map(|index| {
            let item = &pending[*index];
            (item.wanted.clone(), item.restore.clone())
        })
        .collect::<Vec<_>>();
    let candidates = current
        .iter()
        .filter(|window| !baseline_ids.contains(&window.id) && !used.contains(&window.id))
        .cloned()
        .collect::<Vec<_>>();
    for mut assignment in assign_windows(&specs, &candidates, &used) {
        assignment.pending_index = unresolved[assignment.pending_index];
        used.insert(assignment.window_id.clone());
        assignments.push(assignment);
    }
    assignments
}

fn assign_windows(
    specs: &[(TrackedWindow, RestoreSpec)],
    current: &[TrackedWindow],
    used_ids: &HashSet<String>,
) -> Vec<WindowAssignment> {
    let mut pairs = Vec::new();
    let current_restores = current
        .iter()
        .map(super::infer_restore_spec)
        .collect::<Vec<_>>();
    for (spec_index, (wanted, restore)) in specs.iter().enumerate() {
        for (window_index, window) in current
            .iter()
            .enumerate()
            .filter(|(_, window)| !used_ids.contains(&window.id))
        {
            if let Some(score) =
                window_match_score(wanted, restore, window, &current_restores[window_index])
            {
                pairs.push((score, spec_index, window.id.clone()));
            }
        }
    }
    pairs.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
    });
    let top_scores = pairs.iter().fold(HashMap::new(), |mut scores, pair| {
        scores
            .entry(pair.1)
            .and_modify(|score: &mut (i32, usize)| {
                if score.0 == pair.0 {
                    score.1 += 1;
                }
            })
            .or_insert((pair.0, 1));
        scores
    });
    let mut assigned_specs = HashSet::new();
    let mut assigned_windows = used_ids.clone();
    let mut assignments = Vec::new();
    for (score, spec_index, window_id) in pairs {
        if assigned_specs.contains(&spec_index) || assigned_windows.contains(&window_id) {
            continue;
        }
        assigned_specs.insert(spec_index);
        assigned_windows.insert(window_id.clone());
        let ambiguous = top_scores
            .get(&spec_index)
            .is_some_and(|(top, count)| *top == score && *count > 1);
        assignments.push(WindowAssignment {
            pending_index: spec_index,
            window_id,
            ambiguous,
        });
    }
    assignments
}

fn window_match_score(
    wanted: &TrackedWindow,
    restore: &RestoreSpec,
    current: &TrackedWindow,
    current_restore: &RestoreSpec,
) -> Option<i32> {
    if app_key(wanted) != app_key(current) {
        return None;
    }
    let mut score = 100;
    let wanted_title = stable_window_title(&wanted.title);
    let current_title = stable_window_title(&current.title);
    if !wanted_title.is_empty() && wanted_title == current_title {
        score += 1_000;
    }
    if wanted.class.eq_ignore_ascii_case(&current.class) {
        score += 40;
    }
    if !wanted.desktop_file_name.is_empty()
        && wanted
            .desktop_file_name
            .eq_ignore_ascii_case(&current.desktop_file_name)
    {
        score += 80;
    }
    if restore.terminal_kind.is_some() {
        if restore.terminal_kind == current_restore.terminal_kind {
            score += 500;
        } else if wanted_title != current_title {
            return None;
        }
        match (restore.cwd.as_deref(), current_restore.cwd.as_deref()) {
            (Some(wanted_cwd), Some(current_cwd)) if same_path(wanted_cwd, current_cwd) => {
                score += 800;
            }
            (Some(_), Some(_)) if wanted_title != current_title => return None,
            _ => {}
        }
    }
    if restore.executable.as_deref().and_then(executable_name)
        == current_restore
            .executable
            .as_deref()
            .and_then(executable_name)
        && restore.executable.is_some()
    {
        score += 120;
    }
    Some(score)
}

fn executable_name(path: &str) -> Option<&str> {
    Path::new(path).file_name()?.to_str()
}

fn same_path(left: &str, right: &str) -> bool {
    expand_home(left) == expand_home(right)
}

fn stable_window_title(title: &str) -> String {
    let without_transient_frames = title
        .replace("[ . ] Action Required", "")
        .replace("[ ! ] Action Required", "");
    without_transient_frames
        .split(" - ")
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" - ")
        .chars()
        .filter(|character| !matches!(*character as u32, 0x2800..=0x28ff))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(crate) fn current_output_geometries() -> Vec<OutputGeometry> {
    let output = crate::process::output_with_timeout(
        {
            let mut command = Command::new("kscreen-doctor");
            command.arg("-j");
            command
        },
        Duration::from_secs(2),
    );
    let Ok(output) = output else {
        return Vec::new();
    };
    let Ok(document) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    parse_output_geometries(&document)
}

fn parse_output_geometries(document: &serde_json::Value) -> Vec<OutputGeometry> {
    let mut outputs = document["outputs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|output| output["enabled"].as_bool().unwrap_or(false))
        .filter_map(|output| {
            let scale = output["scale"].as_f64().unwrap_or(1.0).max(0.01);
            let width = (output["size"]["width"].as_f64()? / scale).round() as i32;
            let height = (output["size"]["height"].as_f64()? / scale).round() as i32;
            Some((
                output["priority"].as_i64().unwrap_or(i64::MAX),
                OutputGeometry {
                    name: output["name"].as_str()?.to_string(),
                    x: output["pos"]["x"].as_f64()?.round() as i32,
                    y: output["pos"]["y"].as_f64()?.round() as i32,
                    width,
                    height,
                    scale_milli: (scale * 1_000.0).round() as i32,
                },
            ))
        })
        .collect::<Vec<_>>();
    outputs.sort_by_key(|(priority, _)| *priority);
    outputs.into_iter().map(|(_, output)| output).collect()
}

pub(crate) fn layout_target(wanted: &TrackedWindow, outputs: &[OutputGeometry]) -> LayoutTarget {
    let saved_geometry = if wanted.maximized
        || wanted.maximized_horizontally
        || wanted.maximized_vertically
        || wanted.fullscreen
    {
        wanted.normal_geometry.unwrap_or(WindowGeometry {
            x: wanted.x,
            y: wanted.y,
            width: wanted.width,
            height: wanted.height,
        })
    } else {
        WindowGeometry {
            x: wanted.x,
            y: wanted.y,
            width: wanted.width,
            height: wanted.height,
        }
    };
    if !saved_geometry.is_valid() {
        return LayoutTarget {
            geometry: None,
            output_name: None,
            adjustment: Some("Saved geometry was unavailable".into()),
        };
    }
    if outputs.is_empty() {
        return LayoutTarget {
            geometry: Some(saved_geometry),
            output_name: (!wanted.output.is_empty()).then(|| wanted.output.clone()),
            adjustment: Some(
                "Current monitor topology was unavailable; used saved coordinates".into(),
            ),
        };
    }

    let target_output = outputs
        .iter()
        .find(|output| output.name == wanted.output)
        .or_else(|| {
            wanted.output_geometry.as_ref().and_then(|saved_output| {
                outputs.iter().max_by_key(|output| {
                    overlap_area(
                        WindowGeometry {
                            x: saved_output.x,
                            y: saved_output.y,
                            width: saved_output.width,
                            height: saved_output.height,
                        },
                        WindowGeometry {
                            x: output.x,
                            y: output.y,
                            width: output.width,
                            height: output.height,
                        },
                    )
                })
            })
        })
        .unwrap_or(&outputs[0]);

    let (geometry, adjusted) = if let Some(saved_output) = wanted
        .output_geometry
        .as_ref()
        .filter(|output| output.width > 0 && output.height > 0)
    {
        let geometry = WindowGeometry {
            x: map_axis(
                saved_geometry.x,
                saved_geometry.width,
                saved_output.x,
                saved_output.width,
                target_output.x,
                target_output.width,
            ),
            y: map_axis(
                saved_geometry.y,
                saved_geometry.height,
                saved_output.y,
                saved_output.height,
                target_output.y,
                target_output.height,
            ),
            width: saved_geometry.width.min(target_output.width).max(1),
            height: saved_geometry.height.min(target_output.height).max(1),
        };
        let adjusted = geometry != saved_geometry || saved_output.name != target_output.name;
        (geometry, adjusted)
    } else {
        let geometry = clamp_geometry(saved_geometry, target_output);
        (
            geometry,
            geometry != saved_geometry
                || (!wanted.output.is_empty() && wanted.output != target_output.name),
        )
    };
    LayoutTarget {
        geometry: Some(geometry),
        output_name: Some(target_output.name.clone()),
        adjustment: adjusted.then(|| {
            format!(
                "Translated saved geometry from {} to {} for the current monitor topology",
                wanted
                    .output_geometry
                    .as_ref()
                    .map(|output| output.name.as_str())
                    .unwrap_or(wanted.output.as_str()),
                target_output.name
            )
        }),
    }
}

fn map_axis(
    position: i32,
    size: i32,
    saved_origin: i32,
    saved_extent: i32,
    target_origin: i32,
    target_extent: i32,
) -> i32 {
    let target_size = size.min(target_extent).max(1);
    let saved_span = (saved_extent - size).max(0);
    let target_span = (target_extent - target_size).max(0);
    if saved_span == 0 {
        return target_origin;
    }
    let relative = (position - saved_origin).clamp(0, saved_span) as f64 / saved_span as f64;
    target_origin + (relative * target_span as f64).round() as i32
}

fn clamp_geometry(mut geometry: WindowGeometry, output: &OutputGeometry) -> WindowGeometry {
    geometry.width = geometry.width.min(output.width).max(1);
    geometry.height = geometry.height.min(output.height).max(1);
    geometry.x = geometry
        .x
        .clamp(output.x, output.x + output.width - geometry.width);
    geometry.y = geometry
        .y
        .clamp(output.y, output.y + output.height - geometry.height);
    geometry
}

fn overlap_area(left: WindowGeometry, right: WindowGeometry) -> i64 {
    let width = (left.x + left.width).min(right.x + right.width) - left.x.max(right.x);
    let height = (left.y + left.height).min(right.y + right.height) - left.y.max(right.y);
    i64::from(width.max(0)) * i64::from(height.max(0))
}

pub(crate) fn apply_layout_once(
    current_id: &str,
    wanted: &TrackedWindow,
    target: &LayoutTarget,
) -> Result<(), String> {
    let args = layout_args(current_id, wanted, target);
    let mut command = Command::new(crate::process::kdotool_path());
    command.args(&args);
    let status = crate::process::status_with_timeout(command, Duration::from_secs(3))
        .map_err(|err| err.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("kdotool exited with {status}"))
}

fn layout_args(id: &str, wanted: &TrackedWindow, target: &LayoutTarget) -> Vec<String> {
    let mut args = Vec::new();
    if wanted.on_all_desktops {
        args.extend(["set_desktop_for_window".into(), id.into(), "all".into()]);
    } else if wanted.desktop > 0 {
        args.extend([
            "set_desktop_for_window".into(),
            id.into(),
            wanted.desktop.to_string(),
        ]);
    }

    let mut clear_state = vec!["windowstate".into()];
    for property in [
        "fullscreen",
        "maximized",
        "maximized_horz",
        "maximized_vert",
        "minimized",
        "above",
        "below",
        "shaded",
        "skip_taskbar",
        "skip_pager",
        "no_border",
    ] {
        clear_state.extend(["--remove".into(), property.into()]);
    }
    args.extend(state_args_with_window(clear_state, id));

    let mut pre_geometry_state = vec!["windowstate".into()];
    for (enabled, property) in [
        (wanted.keep_above, "above"),
        (wanted.keep_below, "below"),
        (wanted.skip_taskbar, "skip_taskbar"),
        (wanted.skip_pager, "skip_pager"),
        (wanted.no_border, "no_border"),
    ] {
        if enabled {
            pre_geometry_state.extend(["--add".into(), property.into()]);
        }
    }
    if pre_geometry_state.len() > 1 {
        args.extend(state_args_with_window(pre_geometry_state, id));
    }

    if let Some(geometry) = target.geometry.filter(|geometry| geometry.is_valid()) {
        args.extend([
            "windowsize".into(),
            id.into(),
            geometry.width.to_string(),
            geometry.height.to_string(),
        ]);
        args.extend([
            "windowmove".into(),
            id.into(),
            geometry.x.to_string(),
            geometry.y.to_string(),
        ]);
    }

    let mut state_args = vec!["windowstate".into()];
    if wanted.fullscreen {
        state_args.extend(["--add".into(), "fullscreen".into()]);
    } else if wanted.maximized || wanted.maximized_horizontally || wanted.maximized_vertically {
        if wanted.maximized || wanted.maximized_horizontally {
            state_args.extend(["--add".into(), "maximized_horz".into()]);
        }
        if wanted.maximized || wanted.maximized_vertically {
            state_args.extend(["--add".into(), "maximized_vert".into()]);
        }
    }
    if wanted.minimized {
        state_args.extend(["--add".into(), "minimized".into()]);
    }
    if wanted.shaded {
        state_args.extend(["--add".into(), "shaded".into()]);
    }
    if state_args.len() > 1 {
        args.extend(state_args_with_window(state_args, id));
    }
    args
}

pub(crate) fn verify_layout(
    current: &TrackedWindow,
    wanted: &TrackedWindow,
    target: &LayoutTarget,
) -> Result<(), String> {
    let mut mismatches = Vec::new();
    if let Some(expected) = target.geometry {
        let restores_special_geometry = wanted.maximized
            || wanted.maximized_horizontally
            || wanted.maximized_vertically
            || wanted.fullscreen;
        let actual = if restores_special_geometry {
            current.normal_geometry
        } else {
            Some(WindowGeometry {
                x: current.x,
                y: current.y,
                width: current.width,
                height: current.height,
            })
        };
        match actual {
            Some(actual) if !geometry_near(expected, actual) => {
                let kind = if restores_special_geometry {
                    "normal geometry"
                } else {
                    "geometry"
                };
                mismatches.push(format!(
                    "{kind} expected {}x{}+{},{} but KWin reported {}x{}+{},{}",
                    expected.width,
                    expected.height,
                    expected.x,
                    expected.y,
                    actual.width,
                    actual.height,
                    actual.x,
                    actual.y
                ));
            }
            None => mismatches.push("normal geometry was unavailable after restore".into()),
            _ => {}
        }
    }
    if wanted.fullscreen != current.fullscreen {
        mismatches.push(format!("fullscreen expected {}", wanted.fullscreen));
    }
    if wanted.maximized != current.maximized {
        mismatches.push(format!("maximized expected {}", wanted.maximized));
    }
    if wanted.maximized_horizontally != current.maximized_horizontally && !wanted.maximized {
        mismatches.push(format!(
            "horizontal maximization expected {}",
            wanted.maximized_horizontally
        ));
    }
    if wanted.maximized_vertically != current.maximized_vertically && !wanted.maximized {
        mismatches.push(format!(
            "vertical maximization expected {}",
            wanted.maximized_vertically
        ));
    }
    if wanted.minimized != current.minimized {
        mismatches.push(format!("minimized expected {}", wanted.minimized));
    }
    if wanted.on_all_desktops != current.on_all_desktops {
        mismatches.push(format!(
            "all-desktops state expected {}",
            wanted.on_all_desktops
        ));
    } else if !wanted.on_all_desktops && wanted.desktop > 0 && wanted.desktop != current.desktop {
        mismatches.push(format!("desktop expected {}", wanted.desktop));
    }
    if let Some(expected_output) = target.output_name.as_deref()
        && current.output != expected_output
    {
        if current.output.is_empty() {
            mismatches.push(format!(
                "output expected {expected_output}, but KWin output metadata was unavailable"
            ));
        } else {
            mismatches.push(format!("output expected {expected_output}"));
        }
    }
    for (name, expected, actual) in [
        ("keep-above", wanted.keep_above, current.keep_above),
        ("keep-below", wanted.keep_below, current.keep_below),
        ("shaded", wanted.shaded, current.shaded),
        ("skip-taskbar", wanted.skip_taskbar, current.skip_taskbar),
        ("skip-pager", wanted.skip_pager, current.skip_pager),
        ("no-border", wanted.no_border, current.no_border),
    ] {
        if expected != actual {
            mismatches.push(format!("{name} expected {expected}"));
        }
    }
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(mismatches.join("; "))
    }
}

fn geometry_near(expected: WindowGeometry, actual: WindowGeometry) -> bool {
    (expected.x - actual.x).abs() <= GEOMETRY_TOLERANCE
        && (expected.y - actual.y).abs() <= GEOMETRY_TOLERANCE
        && (expected.width - actual.width).abs() <= GEOMETRY_TOLERANCE
        && (expected.height - actual.height).abs() <= GEOMETRY_TOLERANCE
}

pub(crate) fn restore_stacking(windows: &[(TrackedWindow, String)]) -> Result<(), String> {
    let args = stacking_args(windows);
    if args.is_empty() {
        return Ok(());
    }
    let mut command = Command::new(crate::process::kdotool_path());
    command.args(args);
    let status = crate::process::status_with_timeout(command, Duration::from_secs(3))
        .map_err(|err| err.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("kdotool exited with {status}"))
}

fn stacking_args(windows: &[(TrackedWindow, String)]) -> Vec<String> {
    let mut windows = windows.to_vec();
    windows.sort_by_key(|(window, _)| window.stacking_order);
    let mut args = Vec::new();
    for (_, id) in &windows {
        args.extend(["windowraise".to_string(), id.clone()]);
    }
    if let Some((_, id)) = windows.iter().find(|(window, _)| window.active) {
        args.extend(activation_args(id));
    }
    args
}

fn activation_args(id: &str) -> Vec<String> {
    vec![
        "windowactivate".into(),
        id.into(),
        "windowraise".into(),
        id.into(),
    ]
}

fn state_args_with_window(mut args: Vec<String>, id: &str) -> Vec<String> {
    args.push(id.to_string());
    args
}

fn launch(restore: &RestoreSpec) -> Result<(), String> {
    launch_plan(restore)
        .map_err(|error| error.to_string())?
        .run()
}

fn launch_plan(restore: &RestoreSpec) -> Result<LaunchPlan, LaunchUnavailable> {
    if let Some(kind) = restore.terminal_kind.as_deref() {
        return terminal_launch_plan(kind, restore.cwd.as_deref());
    }
    let key = restore.app_key.to_lowercase();
    if key.contains("dolphin")
        && let Some(program) = crate::process::executable_path("dolphin")
    {
        let mut arguments = vec![OsString::from("--new-window")];
        if let Some(cwd) = restore.cwd.as_deref() {
            arguments.push(expand_home(cwd).into_os_string());
        }
        return Ok(LaunchPlan {
            program,
            arguments,
            description: "Dolphin window".into(),
        });
    }
    if key.contains("pcmanfm")
        && let Some(program) = crate::process::executable_path("pcmanfm")
    {
        let mut arguments = vec![OsString::from("--new-win")];
        if let Some(cwd) = restore.cwd.as_deref() {
            arguments.push(expand_home(cwd).into_os_string());
        }
        return Ok(LaunchPlan {
            program,
            arguments,
            description: "PCManFM window".into(),
        });
    }
    let desktop = resolve_desktop_file(restore)?;
    validate_desktop_file(&desktop)?;
    let program = required_executable("gio")?;
    Ok(LaunchPlan {
        program,
        arguments: vec![OsString::from("launch"), desktop.clone().into_os_string()],
        description: format!("desktop entry {}", desktop.display()),
    })
}

fn validate_desktop_file(path: &Path) -> Result<(), LaunchUnavailable> {
    let contents = std::fs::read_to_string(path).map_err(|err| LaunchUnavailable {
        code: LaunchUnavailableCode::NonLaunchableDesktopEntry,
        detail: format!("Could not read desktop entry {}: {err}", path.display()),
    })?;
    let mut in_desktop_entry = false;
    let mut application_type = false;
    let mut hidden = false;
    let mut no_display = false;
    let mut has_exec = false;
    let mut dbus_activatable = false;
    let mut try_exec = None;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Type" => application_type = value.trim() == "Application",
            "Hidden" => hidden = value.trim().eq_ignore_ascii_case("true"),
            "NoDisplay" => no_display = value.trim().eq_ignore_ascii_case("true"),
            "Exec" => has_exec = !value.trim().is_empty(),
            "TryExec" => try_exec = Some(value.trim().to_string()),
            "DBusActivatable" => {
                dbus_activatable = value.trim().eq_ignore_ascii_case("true");
            }
            _ => {}
        }
    }
    if let Some(try_exec) = try_exec.filter(|value| !value.is_empty())
        && crate::process::executable_path(&try_exec).is_none()
    {
        return Err(LaunchUnavailable {
            code: LaunchUnavailableCode::MissingExecutable,
            detail: format!(
                "Desktop entry {} requires unavailable executable {try_exec}",
                path.display()
            ),
        });
    }
    if application_type && !hidden && !no_display && (has_exec || dbus_activatable) {
        return Ok(());
    }
    Err(LaunchUnavailable {
        code: LaunchUnavailableCode::NonLaunchableDesktopEntry,
        detail: format!(
            "Desktop entry {} is hidden or has no launchable application action",
            path.display()
        ),
    })
}

fn required_executable(program: &str) -> Result<PathBuf, LaunchUnavailable> {
    crate::process::executable_path(program).ok_or_else(|| LaunchUnavailable {
        code: LaunchUnavailableCode::MissingExecutable,
        detail: format!("Required executable {program} is unavailable"),
    })
}

fn resolve_desktop_file(restore: &RestoreSpec) -> Result<PathBuf, LaunchUnavailable> {
    let mut data_dirs = Vec::new();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/".into()))
                .join(".local/share")
        });
    data_dirs.push(data_home.clone());
    data_dirs.push(data_home.join("flatpak/exports/share"));
    data_dirs.extend(
        std::env::var("XDG_DATA_DIRS")
            .unwrap_or_else(|_| "/usr/local/share:/usr/share".into())
            .split(':')
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    );
    data_dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    resolve_desktop_file_in(restore, &data_dirs)
}

fn resolve_desktop_file_in(
    restore: &RestoreSpec,
    data_dirs: &[PathBuf],
) -> Result<PathBuf, LaunchUnavailable> {
    let candidates = [restore.desktop_file.as_deref(), Some(&restore.app_key)];
    for candidate in candidates.into_iter().flatten() {
        let candidate_path = expand_home(candidate);
        if candidate_path.is_file() && candidate_path.components().count() > 1 {
            return Ok(candidate_path);
        }
        let candidate = Path::new(candidate)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(candidate)
            .trim();
        if candidate.is_empty() {
            continue;
        }
        let file_names = if candidate.ends_with(".desktop") {
            vec![candidate.to_string(), format!("{candidate}.desktop")]
        } else {
            vec![format!("{candidate}.desktop")]
        };
        for file_name in file_names {
            if data_dirs
                .iter()
                .any(|dir| dir.join("applications").join(&file_name).is_file())
            {
                return Ok(data_dirs
                    .iter()
                    .map(|dir| dir.join("applications").join(&file_name))
                    .find(|path| path.is_file())
                    .expect("desktop file existence was checked"));
            }
        }
    }
    Err(LaunchUnavailable {
        code: LaunchUnavailableCode::MissingDesktopEntry,
        detail: format!(
            "No installed desktop file was found for {}",
            restore.app_key
        ),
    })
}

fn terminal_launch_plan(kind: &str, cwd: Option<&str>) -> Result<LaunchPlan, LaunchUnavailable> {
    let command = terminal_shell_command(kind)?;
    let program = required_executable("xfce4-terminal")?;
    required_executable("fish")?;
    if let Some((executable, _)) = command {
        required_executable(executable)?;
        required_executable("bash")?;
    }
    let cwd = cwd
        .map(expand_home)
        .filter(|path| path.is_dir())
        .unwrap_or_else(home_directory);
    Ok(LaunchPlan {
        program,
        arguments: vec![
            OsString::from("--working-directory"),
            cwd.into_os_string(),
            OsString::from("--command"),
            OsString::from(terminal_restore_invocation(
                command.map(|(_, shell_command)| shell_command),
            )),
        ],
        description: format!("{kind} terminal"),
    })
}

fn terminal_restore_invocation(shell_command: Option<&str>) -> String {
    match shell_command {
        Some(shell_command) => {
            format!(r#"fish -lc 'exec bash -m -c \"{shell_command}; exec fish\"'"#)
        }
        None => "fish -l".into(),
    }
}

fn terminal_shell_command(
    kind: &str,
) -> Result<Option<(&'static str, &'static str)>, LaunchUnavailable> {
    let command = match kind {
        "shell" => None,
        "codex" => Some(("codex", "codex resume --last")),
        "agy" => Some(("agy", "agy -c")),
        "htop" => Some(("htop", "htop")),
        "nvtop" => Some(("nvtop", "nvtop")),
        _ => {
            return Err(LaunchUnavailable {
                code: LaunchUnavailableCode::UnsupportedTerminalKind,
                detail: format!("Unsupported terminal restore kind {kind}"),
            });
        }
    };
    Ok(command)
}

fn home_directory() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return home_directory();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_directory().join(rest);
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked_window(id: &str, title: &str, app: &str) -> TrackedWindow {
        TrackedWindow {
            id: id.into(),
            title: title.into(),
            class: app.into(),
            desktop_file_name: app.into(),
            x: 100,
            y: 80,
            width: 400,
            height: 300,
            output: "DP-1".into(),
            ..Default::default()
        }
    }

    #[test]
    fn legacy_snapshot_windows_receive_safe_restore_defaults() {
        let window: TrackedWindow = serde_json::from_value(serde_json::json!({
            "id": "legacy",
            "title": "Legacy",
            "class": "legacy-app",
            "x": 10,
            "y": 20,
            "width": 800,
            "height": 600,
            "output": "DP-1"
        }))
        .unwrap();

        assert!(window.normal_geometry.is_none());
        assert!(window.output_geometry.is_none());
        assert!(window.activities.is_empty());
        assert!(!window.keep_above);
        assert!(!window.maximized_horizontally);
    }

    #[test]
    fn reopened_window_is_activated_and_raised() {
        assert_eq!(
            activation_args("{window-id}"),
            [
                "windowactivate",
                "{window-id}",
                "windowraise",
                "{window-id}"
            ]
        );
    }

    #[test]
    fn matching_prefers_stable_titles_over_candidate_order() {
        let specs = vec![
            (
                tracked_window("saved-a", "Document A", "org.example.Editor"),
                RestoreSpec::default(),
            ),
            (
                tracked_window("saved-b", "Document B", "org.example.Editor"),
                RestoreSpec::default(),
            ),
        ];
        let current = vec![
            tracked_window("current-b", "Document B", "org.example.Editor"),
            tracked_window("current-a", "Document A", "org.example.Editor"),
        ];
        let assignments = assign_windows(&specs, &current, &HashSet::new())
            .into_iter()
            .map(|assignment| (assignment.pending_index, assignment.window_id))
            .collect::<HashMap<_, _>>();

        assert_eq!(assignments.get(&0).map(String::as_str), Some("current-a"));
        assert_eq!(assignments.get(&1).map(String::as_str), Some("current-b"));
    }

    #[test]
    fn transient_title_frames_do_not_change_window_identity() {
        let wanted = tracked_window("saved", "codex - ~/Dev/app - Terminal", "xfce4-terminal");
        let current = tracked_window(
            "current",
            "codex - [ ! ] Action Required - ⣴ ~/Dev/app - Terminal",
            "xfce4-terminal",
        );

        assert_eq!(
            stable_window_title(&wanted.title),
            stable_window_title(&current.title)
        );
    }

    #[test]
    fn launched_window_matching_excludes_the_prelaunch_baseline() {
        let wanted = tracked_window("saved", "Editor", "org.example.Editor");
        let pending = vec![PendingRestore {
            wanted: wanted.clone(),
            restore: RestoreSpec::default(),
            outcome_index: 0,
            expected_window_id: None,
            launched: true,
        }];
        let old = tracked_window("old", "Editor", "org.example.Editor");
        let new = tracked_window("new", "Editor", "org.example.Editor");
        let assignments = assign_pending_windows(
            &pending,
            &[old, new],
            &HashSet::from(["old".to_string()]),
            &HashSet::new(),
        );

        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].window_id, "new");
    }

    #[test]
    fn output_translation_preserves_relative_placement() {
        let mut wanted = tracked_window("saved", "Editor", "org.example.Editor");
        wanted.output_geometry = Some(OutputGeometry {
            name: "DP-1".into(),
            x: 0,
            y: 0,
            width: 1_000,
            height: 800,
            scale_milli: 1_000,
        });
        let target = layout_target(
            &wanted,
            &[OutputGeometry {
                name: "DP-1".into(),
                x: 2_000,
                y: 100,
                width: 2_000,
                height: 1_600,
                scale_milli: 1_000,
            }],
        );

        assert_eq!(
            target.geometry,
            Some(WindowGeometry {
                x: 2_267,
                y: 308,
                width: 400,
                height: 300,
            })
        );
        assert!(target.adjustment.is_some());
    }

    #[test]
    fn output_parser_uses_logical_dimensions_and_primary_order() {
        let document = serde_json::json!({
            "outputs": [
                {"name": "secondary", "enabled": true, "priority": 2, "scale": 1.0, "pos": {"x": 1000, "y": 0}, "size": {"width": 1000, "height": 800}},
                {"name": "primary", "enabled": true, "priority": 1, "scale": 2.0, "pos": {"x": 0, "y": 0}, "size": {"width": 2000, "height": 1600}},
                {"name": "disabled", "enabled": false, "priority": 0, "scale": 1.0, "pos": {"x": 0, "y": 0}, "size": {"width": 1, "height": 1}}
            ]
        });

        let outputs = parse_output_geometries(&document);
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].name, "primary");
        assert_eq!((outputs[0].width, outputs[0].height), (1_000, 800));
        assert_eq!(outputs[0].scale_milli, 2_000);
    }

    #[test]
    fn layout_clears_maximization_before_sizing_and_reapplies_state() {
        let mut wanted = tracked_window("saved", "Editor", "org.example.Editor");
        wanted.maximized = true;
        wanted.keep_above = true;
        let target = LayoutTarget {
            geometry: Some(WindowGeometry {
                x: 10,
                y: 20,
                width: 800,
                height: 600,
            }),
            output_name: Some("DP-1".into()),
            adjustment: None,
        };
        let args = layout_args("window", &wanted, &target);
        let clear_index = args
            .windows(2)
            .position(|pair| pair == ["--remove", "maximized"])
            .unwrap();
        let size_index = args
            .iter()
            .position(|argument| argument == "windowsize")
            .unwrap();
        let add_index = args
            .windows(2)
            .position(|pair| pair == ["--add", "maximized_horz"])
            .unwrap();

        assert!(clear_index < size_index);
        assert!(size_index < add_index);
        assert!(args.windows(2).any(|pair| pair == ["--add", "above"]));
    }

    #[test]
    fn maximized_restore_verifies_the_saved_normal_geometry() {
        let geometry = WindowGeometry {
            x: 10,
            y: 20,
            width: 800,
            height: 600,
        };
        let mut wanted = tracked_window("saved", "Editor", "org.example.Editor");
        wanted.maximized = true;
        wanted.normal_geometry = Some(geometry);
        let mut current = wanted.clone();
        current.id = "current".into();
        current.x = 0;
        current.y = 0;
        current.width = 1_920;
        current.height = 1_080;
        current.normal_geometry = Some(geometry);
        let target = LayoutTarget {
            geometry: Some(geometry),
            output_name: Some("DP-1".into()),
            adjustment: None,
        };

        assert_eq!(verify_layout(&current, &wanted, &target), Ok(()));
        current.normal_geometry = None;
        assert!(
            verify_layout(&current, &wanted, &target)
                .unwrap_err()
                .contains("normal geometry was unavailable")
        );
    }

    #[test]
    fn restore_requires_output_and_all_desktops_metadata_to_match() {
        let mut wanted = tracked_window("saved", "Editor", "org.example.Editor");
        wanted.on_all_desktops = true;
        let mut current = wanted.clone();
        current.id = "current".into();
        current.on_all_desktops = false;
        current.output.clear();
        let target = LayoutTarget {
            geometry: Some(WindowGeometry {
                x: wanted.x,
                y: wanted.y,
                width: wanted.width,
                height: wanted.height,
            }),
            output_name: Some("DP-1".into()),
            adjustment: None,
        };

        let mismatch = verify_layout(&current, &wanted, &target).unwrap_err();
        assert!(mismatch.contains("all-desktops state expected true"));
        assert!(mismatch.contains("KWin output metadata was unavailable"));
    }

    #[test]
    fn stacking_replays_bottom_to_top_and_reactivates_the_saved_window() {
        let mut top = tracked_window("top", "Top", "org.example.Editor");
        top.stacking_order = 20;
        top.active = true;
        let mut bottom = tracked_window("bottom", "Bottom", "org.example.Editor");
        bottom.stacking_order = 10;
        let args = stacking_args(&[
            (top, "current-top".into()),
            (bottom, "current-bottom".into()),
        ]);

        assert_eq!(
            args,
            [
                "windowraise",
                "current-bottom",
                "windowraise",
                "current-top",
                "windowactivate",
                "current-top",
                "windowraise",
                "current-top",
            ]
        );
    }

    #[test]
    fn resolves_desktop_ids_that_already_end_in_desktop() {
        let root = std::env::temp_dir().join(format!(
            "applicationlauncher-desktop-id-{}",
            std::process::id()
        ));
        let applications = root.join("applications");
        std::fs::create_dir_all(&applications).unwrap();
        std::fs::write(
            applications.join("org.telegram.desktop.desktop"),
            "[Desktop Entry]\nName=Telegram\n",
        )
        .unwrap();
        let restore = RestoreSpec {
            app_key: "org.telegram".into(),
            desktop_file: Some("org.telegram.desktop".into()),
            ..Default::default()
        };

        assert_eq!(
            resolve_desktop_file_in(&restore, std::slice::from_ref(&root)).unwrap(),
            applications.join("org.telegram.desktop.desktop")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolves_absolute_and_flatpak_desktop_entries() {
        let root = std::env::temp_dir().join(format!(
            "applicationlauncher-desktop-paths-{}",
            std::process::id()
        ));
        let direct = root.join("direct.desktop");
        let flatpak = root.join("flatpak/exports/share/applications");
        std::fs::create_dir_all(&flatpak).unwrap();
        std::fs::write(&direct, "[Desktop Entry]\nName=Direct\n").unwrap();
        std::fs::write(
            flatpak.join("org.example.Flatpak.desktop"),
            "[Desktop Entry]\nName=Flatpak\n",
        )
        .unwrap();

        let direct_restore = RestoreSpec {
            app_key: "direct".into(),
            desktop_file: Some(direct.display().to_string()),
            ..RestoreSpec::default()
        };
        assert_eq!(
            resolve_desktop_file_in(&direct_restore, &[]).unwrap(),
            direct
        );

        let flatpak_restore = RestoreSpec {
            app_key: "org.example.Flatpak".into(),
            desktop_file: Some("org.example.Flatpak".into()),
            ..RestoreSpec::default()
        };
        assert_eq!(
            resolve_desktop_file_in(&flatpak_restore, &[root.join("flatpak/exports/share")])
                .unwrap(),
            flatpak.join("org.example.Flatpak.desktop")
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn terminal_restore_kinds_are_explicit_and_unknown_kinds_are_rejected() {
        assert_eq!(terminal_shell_command("shell").unwrap(), None);
        for kind in ["codex", "agy", "htop", "nvtop"] {
            assert!(terminal_shell_command(kind).unwrap().is_some());
        }
        assert_eq!(
            terminal_shell_command("unknown").unwrap_err().code,
            LaunchUnavailableCode::UnsupportedTerminalKind
        );
    }

    #[test]
    fn terminal_restore_uses_a_monitor_mode_wrapper_for_job_control() {
        assert_eq!(
            terminal_restore_invocation(Some("codex resume --last")),
            r#"fish -lc 'exec bash -m -c \"codex resume --last; exec fish\"'"#
        );
        assert_eq!(terminal_restore_invocation(None), "fish -l");
    }

    #[test]
    fn absent_desktop_entries_have_a_stable_unavailable_reason() {
        let restore = RestoreSpec {
            app_key: "definitely-missing-applicationlauncher-test-entry".into(),
            ..RestoreSpec::default()
        };

        assert_eq!(
            resolve_desktop_file_in(&restore, &[]).unwrap_err().code,
            LaunchUnavailableCode::MissingDesktopEntry
        );
    }

    #[test]
    fn hidden_and_non_application_desktop_entries_are_not_reopenable() {
        let root = std::env::temp_dir().join(format!(
            "applicationlauncher-hidden-desktop-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let visible = root.join("visible.desktop");
        let hidden = root.join("hidden.desktop");
        let link = root.join("link.desktop");
        let missing_try_exec = root.join("missing-try-exec.desktop");
        std::fs::write(
            &visible,
            "[Desktop Entry]\nType=Application\nExec=example\n",
        )
        .unwrap();
        std::fs::write(
            &hidden,
            "[Desktop Entry]\nType=Application\nExec=example\nNoDisplay=true\n",
        )
        .unwrap();
        std::fs::write(
            &link,
            "[Desktop Entry]\nType=Link\nURL=https://example.com\n",
        )
        .unwrap();
        std::fs::write(
            &missing_try_exec,
            "[Desktop Entry]\nType=Application\nExec=example\nTryExec=/definitely/missing/applicationlauncher-test\n",
        )
        .unwrap();

        assert!(validate_desktop_file(&visible).is_ok());
        for path in [&hidden, &link] {
            assert_eq!(
                validate_desktop_file(path).unwrap_err().code,
                LaunchUnavailableCode::NonLaunchableDesktopEntry
            );
        }
        assert_eq!(
            validate_desktop_file(&missing_try_exec).unwrap_err().code,
            LaunchUnavailableCode::MissingExecutable
        );

        std::fs::remove_dir_all(root).unwrap();
    }
}
