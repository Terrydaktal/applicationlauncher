use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::time::Duration;

use super::{command_basename, normalize_app_match_key};
use crate::models::{AppInfo, AudioVisualization, PactlSinkInput, WindowAudioCache, WindowInfo};

mod analysis;
mod firefox;
mod monitor;
pub(crate) use firefox::{
    FIREFOX_WORKER_TIMEOUT, FirefoxAudioInbox, FirefoxAudioProcess, apply_firefox_attribution,
    start_firefox_audio_bridge,
};
pub(crate) use monitor::{AudioInbox, STALE_AUDIO_UPDATE, start_audio_monitor};

pub(crate) fn sink_input_can_visualize(sink: &PactlSinkInput) -> bool {
    if sink.mute || sink.corked {
        return false;
    }
    if sink
        .properties
        .get("media.category")
        .is_some_and(|category| !category.eq_ignore_ascii_case("Playback"))
    {
        return false;
    }
    if sink.properties.get("media.class").is_some_and(|class| {
        let class = class.to_ascii_lowercase();
        !class.contains("output") && !class.contains("playback")
    }) {
        return false;
    }
    // Volume is only a mute gate, never the measured level or animation source.
    sink.volume.is_empty()
        || sink.volume.values().any(|channel| {
            channel
                .value_percent
                .trim_end_matches('%')
                .parse::<f32>()
                .is_ok_and(|level| level > 0.0)
        })
}

pub(crate) fn app_audio_sink_indices(app: &AppInfo, sink_inputs: &[PactlSinkInput]) -> Vec<u32> {
    let stem = app
        .desktop_file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(normalize_app_match_key);
    let exec_name = command_basename(&app.exec).map(|name| normalize_app_match_key(&name));
    let app_name = normalize_app_match_key(&app.name);
    sink_inputs
        .iter()
        .filter(|sink| {
            [
                "application.id",
                "application.name",
                "application.icon_name",
                "application.process.binary",
            ]
            .iter()
            .filter_map(|key| sink.properties.get(*key))
            .any(|value| {
                let normalized = normalize_app_match_key(value);
                !normalized.is_empty()
                    && (normalized == app_name
                        || stem.as_ref().is_some_and(|stem| normalized == *stem)
                        || exec_name.as_ref().is_some_and(|exec| normalized == *exec))
            })
        })
        .map(|sink| sink.index)
        .collect()
}

pub(crate) fn visualization_for_sinks(
    indices: &[u32],
    measured: &HashMap<u32, AudioVisualization>,
) -> Option<AudioVisualization> {
    let mut result = AudioVisualization::default();
    for visual in indices.iter().filter_map(|index| measured.get(index)) {
        result.peak = result.peak.max(visual.peak);
        for (target, value) in result.bands.iter_mut().zip(visual.bands) {
            *target = (*target).max(value);
        }
    }
    (result.peak > 0).then_some(result)
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

#[derive(Default)]
struct SinkWindowAttribution<'a> {
    strength: u8,
    candidates: usize,
    window_id: Option<&'a str>,
}

pub(crate) fn build_window_audio_cache(
    windows: &[WindowInfo],
    sink_inputs: &[PactlSinkInput],
) -> WindowAudioCache {
    let mut cache = WindowAudioCache::default();
    let mut owners: HashMap<u32, SinkWindowAttribution<'_>> = HashMap::new();
    for window in windows {
        let matches = find_sink_inputs_for_window(window, sink_inputs);
        for sink in &matches {
            let pid = sink
                .properties
                .get("application.process.id")
                .and_then(|pid| pid.parse::<i32>().ok())
                .filter(|pid| *pid > 0);
            let strength = if pid.is_some() && window.pid == pid {
                3
            } else if pid
                .is_some_and(|pid| window.process_chain.iter().any(|entry| entry.pid == pid))
            {
                2
            } else {
                1
            };
            let owner = owners.entry(sink.index).or_default();
            if strength < owner.strength {
                continue;
            }
            if strength > owner.strength {
                *owner = SinkWindowAttribution {
                    strength,
                    ..Default::default()
                };
            }
            if sink_window_identity_matches(sink, window)
                && owner.window_id != Some(window.id.as_str())
            {
                owner.candidates += 1;
                owner.window_id = Some(&window.id);
            }
        }

        // Controls operate on the whole process, even when it owns several windows.
        if !matches.is_empty() {
            cache
                .sink_matches
                .insert(window.id.clone(), dedup_sink_inputs_for_controls(&matches));
        }
    }
    for sink in sink_inputs {
        // A PID or browser name is not per-window evidence when it has multiple owners.
        let Some(SinkWindowAttribution {
            candidates: 1,
            window_id: Some(window_id),
            ..
        }) = owners.get(&sink.index)
        else {
            continue;
        };
        if sink_input_can_visualize(sink) {
            cache
                .visualization_sinks
                .entry((*window_id).to_owned())
                .or_default()
                .push(sink.index);
        }
    }
    cache
}

fn sink_window_identity_matches(sink: &PactlSinkInput, window: &WindowInfo) -> bool {
    let property = |key| {
        sink.properties
            .get(key)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
    };
    if property("window.id").is_some_and(|id| id != window.id) {
        return false;
    }
    let title = if window.raw_title.trim().is_empty() {
        &window.title
    } else {
        &window.raw_title
    };
    // media.name is a track/tab title, not a window identity. In particular, it
    // cannot locate a background tab, and browsers retain inactive named streams.
    property("window.name").is_none_or(|name| name == title.trim())
}

pub(crate) fn paint_audio_activity_ring(
    painter: &egui::Painter,
    rect: egui::Rect,
    visual: AudioVisualization,
) {
    let center = rect.center();
    let diameter = rect.width().max(rect.height());
    // Keep the existing outer footprint, using the old empty gap for more travel.
    let outer_radius = diameter * 0.57 + 1.0 + (diameter * 0.18).clamp(4.0, 14.0);
    let inner_radius = diameter * 0.50 + 0.5;
    let max_bar = outer_radius - inner_radius;
    let bars = 24;

    for i in 0..bars {
        let t = i as f32 / bars as f32;
        let angle = t * std::f32::consts::TAU;
        let band = i * visual.bands.len() / bars;
        let next = (band + 1) % visual.bands.len();
        let fraction = (i * visual.bands.len() % bars) as f32 / bars as f32;
        let bar_level = ((1.0 - fraction) * f32::from(visual.bands[band])
            + fraction * f32::from(visual.bands[next]))
            / 100.0;
        if bar_level <= 0.0 {
            continue;
        }
        let outer = inner_radius + max_bar * bar_level;
        let dir = egui::vec2(angle.cos(), angle.sin());
        let color = egui::Color32::from_rgba_unmultiplied(
            (61.0 + 100.0 * bar_level) as u8,
            (174.0 + 65.0 * bar_level) as u8,
            255,
            (40.0 + 215.0 * bar_level) as u8,
        );

        painter.line_segment(
            [center + dir * inner_radius, center + dir * outer],
            egui::Stroke::new((1.2 + 1.7 * bar_level).clamp(1.2, 3.0), color),
        );
    }
}
pub(crate) fn set_sink_input_volume(index: u32, volume_percent: u32) {
    std::thread::spawn(move || {
        let mut command = Command::new("pactl");
        command.args([
            "set-sink-input-volume",
            &index.to_string(),
            &format!("{}%", volume_percent),
        ]);
        let _ = applicationlauncher::process::status_with_timeout(command, Duration::from_secs(2));
    });
}

pub(crate) fn set_sink_input_mute(index: u32, mute: bool) {
    std::thread::spawn(move || {
        let mut command = Command::new("pactl");
        command.args([
            "set-sink-input-mute",
            &index.to_string(),
            if mute { "1" } else { "0" },
        ]);
        let _ = applicationlauncher::process::status_with_timeout(command, Duration::from_secs(2));
    });
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
        if class_lower.trim().is_empty()
            && active_lower
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
        {
            return matches;
        }
        for sink in sink_inputs {
            let app_name = sink
                .properties
                .get("application.name")
                .map(|s| s.to_lowercase());
            let app_binary = sink
                .properties
                .get("application.process.binary")
                .map(|s| s.to_lowercase());

            let name_match = app_name.as_ref().is_some_and(|n| {
                (!class_lower.is_empty() && (n.contains(&class_lower) || class_lower.contains(n)))
                    || active_lower
                        .as_ref()
                        .is_some_and(|act| !act.is_empty() && (n.contains(act) || act.contains(n)))
            });
            let binary_match = app_binary.as_ref().is_some_and(|b| {
                (!class_lower.is_empty() && (b.contains(&class_lower) || class_lower.contains(b)))
                    || active_lower
                        .as_ref()
                        .is_some_and(|act| !act.is_empty() && (b.contains(act) || act.contains(b)))
            });

            if name_match || binary_match {
                matches.push(sink.clone());
            }
        }
    }

    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn painted_bars(size: f32, level: u8) -> Vec<([egui::Pos2; 2], egui::Stroke)> {
        let ctx = egui::Context::default();
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            paint_audio_activity_ring(
                &ctx.layer_painter(egui::LayerId::background()),
                egui::Rect::from_center_size(egui::pos2(100.0, 100.0), egui::vec2(size, size)),
                AudioVisualization {
                    peak: level,
                    bands: [level; 8],
                },
            );
        });
        output
            .shapes
            .into_iter()
            .filter_map(|shape| match shape.shape {
                egui::Shape::LineSegment { points, stroke } => Some((points, stroke)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn bars_have_more_travel_without_growing_the_outer_icon_footprint() {
        for size in [12.0_f32, 16.0, 32.0, 64.0, 128.0] {
            let quiet = painted_bars(size, 40);
            let loud = painted_bars(size, 100);
            assert_eq!(quiet.len(), 24);
            assert_eq!(loud.len(), 24);
            let center = egui::pos2(100.0, 100.0);
            let outer_limit = size * 0.57 + 1.0 + (size * 0.18).clamp(4.0, 14.0);
            for ((quiet, quiet_stroke), (loud, loud_stroke)) in quiet.iter().zip(&loud) {
                assert_eq!(quiet[0], loud[0]);
                assert!((loud[1].distance(center) - outer_limit).abs() < 0.001);
                assert!(loud_stroke.color.a() > quiet_stroke.color.a());
                if size == 32.0 {
                    assert!(
                        loud[1].distance(quiet[1]) > 4.0,
                        "beat still moves less than a few pixels"
                    );
                }
            }
        }
    }

    #[test]
    fn silent_visuals_paint_no_bars() {
        assert!(painted_bars(32.0, 0).is_empty());
    }

    pub(super) fn firefox_window(id: &str, pid: i32, title: &str) -> WindowInfo {
        WindowInfo {
            id: id.into(),
            title: title.into(),
            raw_title: title.into(),
            class: "firefox".into(),
            desktop_file_name: Some("firefox".into()),
            minimized: Some(false),
            demands_attention: false,
            icon_path: None,
            active_process: None,
            exe_path: None,
            cwd_path: None,
            command_line: None,
            command_summary: None,
            geometry: None,
            process_chain: Vec::new(),
            pid: Some(pid),
            last_activated_at_ms: None,
            activation_sequence: 0,
        }
    }

    pub(super) fn firefox_sink(index: u32, pid: i32) -> PactlSinkInput {
        serde_json::from_value(serde_json::json!({
            "index": index,
            "volume": {"front-left": {"value_percent": "75%"}},
            "properties": {
                "application.process.id": pid.to_string(),
                "application.name": "Firefox",
                "application.process.binary": "firefox",
                "media.name": "Music - YouTube",
                "media.class": "Stream/Output/Audio"
            }
        }))
        .unwrap()
    }

    struct MeasuredCache {
        level_buckets: HashMap<String, u8>,
        sink_matches: HashMap<String, Vec<PactlSinkInput>>,
    }

    fn measured_cache(
        windows: &[WindowInfo],
        sinks: &[PactlSinkInput],
        measured: &HashMap<u32, AudioVisualization>,
    ) -> MeasuredCache {
        let routing = build_window_audio_cache(windows, sinks);
        MeasuredCache {
            level_buckets: routing
                .visualization_sinks
                .iter()
                .filter_map(|(window, indices)| {
                    visualization_for_sinks(indices, measured)
                        .map(|visual| (window.clone(), visual.peak))
                })
                .collect(),
            sink_matches: routing.sink_matches,
        }
    }

    fn playing_cache(windows: &[WindowInfo], sinks: &[PactlSinkInput]) -> MeasuredCache {
        let measured = sinks
            .iter()
            .map(|sink| {
                (
                    sink.index,
                    AudioVisualization {
                        peak: 75,
                        bands: [75; 8],
                    },
                )
            })
            .collect();
        measured_cache(windows, sinks, &measured)
    }

    #[test]
    fn shared_firefox_pid_is_not_per_window_playback_evidence() {
        let windows = [
            firefox_window("first", 104545, "First search - Mozilla Firefox"),
            firefox_window("second", 104545, "Second search - Mozilla Firefox"),
        ];
        let sinks: Vec<_> = (1..=8).map(|index| firefox_sink(index, 104545)).collect();
        let cache = playing_cache(&windows, &sinks);
        assert!(
            cache.level_buckets.is_empty(),
            "ambiguous browser audio must stay app-wide"
        );
        assert_eq!(
            cache.sink_matches.len(),
            2,
            "application volume controls remain available"
        );

        let app = AppInfo {
            name: "Firefox".into(),
            exec: "firefox %u".into(),
            icon_path: None,
            comment: None,
            desktop_file_path: "/usr/share/applications/firefox.desktop".into(),
            is_settings_module: false,
        };
        assert_eq!(
            app_audio_sink_indices(&app, &sinks),
            (1..=8).collect::<Vec<_>>()
        );
    }

    #[test]
    fn single_firefox_window_keeps_background_tab_playback() {
        let window = firefox_window("first", 100, "Unrelated search - Mozilla Firefox");
        let cache = playing_cache(&[window], &[firefox_sink(1, 100)]);
        assert_eq!(cache.level_buckets, HashMap::from([("first".into(), 75)]));
    }

    #[test]
    fn direct_pid_does_not_light_up_other_firefox_process() {
        let windows = [
            firefox_window("first", 100, "First search"),
            firefox_window("second", 200, "Second search"),
        ];
        let cache = playing_cache(&windows, &[firefox_sink(1, 100)]);
        assert_eq!(cache.level_buckets, HashMap::from([("first".into(), 75)]));
    }

    #[test]
    fn explicit_window_name_disambiguates_shared_pid_without_guessing_from_track() {
        let windows = [
            firefox_window("first", 100, "Music - YouTube"),
            firefox_window("second", 100, "Other tab"),
        ];
        let mut sink = firefox_sink(1, 100);
        assert!(
            playing_cache(&windows, &[sink.clone()])
                .level_buckets
                .is_empty()
        );

        // A background tab can be playing in the second window even if the
        // first window's visible title happens to equal the media title.
        sink.properties
            .insert("window.name".into(), "Other tab".into());
        assert_eq!(
            playing_cache(&windows, &[sink]).level_buckets,
            HashMap::from([("second".into(), 75)])
        );
    }

    #[test]
    fn explicit_window_id_disambiguates_duplicate_titles() {
        let windows = [
            firefox_window("first", 100, "Same title"),
            firefox_window("second", 100, "Same title"),
        ];
        let mut sink = firefox_sink(1, 100);
        sink.properties
            .insert("window.name".into(), "Same title".into());
        assert!(
            playing_cache(&windows, &[sink.clone()])
                .level_buckets
                .is_empty()
        );
        sink.properties.insert("window.id".into(), "second".into());
        assert_eq!(
            playing_cache(&windows, &[sink]).level_buckets,
            HashMap::from([("second".into(), 75)])
        );
    }

    #[test]
    fn missing_or_conflicting_window_identity_does_not_fall_back_to_pid() {
        let window = firefox_window("first", 100, "Current title");
        let mut sink = firefox_sink(1, 100);
        sink.properties
            .insert("window.name".into(), "Previous title".into());
        assert!(
            playing_cache(std::slice::from_ref(&window), &[sink.clone()])
                .level_buckets
                .is_empty()
        );
        sink.properties
            .insert("window.name".into(), "Current title".into());
        sink.properties
            .insert("window.id".into(), "closed-window".into());
        assert!(playing_cache(&[window], &[sink]).level_buckets.is_empty());
    }

    #[test]
    fn name_fallback_cannot_override_conflicting_direct_pid_metadata() {
        let windows = [
            firefox_window("first", 100, "First title"),
            firefox_window("second", 200, "Second title"),
        ];
        let mut sink = firefox_sink(1, 100);
        sink.properties
            .insert("window.name".into(), "Second title".into());
        assert!(
            playing_cache(&windows, &[sink.clone()])
                .level_buckets
                .is_empty()
        );
        let reversed: Vec<_> = windows.into_iter().rev().collect();
        assert!(playing_cache(&reversed, &[sink]).level_buckets.is_empty());
    }

    #[test]
    fn direct_pid_takes_precedence_over_process_chain_candidates() {
        let browser = firefox_window("browser", 100, "Music");
        let mut terminal = firefox_window("terminal", 200, "Terminal");
        terminal.class = "xfce4-terminal".into();
        terminal
            .process_chain
            .push(crate::models::ProcessChainEntry {
                pid: 100,
                name: "firefox".into(),
                exe_path: None,
            });
        let windows = [terminal, browser];
        let sink = firefox_sink(1, 100);
        assert_eq!(
            playing_cache(&windows, &[sink.clone()]).level_buckets,
            HashMap::from([("browser".into(), 75)])
        );
        let reversed: Vec<_> = windows.into_iter().rev().collect();
        assert_eq!(
            playing_cache(&reversed, &[sink]).level_buckets,
            HashMap::from([("browser".into(), 75)])
        );
    }

    #[test]
    fn unique_process_chain_keeps_terminal_player_activity() {
        let mut terminal = firefox_window("terminal", 200, "Player - Terminal");
        terminal.class = "xfce4-terminal".into();
        terminal
            .process_chain
            .push(crate::models::ProcessChainEntry {
                pid: 100,
                name: "player".into(),
                exe_path: None,
            });
        let mut sink = firefox_sink(1, 100);
        sink.properties
            .insert("application.name".into(), "Player".into());
        sink.properties
            .insert("application.process.binary".into(), "player".into());
        assert_eq!(
            playing_cache(&[terminal], &[sink]).level_buckets,
            HashMap::from([("terminal".into(), 75)])
        );
    }

    #[test]
    fn ambiguous_application_name_alone_does_not_identify_a_window() {
        let windows = [
            firefox_window("first", 100, "First title"),
            firefox_window("second", 200, "Second title"),
        ];
        let mut sink = firefox_sink(1, 300);
        sink.properties.remove("application.process.id");
        assert!(playing_cache(&windows, &[sink]).level_buckets.is_empty());
    }

    #[test]
    fn ownership_is_recomputed_when_windows_open_and_close() {
        let mut windows = vec![firefox_window("first", 100, "First title")];
        let sinks = [firefox_sink(1, 100)];
        assert_eq!(playing_cache(&windows, &sinks).level_buckets.len(), 1);
        windows.push(firefox_window("second", 100, "Second title"));
        assert!(playing_cache(&windows, &sinks).level_buckets.is_empty());
        windows.remove(0);
        assert_eq!(
            playing_cache(&windows, &sinks).level_buckets,
            HashMap::from([("second".into(), 75)])
        );
    }

    #[test]
    fn multiple_streams_keep_the_max_level_and_one_volume_control() {
        let window = firefox_window("first", 100, "Music");
        let quiet = firefox_sink(1, 100);
        let loud = firefox_sink(2, 100);
        let cache = measured_cache(
            &[window],
            &[quiet, loud],
            &HashMap::from([
                (
                    1,
                    AudioVisualization {
                        peak: 15,
                        bands: [15; 8],
                    },
                ),
                (
                    2,
                    AudioVisualization {
                        peak: 90,
                        bands: [90; 8],
                    },
                ),
            ]),
        );
        assert_eq!(cache.level_buckets, HashMap::from([("first".into(), 90)]));
        assert_eq!(cache.sink_matches["first"].len(), 1);
    }

    #[test]
    fn mute_cork_capture_and_paused_browser_never_get_waveforms() {
        let windows = [firefox_window("first", 100, "Music")];
        let mut sink = firefox_sink(1, 100);
        sink.mute = true;
        assert!(
            playing_cache(&windows, &[sink.clone()])
                .level_buckets
                .is_empty()
        );
        sink.mute = false;
        sink.corked = true;
        assert!(
            playing_cache(&windows, &[sink.clone()])
                .level_buckets
                .is_empty()
        );
        sink.corked = false;
        sink.properties
            .insert("media.category".into(), "Capture".into());
        assert!(
            playing_cache(&windows, &[sink.clone()])
                .level_buckets
                .is_empty()
        );
        sink.properties.remove("media.category");
        let cache = measured_cache(&windows, &[sink], &HashMap::new());
        assert!(cache.level_buckets.is_empty());
        assert_eq!(cache.sink_matches.len(), 1);
    }

    #[test]
    fn configured_volume_is_not_playback_and_one_stream_cannot_light_another() {
        let windows = [
            firefox_window("first", 100, "First"),
            firefox_window("second", 200, "Second"),
        ];
        let sinks = [firefox_sink(1, 100), firefox_sink(2, 200)];
        assert!(
            measured_cache(&windows, &sinks, &HashMap::new())
                .level_buckets
                .is_empty()
        );
        let measured = HashMap::from([(
            2,
            AudioVisualization {
                peak: 37,
                bands: [37; 8],
            },
        )]);
        assert_eq!(
            measured_cache(&windows, &sinks, &measured).level_buckets,
            HashMap::from([("second".into(), 37)])
        );
    }
}
