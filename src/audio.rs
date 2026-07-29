use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::process::Command;

use super::{
    PIPEWIRE_ACTIVE_TOTAL_US_THRESHOLD, PIPEWIRE_ACTIVE_US_THRESHOLD, command_basename,
    normalize_app_match_key, push_normalized_key_variants,
};
use crate::models::{AppInfo, PactlSinkInput, WindowAudioCache, WindowInfo};

pub(crate) fn sink_input_is_actively_rendering(
    sink: &PactlSinkInput,
    active_media_app_keys: &HashSet<String>,
    observed_pipewire_node_ids: &HashSet<u32>,
    active_pipewire_node_ids: &HashSet<u32>,
    pipewire_activity_cache_valid: bool,
) -> bool {
    if sink_input_is_browser_like(sink) {
        return sink_input_media_keys(sink)
            .iter()
            .any(|key| active_media_app_keys.contains(key));
    }

    // `pw-top` snapshots can miss short-lived activity, so only treat PipeWire
    // as authoritative when this node actually appeared in the sampled output.
    if !pipewire_activity_cache_valid {
        return true;
    }

    let Some(id) = sink
        .properties
        .get("object.id")
        .and_then(|id| id.parse::<u32>().ok())
    else {
        return true;
    };

    if !observed_pipewire_node_ids.contains(&id) {
        return true;
    }

    active_pipewire_node_ids.contains(&id)
}

pub(crate) fn sink_input_level(
    sink: &PactlSinkInput,
    active_media_app_keys: &HashSet<String>,
    observed_pipewire_node_ids: &HashSet<u32>,
    active_pipewire_node_ids: &HashSet<u32>,
    pipewire_activity_cache_valid: bool,
) -> f32 {
    if sink.mute || sink.corked {
        return 0.0;
    }

    if sink
        .properties
        .get("media.category")
        .is_some_and(|category| !category.eq_ignore_ascii_case("Playback"))
    {
        return 0.0;
    }

    if sink.properties.get("media.class").is_some_and(|class| {
        let class = class.to_ascii_lowercase();
        !class.contains("output") && !class.contains("playback")
    }) {
        return 0.0;
    }

    if !sink_input_is_actively_rendering(
        sink,
        active_media_app_keys,
        observed_pipewire_node_ids,
        active_pipewire_node_ids,
        pipewire_activity_cache_valid,
    ) {
        return 0.0;
    }

    let mut total = 0.0;
    let mut count = 0.0;
    for channel in sink.volume.values() {
        if let Ok(percent) = channel.value_percent.trim_end_matches('%').parse::<f32>() {
            total += percent;
            count += 1.0;
        }
    }

    if count == 0.0 {
        return 0.0;
    }

    let level = total / count / 100.0;
    if level < 0.01 {
        0.0
    } else {
        level.clamp(0.0, 1.5)
    }
}

pub(crate) fn active_audio_level_for_sinks(
    sinks: &[PactlSinkInput],
    active_media_app_keys: &HashSet<String>,
    observed_pipewire_node_ids: &HashSet<u32>,
    active_pipewire_node_ids: &HashSet<u32>,
    pipewire_activity_cache_valid: bool,
) -> Option<f32> {
    let mut max_level = 0.0f32;
    for sink in sinks {
        max_level = max_level.max(sink_input_level(
            sink,
            active_media_app_keys,
            observed_pipewire_node_ids,
            active_pipewire_node_ids,
            pipewire_activity_cache_valid,
        ));
    }
    (max_level > 0.0).then_some(max_level)
}

pub(crate) fn app_audio_level(
    app: &AppInfo,
    sink_inputs: &[PactlSinkInput],
    active_media_app_keys: &HashSet<String>,
    observed_pipewire_node_ids: &HashSet<u32>,
    active_pipewire_node_ids: &HashSet<u32>,
    pipewire_activity_cache_valid: bool,
) -> Option<f32> {
    let stem = app
        .desktop_file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(normalize_app_match_key);
    let exec_name = command_basename(&app.exec).map(|name| normalize_app_match_key(&name));
    let app_name = normalize_app_match_key(&app.name);

    let mut matches = Vec::new();
    for sink in sink_inputs {
        if sink_input_level(
            sink,
            active_media_app_keys,
            observed_pipewire_node_ids,
            active_pipewire_node_ids,
            pipewire_activity_cache_valid,
        ) <= 0.0
        {
            continue;
        }

        let candidates = [
            sink.properties.get("application.id"),
            sink.properties.get("application.name"),
            sink.properties.get("application.icon_name"),
            sink.properties.get("application.process.binary"),
        ];

        let matched = candidates.iter().flatten().any(|value| {
            let normalized = normalize_app_match_key(value);
            !normalized.is_empty()
                && (normalized == app_name
                    || stem.as_ref().is_some_and(|stem| normalized == *stem)
                    || exec_name
                        .as_ref()
                        .is_some_and(|exec_name| normalized == *exec_name))
        });

        if matched {
            matches.push(sink.clone());
        }
    }

    active_audio_level_for_sinks(
        &matches,
        active_media_app_keys,
        observed_pipewire_node_ids,
        active_pipewire_node_ids,
        pipewire_activity_cache_valid,
    )
}

pub(crate) fn quantize_audio_level(level: f32) -> u8 {
    (level.clamp(0.0, 1.0) * 100.0).round() as u8
}

pub(crate) fn sink_match_signature(cache: &WindowAudioCache) -> HashMap<String, Vec<u32>> {
    cache
        .sink_matches
        .iter()
        .map(|(window_id, sinks)| {
            (
                window_id.clone(),
                sinks.iter().map(|sink| sink.index).collect::<Vec<_>>(),
            )
        })
        .collect()
}

pub(crate) fn fetch_sink_inputs() -> Vec<PactlSinkInput> {
    let output = Command::new("pactl")
        .args(["--format=json", "list", "sink-inputs"])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            serde_json::from_slice::<Vec<PactlSinkInput>>(&out.stdout).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

pub(crate) fn sink_input_media_keys(sink: &PactlSinkInput) -> HashSet<String> {
    [
        "application.id",
        "application.name",
        "application.icon_name",
        "application.process.binary",
        "node.name",
    ]
    .iter()
    .filter_map(|key| sink.properties.get(*key))
    .map(|value| normalize_app_match_key(value))
    .filter(|value| !value.is_empty())
    .collect()
}

pub(crate) fn sink_input_is_browser_like(sink: &PactlSinkInput) -> bool {
    sink_input_media_keys(sink).iter().any(|key| {
        matches!(
            key.as_str(),
            "firefox"
                | "librewolf"
                | "floorp"
                | "zen"
                | "googlechrome"
                | "chrome"
                | "chromium"
                | "brave"
                | "bravebrowser"
                | "microsoftedge"
                | "edge"
                | "vivaldi"
        )
    })
}

pub(crate) fn mpris_service_names() -> Vec<String> {
    let output = Command::new("busctl")
        .args(["--user", "list", "--no-legend"])
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .filter(|name| name.starts_with("org.mpris.MediaPlayer2."))
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn busctl_string_property(
    service: &str,
    interface: &str,
    property: &str,
) -> Option<String> {
    let output = Command::new("busctl")
        .args([
            "--user",
            "get-property",
            service,
            "/org/mpris/MediaPlayer2",
            interface,
            property,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_quote = stdout.find('"')?;
    let rest = &stdout[first_quote + 1..];
    let second_quote = rest.find('"')?;
    Some(rest[..second_quote].to_owned())
}

pub(crate) fn fetch_active_media_app_keys() -> HashSet<String> {
    let mut keys = HashSet::new();
    for service in mpris_service_names() {
        let is_playing =
            busctl_string_property(&service, "org.mpris.MediaPlayer2.Player", "PlaybackStatus")
                .is_some_and(|status| status.eq_ignore_ascii_case("Playing"));
        if !is_playing {
            continue;
        }

        if let Some(identity) =
            busctl_string_property(&service, "org.mpris.MediaPlayer2", "Identity")
        {
            push_normalized_key_variants(&mut keys, &identity);
        }

        if let Some(service_suffix) = service.strip_prefix("org.mpris.MediaPlayer2.") {
            push_normalized_key_variants(&mut keys, service_suffix);
            if let Some(base_name) = service_suffix.split(".instance_").next() {
                push_normalized_key_variants(&mut keys, base_name);
            }
        }
    }
    keys
}

pub(crate) fn fetch_pipewire_activity() -> (HashSet<u32>, HashSet<u32>, bool) {
    let output = Command::new("pw-top").args(["-b", "-n", "1"]).output();
    match output {
        Ok(out) if out.status.success() => {
            let mut observed_ids = HashSet::new();
            let mut active_ids = HashSet::new();
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty()
                    || trimmed.starts_with("PipeWire")
                    || trimmed.starts_with("ID ")
                {
                    continue;
                }

                let cols: Vec<&str> = trimmed.split_whitespace().collect();
                if cols.len() < 6 {
                    continue;
                }

                let Some(state) = cols.first().and_then(|value| value.chars().next()) else {
                    continue;
                };
                if !matches!(state, 'R' | 'S' | 'I' | 'C' | 'X') {
                    continue;
                }

                let Some(id) = cols.get(1).and_then(|value| value.parse::<u32>().ok()) else {
                    continue;
                };
                observed_ids.insert(id);

                let wait_us = cols
                    .get(4)
                    .and_then(|value| value.strip_suffix("us"))
                    .and_then(|value| value.parse::<f32>().ok())
                    .unwrap_or(0.0);
                let busy_us = cols
                    .get(5)
                    .and_then(|value| value.strip_suffix("us"))
                    .and_then(|value| value.parse::<f32>().ok())
                    .unwrap_or(0.0);
                let wait_active = wait_us >= PIPEWIRE_ACTIVE_US_THRESHOLD;
                let busy_active = busy_us >= PIPEWIRE_ACTIVE_US_THRESHOLD;
                let total_active = (wait_us + busy_us) >= PIPEWIRE_ACTIVE_TOTAL_US_THRESHOLD;
                let is_active = (wait_active || busy_active) && total_active;

                if is_active {
                    active_ids.insert(id);
                }
            }

            (observed_ids, active_ids, true)
        }
        _ => (HashSet::new(), HashSet::new(), false),
    }
}

pub(crate) fn paint_audio_activity_ring(
    painter: &egui::Painter,
    rect: egui::Rect,
    level: f32,
    time_seconds: f32,
) {
    let strength = level.clamp(0.12, 1.2);
    let center = rect.center();
    let base_radius = rect.width().max(rect.height()) * 0.57;
    let max_bar = (rect.width().max(rect.height()) * 0.18).clamp(4.0, 14.0);
    let bars = 24;

    for i in 0..bars {
        let t = i as f32 / bars as f32;
        let angle = t * std::f32::consts::TAU;
        let wave_a = ((time_seconds * 7.5 + t * 13.0).sin() * 0.5 + 0.5).powf(1.4);
        let wave_b = ((time_seconds * 11.0 - t * 19.0).sin() * 0.5 + 0.5) * 0.45;
        let bar_level = (0.25 + wave_a * 0.75 + wave_b).clamp(0.0, 1.0) * strength;
        let inner = base_radius + 1.0;
        let outer = inner + max_bar * bar_level;
        let dir = egui::vec2(angle.cos(), angle.sin());
        let alpha = (70.0 + 135.0 * bar_level).clamp(45.0, 210.0) as u8;
        let color = if i % 3 == 0 {
            egui::Color32::from_rgba_unmultiplied(126, 226, 255, alpha)
        } else {
            egui::Color32::from_rgba_unmultiplied(61, 174, 233, alpha)
        };

        painter.line_segment(
            [center + dir * inner, center + dir * outer],
            egui::Stroke::new((1.2 + 1.7 * bar_level).clamp(1.2, 3.0), color),
        );
    }
}
pub(crate) fn set_sink_input_volume(index: u32, volume_percent: u32) {
    let _ = Command::new("pactl")
        .args(&[
            "set-sink-input-volume",
            &index.to_string(),
            &format!("{}%", volume_percent),
        ])
        .status();
}

pub(crate) fn set_sink_input_mute(index: u32, mute: bool) {
    let _ = Command::new("pactl")
        .args(&[
            "set-sink-input-mute",
            &index.to_string(),
            if mute { "1" } else { "0" },
        ])
        .status();
}

pub(crate) fn sink_display_volume_percent(sink: &PactlSinkInput) -> u32 {
    sink.volume
        .values()
        .next()
        .and_then(|chan| chan.value_percent.trim_end_matches('%').parse::<u32>().ok())
        .unwrap_or(100)
}

pub(crate) fn dedup_sink_inputs_for_controls(
    sink_inputs: &[PactlSinkInput],
) -> Vec<PactlSinkInput> {
    let mut deduped = Vec::new();
    let mut seen_process_ids = HashSet::new();

    for sink in sink_inputs {
        if let Some(process_id) = sink.properties.get("application.process.id") {
            if seen_process_ids.insert(process_id.clone()) {
                deduped.push(sink.clone());
            }
            continue;
        }
        deduped.push(sink.clone());
    }

    deduped
}

pub(crate) fn find_sink_inputs_for_window(
    window: &WindowInfo,
    sink_inputs: &[PactlSinkInput],
) -> Vec<PactlSinkInput> {
    let mut matches = Vec::new();

    // 1. Try to match by PID
    if let Some(wpid) = window.pid {
        let wpid_str = wpid.to_string();
        for sink in sink_inputs {
            if let Some(pid_val) = sink.properties.get("application.process.id") {
                if pid_val == &wpid_str {
                    matches.push(sink.clone());
                }
            }
        }
    }

    // 2. Try to match by process chain PIDs
    if matches.is_empty() {
        for entry in &window.process_chain {
            let pid_str = entry.pid.to_string();
            for sink in sink_inputs {
                if let Some(pid_val) = sink.properties.get("application.process.id") {
                    if pid_val == &pid_str {
                        matches.push(sink.clone());
                    }
                }
            }
        }
    }

    // 3. Try to match by class or active process name
    if matches.is_empty() {
        let class_lower = window.class.to_lowercase();
        let active_lower = window.active_process.as_ref().map(|s| s.to_lowercase());
        for sink in sink_inputs {
            let app_name = sink
                .properties
                .get("application.name")
                .map(|s| s.to_lowercase());
            let app_binary = sink
                .properties
                .get("application.process.binary")
                .map(|s| s.to_lowercase());

            let name_match = app_name.as_ref().map_or(false, |n| {
                n.contains(&class_lower)
                    || class_lower.contains(n)
                    || active_lower
                        .as_ref()
                        .map_or(false, |act| n.contains(act) || act.contains(n))
            });
            let binary_match = app_binary.as_ref().map_or(false, |b| {
                b.contains(&class_lower)
                    || class_lower.contains(b)
                    || active_lower
                        .as_ref()
                        .map_or(false, |act| b.contains(act) || act.contains(b))
            });

            if name_match || binary_match {
                matches.push(sink.clone());
            }
        }
    }

    matches
}
