use super::*;
#[cfg(test)]
mod tests {
    use super::*;
    use fuzzy_rank::fields::fuzzy::{MetadataCandidate, MetadataQuery, SearchField};

    #[test]
    fn optimization_inventory_references_all_twelve_steps_and_existing_regressions() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let inventory: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/optimization-contracts.json")).unwrap();
        let steps = inventory["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 12);
        for (index, step) in steps.iter().enumerate() {
            assert_eq!(
                step["id"].as_str().unwrap(),
                format!("OPT-{:02}", index + 1)
            );
            assert!(root.join(step["owner"].as_str().unwrap()).is_file());
            assert!(!step["contract"].as_str().unwrap().is_empty());
            assert!(!step["bound"].as_str().unwrap().is_empty());
            let tests = step["tests"].as_array().unwrap();
            assert!(!tests.is_empty());
            for test in tests {
                let source =
                    std::fs::read_to_string(root.join(test["path"].as_str().unwrap())).unwrap();
                let cases = test["cases"].as_array().unwrap();
                assert!(!cases.is_empty());
                for case in cases {
                    assert!(source.contains(&format!("fn {}(", case.as_str().unwrap())));
                }
            }
        }
    }

    #[test]
    fn default_window_cache_reuses_rows_and_refreshes_transient_titles() {
        let mut windows = vec![test_window_info("codex - \u{280b} /project - Terminal")];
        let mut cache = helpers::DefaultWindowsCache::default();
        let first = cache.get(&windows, false);
        for _ in 0..100 {
            assert!(Arc::ptr_eq(&first, &cache.get(&windows, false)));
        }
        drop(first);
        let old = windows[0].clone();
        windows[0].title = "codex - \u{2819} /project - Terminal".into();
        cache.update(&[(old, windows[0].clone())]);
        assert_eq!(cache.get(&windows, false)[0].title, windows[0].title);
        windows.push(test_window_info("codex - /other - Terminal"));
        cache.invalidate();
        assert_eq!(cache.get(&windows, false).len(), 2);
    }

    #[test]
    fn cached_window_order_keeps_application_and_terminal_subgroup_rules() {
        let make = |id: &str, class: &str, program: &str, title: &str| {
            let mut window = test_window_info(title);
            window.id = id.into();
            window.class = class.into();
            window.active_process = Some(program.into());
            window.command_summary = Some(program.into());
            window
        };
        let windows = vec![
            make(
                "c2",
                "xfce4-terminal",
                "codex",
                "codex - \u{2819} z - Terminal",
            ),
            make("f", "xfce4-terminal", "fish", "fish - a - Terminal"),
            make("browser", "firefox", "firefox", "Browser"),
            make(
                "c1",
                "xfce4-terminal",
                "codex",
                "codex - \u{280b} a - Terminal",
            ),
        ];
        let mut cache = helpers::DefaultWindowsCache::default();
        assert_eq!(
            cache
                .get(&windows, false)
                .iter()
                .map(|w| w.id.as_str())
                .collect::<Vec<_>>(),
            ["browser", "f", "c1", "c2"]
        );
        let mut changed = windows.clone();
        changed[0].last_activated_at_ms = Some(999);
        cache.update(&[(windows[0].clone(), changed[0].clone())]);
        assert_eq!(cache.get(&changed, true)[0].id, "c2");
    }

    fn console_window(id: &str, program: &str, session: Option<&str>) -> WindowInfo {
        let mut window = test_window_info(&format!("{program} - {id} - Terminal"));
        window.id = id.into();
        window.active_process = Some(program.into());
        window.command_summary = Some(program.into());
        if let Some(session) = session {
            window.title = format!("tmux: {session} - {program}: /project - Terminal");
            window.tmux_pane = Some(applicationlauncher::tracker::LiveTmuxPane {
                client_pid: 123,
                session: applicationlauncher::tracker::TmuxSession {
                    socket_path: "/tmp/test-tmux.sock".into(),
                    session_name: session.into(),
                    ..Default::default()
                },
                pane_id: "%0".into(),
                pane_pid: 4320,
                process_pid: 4321,
                process_name: program.into(),
                cwd: "/project".into(),
                title: program.into(),
            });
        }
        window
    }

    #[test]
    fn console_application_groups_put_tmux_windows_before_regular_windows() {
        let windows = vec![
            console_window("codex-regular-z", "codex", None),
            console_window("htop-regular", "htop", None),
            console_window("codex-tmux-z", "codex", Some("z")),
            console_window("fish", "fish", None),
            console_window("codex-regular-a", "codex", None),
            console_window("codex-tmux-a", "codex", Some("a")),
            console_window("htop-tmux", "htop", Some("monitor")),
        ];
        let mut cache = helpers::DefaultWindowsCache::default();
        let rows = cache.get(&windows, false);
        assert_eq!(
            rows.iter()
                .map(|window| window.id.as_str())
                .collect::<Vec<_>>(),
            [
                "fish",
                "htop-tmux",
                "htop-regular",
                "codex-tmux-a",
                "codex-tmux-z",
                "codex-regular-a",
                "codex-regular-z"
            ]
        );
        assert!(Arc::ptr_eq(&rows, &cache.get(&windows, false)));
    }

    #[test]
    fn console_tmux_attachment_changes_invalidate_cached_window_order() {
        let mut windows = vec![
            console_window("a", "codex", None),
            console_window("z", "codex", None),
        ];
        let mut cache = helpers::DefaultWindowsCache::default();
        assert_eq!(cache.get(&windows, false)[0].id, "a");
        let old = windows[1].clone();
        windows[1].tmux_pane = console_window("z", "codex", Some("work")).tmux_pane;
        cache.update(&[(old, windows[1].clone())]);
        assert_eq!(cache.get(&windows, false)[0].id, "z");
        let old = windows[1].clone();
        windows[1].tmux_pane = None;
        cache.update(&[(old, windows[1].clone())]);
        assert_eq!(cache.get(&windows, false)[0].id, "a");
    }

    #[test]
    fn console_tmux_priority_does_not_override_recent_first_ordering() {
        let mut regular = console_window("regular", "codex", None);
        regular.last_activated_at_ms = Some(200);
        let mut tmux = console_window("tmux", "codex", Some("work"));
        tmux.last_activated_at_ms = Some(100);
        let windows = vec![tmux, regular];
        let mut cache = helpers::DefaultWindowsCache::default();
        assert_eq!(cache.get(&windows, true)[0].id, "regular");
        assert_eq!(cache.get(&windows, false)[0].id, "tmux");
    }

    #[test]
    fn incremental_documents_prepare_only_changed_entries_and_drop_removed_entries() {
        let mut windows = vec![test_window_info("first"), test_window_info("second")];
        windows[0].id = "first".into();
        windows[1].id = "second".into();
        let mut documents = Vec::new();
        let mut dirty = HashSet::new();
        assert_eq!(
            crate::search::update_window_search_documents(&windows, &mut documents, &mut dirty),
            2
        );
        let first_allocation = documents[0].values.as_ptr();
        windows[1].title = "changed".into();
        dirty.insert("second".into());
        assert_eq!(
            crate::search::update_window_search_documents(&windows, &mut documents, &mut dirty),
            1
        );
        assert_eq!(documents[0].values.as_ptr(), first_allocation);
        assert_eq!(documents[1].values[0].1, "changed");
        assert_eq!(
            crate::search::update_window_search_documents(&windows, &mut documents, &mut dirty),
            0
        );
        windows.remove(0);
        assert_eq!(
            crate::search::update_window_search_documents(&windows, &mut documents, &mut dirty),
            0
        );
        assert_eq!(documents.len(), 1);
        assert!(documents[0].key.ends_with("second"));
    }

    #[test]
    fn unicode_highlights_keep_all_occurrences_and_exact_overlap_precedence() {
        use fuzzy_rank::fields::fuzzy::MatchedFieldHighlight;
        let title = "\u{280b} caf\u{e9} | Whisper | WHISPER | \u{3bb}";
        let fields = [SearchField {
            priority: 0,
            value: "whisper",
        }];
        let highlights = [
            MatchedFieldHighlight {
                field_index: 0,
                ranges: vec![(0, 7, false)],
                focus_range: Some((0, 7)),
            },
            MatchedFieldHighlight {
                field_index: 0,
                ranges: vec![(0, 4, true)],
                focus_range: Some((0, 4)),
            },
            MatchedFieldHighlight {
                field_index: 9,
                ranges: vec![(0, usize::MAX, false)],
                focus_range: None,
            },
        ];
        let (ranges, focus) =
            crate::search::map_field_highlights_to_full_text(title, &fields, &highlights);
        assert_eq!(
            ranges
                .iter()
                .map(|(s, e, exact)| (&title[*s..*e], *exact))
                .collect::<Vec<_>>(),
            [
                ("Whis", true),
                ("per", false),
                ("WHIS", true),
                ("PER", false)
            ]
        );
        let first = title.find("Whisper").unwrap();
        assert_eq!(focus, Some((first, first + 4)));
    }

    #[test]
    fn virtual_rows_and_grids_render_only_viewport_and_preserve_geometry() {
        for (columns, tile, gap) in [(1, 20.0, 0.0), (4, 48.0, 10.0)] {
            let ctx = egui::Context::default();
            let mut rendered = 0;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(260.0, 230.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                        let area = egui::ScrollArea::vertical()
                            .max_height(200.0)
                            .vertical_scroll_offset(500.0)
                            .show_viewport(ui, |ui, viewport| {
                                let (range, centers) = helpers::virtual_items(
                                    ui, viewport, 400, columns, tile, gap, None,
                                );
                                assert!(range.start > 0);
                                assert!(range.len() < 60);
                                assert_eq!(centers.len(), 400);
                                assert!((centers[columns] - centers[0] - tile - gap).abs() < 0.01);
                                let mut render = |ui: &mut egui::Ui| {
                                    for index in range.clone() {
                                        helpers::virtual_item(
                                            ui,
                                            index,
                                            egui::vec2(
                                                if columns == 1 {
                                                    ui.available_width()
                                                } else {
                                                    tile
                                                },
                                                tile,
                                            ),
                                            |ui| {
                                                let (rect, _) = ui.allocate_exact_size(
                                                    egui::vec2(
                                                        if columns == 1 {
                                                            ui.available_width()
                                                        } else {
                                                            tile
                                                        },
                                                        tile,
                                                    ),
                                                    egui::Sense::click(),
                                                );
                                                assert!(
                                                    (rect.center().y - centers[index]).abs() < 0.1,
                                                    "wrong position for {index}: {rect:?}, {}",
                                                    centers[index]
                                                );
                                                rendered += 1;
                                            },
                                        );
                                    }
                                };
                                if columns == 1 {
                                    render(ui);
                                } else {
                                    ui.horizontal_wrapped(render);
                                }
                            });
                        assert!(
                            (area.content_size.y
                                - (400_usize.div_ceil(columns) as f32 * (tile + gap) - gap))
                                .abs()
                                < 1.0
                        );
                    });
                },
            );
            assert!(!output.shapes.is_empty());
            assert!(rendered < 60);
        }
    }

    #[test]
    fn virtual_item_ids_remain_stable_when_the_viewport_moves() {
        let ctx = egui::Context::default();
        let mut previous = HashMap::new();
        for offset in [0.0, 40.0, 0.0] {
            let mut ids = HashMap::new();
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(260.0, 230.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        egui::ScrollArea::vertical()
                            .max_height(200.0)
                            .vertical_scroll_offset(offset)
                            .show_viewport(ui, |ui, viewport| {
                                let (range, _) =
                                    helpers::virtual_items(ui, viewport, 400, 1, 20.0, 0.0, None);
                                for index in range {
                                    let response = helpers::virtual_item(
                                        ui,
                                        index,
                                        egui::vec2(ui.available_width(), 20.0),
                                        |ui| {
                                            ui.allocate_response(
                                                egui::vec2(ui.available_width(), 20.0),
                                                egui::Sense::click(),
                                            )
                                        },
                                    );
                                    ids.insert(index, response.id);
                                }
                            });
                    });
                },
            );
            assert!(ids.contains_key(&5));
            if let Some(id) = previous.get(&5) {
                assert_eq!(*id, ids[&5]);
            }
            previous = ids;
        }
    }

    #[test]
    fn virtual_keyboard_selection_reaches_offscreen_and_partial_final_rows() {
        for columns in [1, 4] {
            let ctx = egui::Context::default();
            ctx.style_mut(|style| style.scroll_animation = egui::style::ScrollAnimation::none());
            let mut selection_visible = false;
            for frame in 0..3 {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(260.0, 230.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(200.0)
                                .show_viewport(ui, |ui, viewport| {
                                    let (range, centers) = helpers::virtual_items(
                                        ui,
                                        viewport,
                                        403,
                                        columns,
                                        48.0,
                                        10.0,
                                        (frame == 0).then_some(402),
                                    );
                                    assert_eq!(centers.len(), 403);
                                    selection_visible |= range.contains(&402);
                                });
                        });
                    },
                );
            }
            assert!(
                selection_visible,
                "offscreen selection lost for {columns} columns"
            );
        }
    }

    #[test]
    fn deployment_notices_expire_after_exactly_three_seconds() {
        let now = Instant::now();
        let timer = ForegroundNoticeTimer::new(now);
        assert_eq!(timer.remaining(now), Some(Duration::from_secs(3)));
        assert_eq!(
            timer.remaining(now + Duration::from_millis(2999)),
            Some(Duration::from_millis(1))
        );
        assert_eq!(timer.remaining(now + Duration::from_secs(3)), None);
        assert_eq!(timer.remaining(now + Duration::from_secs(60)), None);
    }

    #[test]
    fn deployment_notice_timer_does_not_restart_on_focused_frames() {
        let now = Instant::now();
        let mut timer = ForegroundNoticeTimer::new(now);
        timer.update(now, true, false);
        timer.update(now + Duration::from_secs(2), true, false);
        assert_eq!(
            timer.remaining(now + Duration::from_secs(2)),
            Some(Duration::from_secs(1))
        );
        timer.update(now + Duration::from_secs(3), true, false);
        assert_eq!(timer.remaining(now + Duration::from_secs(3)), None);
    }

    #[test]
    fn deployment_notices_reappear_on_each_foreground_transition() {
        let now = Instant::now();
        let mut timer = ForegroundNoticeTimer::new(now);
        timer.update(now, true, false);
        for seconds in [10, 60, 3600] {
            let foreground_at = now + Duration::from_secs(seconds);
            timer.update(foreground_at - Duration::from_secs(1), false, false);
            assert_eq!(timer.remaining(foreground_at), None);
            timer.update(foreground_at, true, false);
            assert_eq!(timer.remaining(foreground_at), Some(Duration::from_secs(3)));
            assert_eq!(
                timer.remaining(foreground_at + Duration::from_secs(3)),
                None
            );
        }
    }

    #[test]
    fn deployment_notices_reappear_on_shortcut_relaunch_even_if_already_focused() {
        let now = Instant::now();
        let mut timer = ForegroundNoticeTimer::new(now);
        timer.update(now, true, false);
        let relaunch_at = now + Duration::from_secs(30);
        timer.update(relaunch_at, true, true);
        assert_eq!(timer.remaining(relaunch_at), Some(Duration::from_secs(3)));
        timer.update(relaunch_at + Duration::from_secs(3), true, false);
        assert_eq!(timer.remaining(relaunch_at + Duration::from_secs(3)), None);
    }

    #[test]
    fn deployment_notices_wait_for_actual_focus_after_delayed_foreground_request() {
        let now = Instant::now();
        let mut timer = ForegroundNoticeTimer::new(now);
        timer.update(now, false, true);
        let focused_at = now + Duration::from_secs(10);
        timer.update(focused_at, false, false);
        assert_eq!(timer.remaining(focused_at), None);
        timer.update(focused_at, true, false);
        assert_eq!(timer.remaining(focused_at), Some(Duration::from_secs(3)));
    }

    #[test]
    fn open_window_count_is_small_muted_and_does_not_take_search_focus() {
        let ctx = egui::Context::default();
        for count in [0, 1, 9, 10, 99, 100, 1000] {
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let (label, width) = super::super::view::open_window_count_label(ui, count);
                    let response = ui.add(label);
                    assert!(!response.sense.is_focusable());
                    assert!(!response.sense.senses_click());
                    assert!(!response.sense.senses_drag());
                    assert!((response.rect.width() - width).abs() < 1.0);
                });
            });
            let text = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == count.to_string() => {
                        Some(text)
                    }
                    _ => None,
                })
                .expect("the full window count should be painted");
            let format = &text.galley.job.sections[0].format;
            assert_eq!(format.font_id.size, 12.0);
            assert_eq!(format.color, egui::Color32::from_gray(140));
        }
    }

    #[test]
    fn application_icon_tooltip_keeps_the_full_name_on_one_line_in_narrow_space() {
        for name in ["Tixati", "Telegram Desktop", "Firefox Web Browser"] {
            let ctx = egui::Context::default();
            ctx.style_mut(|style| style.spacing.tooltip_width = 32.0);
            let mut painted = None;
            for frame in 0..4 {
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(500.0, 760.0),
                        )),
                        time: Some(frame as f64 / 60.0),
                        events: vec![egui::Event::PointerMoved(egui::pos2(449.0, 224.0))],
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            let response = ui.interact(
                                egui::Rect::from_min_size(
                                    egui::pos2(425.0, 200.0),
                                    egui::vec2(48.0, 48.0),
                                ),
                                ui.id().with("application_icon"),
                                egui::Sense::hover(),
                            );
                            show_immediate_icon_tooltip(&response, name);
                        });
                    },
                );
                painted = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == name => Some((
                        text.galley.clone(),
                        text.visual_bounding_rect(),
                        shape.clip_rect,
                    )),
                    _ => None,
                });
            }
            let (galley, bounds, clip_rect) =
                painted.expect("hovering an application should paint its full name");
            assert_eq!(galley.rows.len(), 1, "tooltip should not wrap {name:?}");
            assert_eq!(galley.rows[0].glyphs.len(), name.chars().count());
            assert!(
                clip_rect.contains_rect(bounds),
                "tooltip should not clip {name:?}"
            );
            assert!(bounds.min.x >= 0.0 && bounds.max.x <= 500.0);
        }
    }

    #[test]
    fn periodic_snapshot_labels_identify_the_tier_and_capture_time() {
        let mut snapshot = applicationlauncher::tracker::SnapshotSummary {
            id: 1,
            name: None,
            kind: "hourly".into(),
            created_at_ms: 1_672_531_200_000,
            window_count: 50,
            daemon_version: None,
            daemon_build_id: None,
        };
        assert_eq!(snapshot_label(&snapshot), "Hourly 2023-01-01 00:00 UTC");
        snapshot.created_at_ms += 3_600_000;
        assert_eq!(snapshot_label(&snapshot), "Hourly 2023-01-01 01:00 UTC");
        snapshot.kind = "recent".into();
        assert_eq!(snapshot_label(&snapshot), "5-minute 2023-01-01 01:00 UTC");
        snapshot.created_at_ms += 300_000;
        assert_eq!(snapshot_label(&snapshot), "5-minute 2023-01-01 01:05 UTC");
        snapshot.kind = "named".into();
        snapshot.name = Some("Before reconstruction".into());
        assert_eq!(snapshot_label(&snapshot), "Before reconstruction");
        assert_eq!(format_activation_time(None), "Not recorded yet");
        assert!(
            format_activation_time(Some(1_672_531_200_123))
                .starts_with("2023-01-01 00:00:00.123 UTC (")
        );
    }

    #[test]
    fn snapshot_daemon_labels_show_the_saved_producer_and_accept_legacy_responses() {
        let mut snapshot: applicationlauncher::tracker::SnapshotSummary = serde_json::from_str(
            r#"{"id":1,"name":null,"kind":"recent","created_at_ms":123,"window_count":50}"#,
        )
        .unwrap();
        assert_eq!(snapshot.daemon_version, None);
        assert_eq!(snapshot.daemon_build_id, None);
        assert_eq!(
            snapshot_daemon_label(&snapshot),
            "Daemon version not recorded"
        );

        snapshot.daemon_version = Some("0.3.0".into());
        snapshot.daemon_build_id =
            Some("0.3.0-release-x86_64-unknown-linux-gnu-0123456789abcdef".into());
        assert_eq!(
            snapshot_daemon_label(&snapshot),
            "Daemon 0.3.0 - build 0123456789abcdef"
        );
        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded =
            serde_json::from_str::<applicationlauncher::tracker::SnapshotSummary>(&encoded)
                .unwrap();
        assert_eq!(decoded, snapshot);

        snapshot.daemon_build_id = None;
        assert_eq!(snapshot_daemon_label(&snapshot), "Daemon 0.3.0");
        snapshot.daemon_version = Some(" ".into());
        snapshot.daemon_build_id = Some(String::new());
        assert_eq!(
            snapshot_daemon_label(&snapshot),
            "Daemon version not recorded"
        );
    }

    fn test_window_info(title: &str) -> WindowInfo {
        WindowInfo {
            id: "test-window".to_string(),
            title: title.to_string(),
            raw_title: title.to_string(),
            class: "xfce4-terminal".to_string(),
            desktop_file_name: Some("xfce4-terminal.desktop".to_string()),
            minimized: Some(false),
            demands_attention: false,
            icon_path: None,
            active_process: Some("codex".to_string()),
            exe_path: Some(PathBuf::from("/usr/bin/xfce4-terminal")),
            cwd_path: Some(PathBuf::from("/home/lewis/Dev/applicationlauncher")),
            command_line: Some("codex resume".to_string()),
            command_summary: Some("codex resume".to_string()),
            geometry: Some((0, 0, 800, 600)),
            process_chain: Vec::new(),
            tmux_pane: None,
            pid: Some(1234),
            last_activated_at_ms: Some(0),
            activation_sequence: 1,
        }
    }

    #[test]
    fn window_version_rows_preserve_loading_and_unknown_build_status() {
        let rows = |version, pending| {
            super::super::popups::process_version_rows("Application", version, pending)
        };
        assert!(rows(None, true).iter().all(|row| row.value == "Loading..."));
        assert!(
            rows(None, false)
                .iter()
                .all(|row| row.value == "Unavailable")
        );
        let version = crate::windows::versions::ProcessVersion {
            uncompiled_changes: Some("Unknown (no supported local build metadata)".into()),
            ..Default::default()
        };
        let rows = rows(Some(&version), false);
        assert_eq!(rows.len(), 8);
        assert!(!rows.iter().any(|row| row.label.contains("version source")));
        assert!(
            rows.iter()
                .find(|row| row.label == "Application uncompiled changes")
                .unwrap()
                .value
                .starts_with("Unknown")
        );
        assert!(
            rows.iter().all(|row| !row.searched
                && !row.separator_before
                && row.binary_is_current.is_none())
        );
    }

    #[test]
    fn unavailable_running_process_rows_keep_package_metadata_and_explain_the_missing_fields() {
        let version = crate::windows::versions::ProcessVersion {
            running_unavailable: Some("process exited before inspection"),
            installation: Some("[PDP] Pacman Dependency (Package-managed)".into()),
            package: Some("bubblewrap 0.11.2-1.1".into()),
            latest_version: Some("bubblewrap 0.11.2".into()),
            latest_build_id: Some("on-disk-build-id".into()),
            progs_metadata: crate::windows::versions::ProgsMetadata {
                capabilities: Some("CLI".into()),
                description: Some("Unprivileged sandboxing tool".into()),
            },
            ..Default::default()
        };
        let rows = super::super::popups::process_version_rows("Application", Some(&version), false);
        let value = |label: &str| &rows.iter().find(|row| row.label == label).unwrap().value;
        assert_eq!(
            value("Application version"),
            "Unavailable (process exited before inspection)"
        );
        assert_eq!(
            value("Application ELF build ID"),
            "Unavailable (process exited before inspection)"
        );
        assert_eq!(
            value("Application description"),
            "Unprivileged sandboxing tool"
        );
        assert_eq!(value("Application capabilities"), "CLI");
        assert!(
            value("Application installation").contains("installed package: bubblewrap 0.11.2-1.1")
        );
        assert_eq!(
            value("Application latest version (on disk)"),
            "bubblewrap 0.11.2"
        );
        assert_eq!(
            value("Application latest ELF build ID (on disk)"),
            "on-disk-build-id"
        );
        assert!(
            rows.iter()
                .all(|row| !row.searched && row.binary_is_current.is_none())
        );
    }

    #[test]
    fn progs_capabilities_and_description_are_non_searchable_application_metadata() {
        let version = crate::windows::versions::ProcessVersion {
            progs_metadata: crate::windows::versions::ProgsMetadata {
                capabilities: Some(
                    "GUI \u{00b7} CLI  \u{2022}  Primary role: GUI application".into(),
                ),
                description: Some("A cross-platform, GPU-accelerated terminal emulator".into()),
            },
            ..Default::default()
        };
        for role in ["Application", "Node.js runtime"] {
            let rows = super::super::popups::process_version_rows(role, Some(&version), false);
            for (suffix, expected) in [
                (
                    "capabilities",
                    "GUI \u{00b7} CLI  \u{2022}  Primary role: GUI application",
                ),
                (
                    "description",
                    "A cross-platform, GPU-accelerated terminal emulator",
                ),
            ] {
                let row = rows
                    .iter()
                    .find(|row| row.label == format!("{role} {suffix}"))
                    .unwrap();
                assert_eq!(row.value, expected);
                assert!(!row.searched && !row.separator_before && row.binary_is_current.is_none());
            }
        }
    }

    #[test]
    fn running_elf_id_carries_freshness_without_a_separate_status_row() {
        for role in ["Application", "Node.js runtime"] {
            for current in [Some(true), Some(false), None] {
                for build_id in [
                    Some("6e689278f21a31797320afd5269395c09d75e390"),
                    None,
                    Some(""),
                ] {
                    let version = crate::windows::versions::ProcessVersion {
                        build_id: build_id.map(String::from),
                        latest_build_id: Some("on-disk-build-id".into()),
                        running_latest_binary: current,
                        ..Default::default()
                    };
                    let rows =
                        super::super::popups::process_version_rows(role, Some(&version), false);
                    assert!(
                        !rows
                            .iter()
                            .any(|row| row.label.ends_with("running latest binary"))
                    );
                    let running_index = rows
                        .iter()
                        .position(|row| row.label == format!("{role} ELF build ID"))
                        .unwrap();
                    let running = &rows[running_index];
                    assert_eq!(
                        rows[running_index + 1].label,
                        format!("{role} latest ELF build ID (on disk)")
                    );
                    assert_eq!(
                        running.binary_is_current,
                        build_id.filter(|id| !id.is_empty()).and(current)
                    );
                    assert!(
                        rows.iter()
                            .filter(|row| row.label != running.label)
                            .all(|row| row.binary_is_current.is_none())
                    );
                }
            }
        }
    }

    #[test]
    fn repository_version_rows_are_separate_from_running_installed_and_on_disk_versions() {
        let mut version = crate::windows::versions::ProcessVersion {
            version: Some("running binary".into()),
            latest_version: Some("on disk binary".into()),
            installation: Some("[PEP] Pacman Explicit (Package-managed)".into()),
            package_target: Some(crate::windows::versions::PackageTarget {
                name: "xfce4-terminal".into(),
                inferred: false,
            }),
            ..Default::default()
        };
        let pending =
            super::super::popups::process_version_rows("Application", Some(&version), false);
        assert!(pending.iter().any(
            |row| row.label.ends_with("latest package version (Pacman/AUR)")
                && row.value == "Checking Pacman/AUR..."
        ));
        version.latest_package_version = Some(
            "extra/xfce4-terminal 1.2.0-1 (Pacman sync database); Last Pacman sync: 2026-10-07 20:48:01 +01:00"
                .into(),
        );
        let rows = super::super::popups::process_version_rows("Application", Some(&version), false);
        assert!(
            rows.iter()
                .any(|row| row.label == "Application version" && row.value == "running binary")
        );
        assert!(
            rows.iter()
                .any(|row| row.label == "Application latest version (on disk)"
                    && row.value == "on disk binary")
        );
        let repository = rows
            .iter()
            .find(|row| row.label.ends_with("latest package version (Pacman/AUR)"))
            .unwrap();
        assert_eq!(repository.value, version.latest_package_version.unwrap());
        assert!(repository.binary_is_current.is_none() && !repository.searched);
    }

    #[test]
    fn original_local_version_rows_omit_unrelated_package_information() {
        for installation in ["[DVM] DEV (User-managed)", "[NPT] npm (Toolchain-managed)"] {
            let version = crate::windows::versions::ProcessVersion {
                installation: Some(installation.into()),
                version: Some("running version".into()),
                latest_version: Some("local on-disk version".into()),
                uncompiled_changes: Some("Yes".into()),
                latest_package_version: Some(
                    "No exact Pacman/AUR package found: progs; package-name match for local build"
                        .into(),
                ),
                ..Default::default()
            };
            for role in ["Application", "Runtime"] {
                let rows = super::super::popups::process_version_rows(role, Some(&version), false);
                assert!(
                    !rows
                        .iter()
                        .any(|row| row.label.contains("latest package version")
                            || row.label.contains("update source")
                            || row.value.contains("No exact Pacman/AUR"))
                );
                assert!(rows.iter().any(|row| row.value == "local on-disk version"));
                assert!(
                    rows.iter()
                        .any(|row| row.label == format!("{role} uncompiled changes")
                            && row.value == "Yes")
                );
            }
        }
    }

    #[test]
    fn fork_and_clone_version_rows_retain_upstream_package_comparisons() {
        for installation in [
            "[FKM] Fork (User-managed)",
            "[CLX] Git Clone (External source)",
        ] {
            let mut version = crate::windows::versions::ProcessVersion {
                installation: Some(installation.into()),
                package_target: Some(crate::windows::versions::PackageTarget {
                    name: "fish".into(),
                    inferred: true,
                }),
                ..Default::default()
            };
            for latest in [None, Some("extra/fish 4.6.0-1 (Pacman sync database)")] {
                version.latest_package_version = latest.map(str::to_owned);
                for role in ["Application", "Runtime"] {
                    let rows =
                        super::super::popups::process_version_rows(role, Some(&version), false);
                    let package = rows
                        .iter()
                        .find(|row| {
                            row.label == format!("{role} latest package version (Pacman/AUR)")
                        })
                        .expect("forks and clones must retain the upstream package comparison");
                    assert_eq!(package.value, latest.unwrap_or("Checking Pacman/AUR..."));
                    assert!(!rows.iter().any(|row| row.label.ends_with("update source")));
                }
            }
        }
    }

    #[test]
    fn hosted_applications_compare_their_own_package_not_the_runtime_package() {
        use crate::windows::versions::{NodeApplication, ProcessVersion, ScriptApplication};
        for runtime in ["Node.js", "Python", "Java"] {
            let mut version = ProcessVersion {
                runtime_name: Some(runtime),
                application_installation: Some("[DVM] DEV (User-managed)".into()),
                application_latest_package_version: Some(
                    "No exact Pacman/AUR package found: local-app".into(),
                ),
                package_target: Some(crate::windows::versions::PackageTarget {
                    name: "runtime".into(),
                    inferred: false,
                }),
                latest_package_version: Some("core/runtime 1.0 (Pacman sync database)".into()),
                ..Default::default()
            };
            if runtime == "Node.js" {
                version.application = Some(NodeApplication {
                    package: "@local/app".into(),
                    installed_version: "1.0".into(),
                    entry_point: "/local/app/index.js".into(),
                    manifest: "/local/app/package.json".into(),
                    identification: "Test fixture",
                });
            } else {
                version.script_application = Some(ScriptApplication {
                    name: "local-app".into(),
                    target: "main".into(),
                    kind: "Local application",
                    entry_point: Some("/local/app/main".into()),
                    identification: "Test fixture",
                });
            }
            let application = super::super::popups::application_version_rows(
                "Application",
                Some(&version),
                false,
            );
            assert!(
                !application
                    .iter()
                    .any(|row| row.label == "Application update source"),
                "{runtime}"
            );
            assert!(
                !application
                    .iter()
                    .any(|row| row.label.contains("latest package version")),
                "{runtime}"
            );
            version.application_installation = Some("[FKM] Fork (User-managed)".into());
            version.application_package_target = Some(crate::windows::versions::PackageTarget {
                name: "upstream-app".into(),
                inferred: true,
            });
            version.application_latest_package_version =
                Some("aur/upstream-app 2.0 (Paru/AUR)".into());
            let application = super::super::popups::application_version_rows(
                "Application",
                Some(&version),
                false,
            );
            assert!(
                application.iter().any(|row| row.label
                    == "Application latest package version (Pacman/AUR)"
                    && row.value == "aur/upstream-app 2.0 (Paru/AUR)"),
                "{runtime}"
            );
            assert!(
                !application
                    .iter()
                    .any(|row| row.value.contains("core/runtime")),
                "{runtime}"
            );
            let runtime_rows =
                super::super::popups::process_version_rows("Runtime", Some(&version), false);
            assert!(
                runtime_rows.iter().any(|row| row.label
                    == "Runtime latest package version (Pacman/AUR)"
                    && row.value == "core/runtime 1.0 (Pacman sync database)"),
                "{runtime}"
            );
        }
    }

    #[test]
    fn node_app_versions_are_separate_from_the_runtime_executable() {
        let version = crate::windows::versions::ProcessVersion {
            runtime_name: Some("Node.js"),
            application: Some(crate::windows::versions::NodeApplication {
                package: "@earendil-works/pi-coding-agent".into(),
                installed_version: "0.84.2".into(),
                entry_point: "/installed/pi/dist/cli.js".into(),
                manifest: "/installed/pi/package.json".into(),
                identification: "Command lookup (inferred; installed metadata)",
            }),
            version: Some("v26.4.0".into()),
            latest_version: Some("v26.4.0".into()),
            package: Some("nodejs 26.4.0-2".into()),
            installation: Some("[PEP] Pacman Explicit (Package-managed)".into()),
            application_installation: Some("[NPT] npm (Toolchain-managed)".into()),
            application_progs_metadata: crate::windows::versions::ProgsMetadata {
                capabilities: Some("CLI  \u{2022}  Primary role: Command-line tool".into()),
                description: Some("Pi coding agent".into()),
            },
            progs_metadata: crate::windows::versions::ProgsMetadata {
                capabilities: Some(
                    "CLI \u{00b7} LIB  \u{2022}  Primary role: Command-line tool".into(),
                ),
                description: Some("Node.js JavaScript runtime".into()),
            },
            ..Default::default()
        };
        let rows =
            super::super::popups::application_version_rows("Application", Some(&version), false);
        let runtime_rows =
            super::super::popups::process_version_rows("Runtime", Some(&version), false);
        let value = |label: &str| {
            rows.iter()
                .chain(runtime_rows.iter())
                .find(|row| row.label == label)
                .unwrap()
                .value
                .as_str()
        };
        assert_eq!(value("Application installed version"), "0.84.2");
        assert_eq!(
            value("Application package"),
            "@earendil-works/pi-coding-agent"
        );
        assert_eq!(
            value("Application installed entry point"),
            "/installed/pi/dist/cli.js"
        );
        assert!(value("Application running version").starts_with("Unknown"));
        assert_eq!(value("Runtime version"), "v26.4.0");
        assert_eq!(value("Application description"), "Pi coding agent");
        assert_eq!(
            value("Application capabilities"),
            "CLI  \u{2022}  Primary role: Command-line tool"
        );
        assert_eq!(value("Runtime description"), "Node.js JavaScript runtime");
        assert_eq!(
            value("Runtime capabilities"),
            "CLI \u{00b7} LIB  \u{2022}  Primary role: Command-line tool"
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.label == "Application latest package version (Pacman/AUR)")
        );
        assert_eq!(
            value("Runtime installation"),
            "[PEP] Pacman Explicit (Package-managed); installed package: nodejs 26.4.0-2"
        );
        assert_eq!(
            value("Application installation"),
            "[NPT] npm (Toolchain-managed)"
        );
        assert!(!rows.iter().any(|row| row.label == "Application version"));
        assert!(
            rows.iter()
                .all(|row| !row.searched && !row.separator_before)
        );
        assert!(rows.iter().all(|row| !row.label.starts_with("Runtime ")));
        assert!(
            runtime_rows
                .iter()
                .all(|row| !row.label.starts_with("Application "))
        );
    }

    fn hosted_process(name: &str, executable: &str) -> ProcessInfoNode {
        ProcessInfoNode {
            identity: ProcessIdentity {
                pid: 42,
                start_ticks: 100,
            },
            parent_pid: 41,
            depth: 2,
            name: name.into(),
            header: format!("{name} (pid 42) - {executable}").into(),
            executable: Some(executable.into()),
            executable_identity: None,
            cwd: Some("/work".into()),
            command_line: Some(name.into()),
            command_summary: Some(name.into()),
        }
    }

    #[test]
    fn hosted_metadata_groups_are_separate_stable_and_independently_collapsible() {
        use crate::windows::versions::{NodeApplication, ProcessVersion};
        let ctx = egui::Context::default();
        let node = hosted_process("pi", "/usr/bin/node");
        let id = egui::Id::new("pi-process");
        let version = ProcessVersion {
            runtime_name: Some("Node.js"),
            application: Some(NodeApplication {
                package: "@earendil-works/pi-coding-agent".into(),
                installed_version: "0.84.2".into(),
                entry_point: "/installed/pi/dist/cli.js".into(),
                manifest: "/installed/pi/package.json".into(),
                identification: "test fixture",
            }),
            version: Some("v26.4.0".into()),
            build_id: Some("node-build-id".into()),
            ..Default::default()
        };
        let groups =
            super::super::popups::hosted_metadata_sections(&ctx, id, &node, Some(&version));
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].header.as_ref(), "Application: Pi");
        assert_eq!(groups[1].header.as_ref(), "Runtime: Node.js");
        assert_ne!(groups[0].id, groups[1].id);
        assert!(groups.iter().all(|group| group.id != id));
        assert!(
            groups[0]
                .rows
                .iter()
                .all(|row| !row.label.starts_with("Runtime"))
        );
        assert!(
            groups[1]
                .rows
                .iter()
                .any(|row| row.label == "Runtime executable path" && row.value == "/usr/bin/node")
        );
        assert!(
            groups[1]
                .rows
                .iter()
                .any(|row| row.label == "Runtime version" && row.value == "v26.4.0")
        );
        assert!(
            groups[1]
                .rows
                .iter()
                .any(|row| row.label == "Runtime ELF build ID" && row.value == "node-build-id")
        );
        let mut closed = egui::collapsing_header::CollapsingState::load_with_default_open(
            &ctx,
            groups[0].id,
            true,
        );
        closed.set_open(false);
        closed.store(&ctx);
        let collapsed =
            super::super::popups::hosted_metadata_sections(&ctx, id, &node, Some(&version));
        assert!(collapsed[0].rows.is_empty());
        assert!(!collapsed[1].rows.is_empty());
        assert_eq!(collapsed[0].id, groups[0].id);
        let other = super::super::popups::hosted_metadata_sections(
            &ctx,
            egui::Id::new("another-window"),
            &node,
            Some(&version),
        );
        assert!(!other[0].rows.is_empty());
        assert!(
            super::super::popups::hosted_metadata_sections(
                &ctx,
                id,
                &hosted_process("fish", "/usr/bin/fish"),
                None
            )
            .is_empty()
        );
        let loading = super::super::popups::hosted_metadata_sections(
            &ctx,
            egui::Id::new("loading"),
            &node,
            None,
        );
        assert_eq!(loading[0].rows.len(), 1);
        assert_eq!(loading[0].rows[0].value, "Loading...");
    }

    #[test]
    fn python_and_java_application_groups_never_borrow_runtime_versions_or_packages() {
        use crate::windows::versions::{ProcessVersion, ScriptApplication};
        let ctx = egui::Context::default();
        for (runtime, executable, name, entry, kind) in [
            (
                "Python",
                "/usr/bin/python3",
                "whisper_service",
                "/work/whisper_service.py",
                "Python script",
            ),
            (
                "Java",
                "/usr/bin/java",
                "viewer",
                "/work/viewer.jar",
                "Java archive",
            ),
        ] {
            let node = hosted_process(name, executable);
            let version = ProcessVersion {
                runtime_name: Some(runtime),
                version: Some("runtime 99".into()),
                package: Some("runtime-package 99".into()),
                script_application: Some(ScriptApplication {
                    name: name.into(),
                    entry_point: Some(entry.into()),
                    target: entry.into(),
                    kind,
                    identification: "Process file argument",
                }),
                ..Default::default()
            };
            let groups = super::super::popups::hosted_metadata_sections(
                &ctx,
                egui::Id::new(runtime),
                &node,
                Some(&version),
            );
            assert_eq!(groups[0].header.as_ref(), format!("Application: {name}"));
            assert_eq!(groups[1].header.as_ref(), format!("Runtime: {runtime}"));
            assert!(
                groups[0]
                    .rows
                    .iter()
                    .any(|row| row.label == "Application entry point" && row.value == entry)
            );
            assert!(
                groups[0]
                    .rows
                    .iter()
                    .all(|row| !row.value.contains("runtime 99")
                        && !row.value.contains("runtime-package"))
            );
            assert!(
                groups[0]
                    .rows
                    .iter()
                    .any(|row| row.label == "Application running version"
                        && row.value.starts_with("Unknown"))
            );
            assert!(
                groups[1]
                    .rows
                    .iter()
                    .any(|row| row.label == "Runtime version" && row.value == "runtime 99")
            );
        }
    }

    #[test]
    fn pi_startup_build_evidence_is_separate_from_node_and_never_inferred_from_package_version() {
        use crate::windows::versions::{
            NodeApplication, PiBuildEvidence, PiBuildStatus, ProcessVersion,
        };
        let mut version = ProcessVersion {
            runtime_name: Some("Node.js"),
            application: Some(NodeApplication {
                package: "@earendil-works/pi-coding-agent".into(),
                installed_version: "0.85.0".into(),
                entry_point: "/new/pi/dist/cli.js".into(),
                manifest: "/new/pi/package.json".into(),
                identification: "Installed package, not a running identity",
            }),
            version: Some("v26.4.0".into()),
            running_latest_binary: Some(true),
            build_id: Some("node-elf-identity".into()),
            ..Default::default()
        };
        let rows =
            super::super::popups::application_version_rows("Application", Some(&version), false);
        let row = |rows: Vec<crate::models::InfoPopupRow>, name: &str| {
            rows.into_iter().find(|row| row.label == name).unwrap()
        };
        assert!(
            !rows
                .iter()
                .any(|row| row.label == "Application build status")
        );
        for label in [
            "Application running code fingerprint",
            "Application latest extensions fingerprint",
        ] {
            let fingerprint = row(rows.clone(), label);
            assert_eq!(fingerprint.value, "Unknown");
            assert_eq!(fingerprint.binary_is_current, None);
        }
        assert!(
            row(rows, "Application running version")
                .value
                .starts_with("Unknown")
        );
        for (status, color) in [
            (PiBuildStatus::Current, Some(true)),
            (PiBuildStatus::RestartRequired, Some(false)),
            (PiBuildStatus::InstallationUpdateRequired, Some(false)),
            (PiBuildStatus::Unknown, None),
        ] {
            version.application_build = Some(PiBuildEvidence {
                status,
                reason: "Synthetic explicit startup evidence".into(),
                running_version: Some("0.84.2".into()),
                running_entry: Some("/old/pi/dist/cli.js".into()),
                running_sdk: Some("a".repeat(64)),
                latest_sdk: Some("b".repeat(64)),
                running_extensions: Some("c".repeat(64)),
                latest_extensions: Some("d".repeat(64)),
                scope: Some("Pi SDK and loaded extensions".into()),
                record_path: Some("/run/user/1000/pi/processes/42-99.json".into()),
                ..Default::default()
            });
            let rows = super::super::popups::application_version_rows(
                "Application",
                Some(&version),
                false,
            );
            assert!(
                !rows
                    .iter()
                    .any(|row| row.label == "Application build status")
            );
            assert!(rows.iter().all(|row| !row.separator_before));
            let labels: Vec<_> = rows.iter().map(|row| row.label.as_str()).collect();
            assert_eq!(labels.iter().collect::<HashSet<_>>().len(), labels.len());
            for (running, installed) in [
                (
                    "Application running version",
                    "Application installed version",
                ),
                (
                    "Application running entry point",
                    "Application installed entry point",
                ),
                (
                    "Application running code fingerprint",
                    "Application latest installed code fingerprint",
                ),
                (
                    "Application running extensions fingerprint",
                    "Application latest extensions fingerprint",
                ),
            ] {
                assert!(
                    labels.windows(2).any(|pair| pair == [running, installed]),
                    "{running} and {installed} must be adjacent"
                );
            }
            for fingerprint in &rows {
                let expected = if matches!(
                    fingerprint.label.as_str(),
                    "Application running code fingerprint"
                        | "Application latest extensions fingerprint"
                ) {
                    color
                } else {
                    None
                };
                assert_eq!(
                    fingerprint.binary_is_current, expected,
                    "{}",
                    fingerprint.label
                );
            }
            assert_eq!(
                row(rows.clone(), "Application running version").value,
                "0.84.2"
            );
            assert_eq!(
                row(rows.clone(), "Application installed version").value,
                "0.85.0"
            );
            assert_eq!(
                row(rows.clone(), "Application running entry point").value,
                "/old/pi/dist/cli.js"
            );
            assert_eq!(
                row(rows.clone(), "Application installed entry point").value,
                "/new/pi/dist/cli.js"
            );
            assert_eq!(
                row(rows.clone(), "Application installed package manifest").value,
                "/new/pi/package.json"
            );
            assert_eq!(
                row(rows.clone(), "Application running code fingerprint").value,
                "a".repeat(64)
            );
            assert_eq!(
                row(
                    rows.clone(),
                    "Application latest installed code fingerprint"
                )
                .value,
                "b".repeat(64)
            );
            assert_eq!(
                row(rows.clone(), "Application latest extensions fingerprint").value,
                "d".repeat(64)
            );
            assert_eq!(
                row(
                    super::super::popups::process_version_rows("Runtime", Some(&version), false),
                    "Runtime ELF build ID"
                )
                .value,
                "node-elf-identity"
            );
            assert_eq!(
                row(
                    super::super::popups::process_version_rows("Runtime", Some(&version), false),
                    "Runtime ELF build ID"
                )
                .binary_is_current,
                Some(true)
            );
        }
    }

    #[test]
    fn pi_missing_fingerprints_are_neutral_even_with_a_known_build_status() {
        use crate::windows::versions::{
            NodeApplication, PiBuildEvidence, PiBuildStatus, ProcessVersion,
        };
        for status in [
            PiBuildStatus::Current,
            PiBuildStatus::RestartRequired,
            PiBuildStatus::InstallationUpdateRequired,
            PiBuildStatus::Unknown,
        ] {
            for fingerprint in [None, Some(String::new()), Some("   ".into())] {
                let version = ProcessVersion {
                    runtime_name: Some("Node.js"),
                    application: Some(NodeApplication {
                        package: "@earendil-works/pi-coding-agent".into(),
                        installed_version: "0.85.0".into(),
                        entry_point: "/pi/dist/cli.js".into(),
                        manifest: "/pi/package.json".into(),
                        identification: "Test fixture",
                    }),
                    application_build: Some(PiBuildEvidence {
                        status,
                        running_sdk: fingerprint.clone(),
                        latest_extensions: fingerprint,
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let rows = super::super::popups::application_version_rows(
                    "Application",
                    Some(&version),
                    false,
                );
                for label in [
                    "Application running code fingerprint",
                    "Application latest extensions fingerprint",
                ] {
                    let row = rows.iter().find(|row| row.label == label).unwrap();
                    assert_eq!(row.binary_is_current, None, "{label}, {status:?}");
                }
            }
        }
    }

    #[test]
    fn window_owner_section_contains_only_window_metadata_and_its_own_version() {
        let mut window = test_window_info("codex - ~/Dev/applicationlauncher - Terminal");
        window.process_chain.push(ProcessChainEntry {
            pid: 4321,
            name: "codex".into(),
            exe_path: Some(PathBuf::from("/usr/bin/codex")),
        });
        let version_rows = |role: &str| {
            super::super::popups::process_version_rows(
                role,
                Some(&crate::windows::versions::ProcessVersion {
                    version: Some("running version".into()),
                    latest_version: Some("latest version".into()),
                    latest_build_id: Some("latest-elf-id".into()),
                    running_latest_binary: Some(false),
                    uncompiled_changes: Some("Yes".into()),
                    installation: Some("[FKM] Fork (User-managed)".into()),
                    package_target: Some(crate::windows::versions::PackageTarget {
                        name: "xfce4-terminal".into(),
                        inferred: true,
                    }),
                    ..Default::default()
                }),
                false,
            )
        };

        for in_tmux in [false, true] {
            window.tmux_pane = in_tmux.then(|| applicationlauncher::tracker::LiveTmuxPane {
                client_pid: 123,
                session: applicationlauncher::tracker::TmuxSession {
                    socket_path: "/tmp/test-tmux.sock".into(),
                    session_name: "test-session".into(),
                    ..Default::default()
                },
                pane_id: "%0".into(),
                pane_pid: 4320,
                process_pid: 4321,
                process_name: "codex".into(),
                cwd: window.cwd_path.clone().unwrap(),
                title: "working".into(),
            });
            let rows = super::super::popups::window_info_rows(
                &window,
                "/usr/share/applications/xfce4-terminal.desktop".into(),
                version_rows("Application"),
            );
            let labels = vec![
                "Applicationlauncher title",
                "Raw window title",
                "Application key",
                "Window ID",
                "Class",
                "Window desktop file",
                "Window PID",
                "Window executable",
                "Window executable path",
                "Geometry",
                "Minimized",
                "Last activated",
                "Activation sequence",
                "Application version",
                "Application latest version (on disk)",
                "Application latest package version (Pacman/AUR)",
                "Application uncompiled changes",
                "Application installation",
                "Application capabilities",
                "Application description",
                "Application ELF build ID",
                "Application latest ELF build ID (on disk)",
            ];
            assert_eq!(
                rows.iter()
                    .map(|row| row.label.as_str())
                    .collect::<Vec<_>>(),
                labels
            );
            assert_eq!(
                rows.iter()
                    .filter(|row| row.separator_before)
                    .map(|row| row.label.as_str())
                    .collect::<Vec<_>>(),
                Vec::<&str>::new()
            );
            let value = |label: &str| {
                rows.iter()
                    .find(|row| row.label == label)
                    .unwrap()
                    .value
                    .as_str()
            };
            assert_eq!(value("Window executable path"), "/usr/bin/xfce4-terminal");
            assert_eq!(value("Window PID"), "1234");
            assert_eq!(value("Geometry"), "x=0, y=0, width=800, height=600");
            assert_eq!(value("Application version"), "running version");
            assert_eq!(
                value("Application latest version (on disk)"),
                "latest version"
            );
            assert_eq!(value("Application uncompiled changes"), "Yes");
            assert_eq!(
                value("Application installation"),
                "[FKM] Fork (User-managed)"
            );
            assert_eq!(
                value("Application latest ELF build ID (on disk)"),
                "latest-elf-id"
            );
            let searched_labels = vec![
                "Applicationlauncher title",
                "Application key",
                "Window executable",
            ];
            assert_eq!(
                rows.iter()
                    .filter(|row| row.searched)
                    .map(|row| row.label.as_str())
                    .collect::<Vec<_>>(),
                searched_labels
            );
        }
    }

    #[test]
    fn process_sections_use_their_own_metadata_and_keep_tmux_fields_under_tmux() {
        let mut window = test_window_info("tmux: work - pi - ~/work - Terminal");
        window.active_process = Some("pi".into());
        window.command_line = Some("pi".into());
        window.command_summary = Some("pi".into());
        window.process_chain.push(ProcessChainEntry {
            pid: 7,
            name: "pi".into(),
            exe_path: Some("/usr/bin/node".into()),
        });
        window.tmux_pane = Some(applicationlauncher::tracker::LiveTmuxPane {
            client_pid: 5,
            session: applicationlauncher::tracker::TmuxSession {
                server_pid: 20,
                session_name: "work".into(),
                socket_path: "/tmp/test-tmux.sock".into(),
                ..Default::default()
            },
            pane_id: "%1".into(),
            pane_pid: 6,
            process_pid: 7,
            process_name: "pi".into(),
            cwd: window.cwd_path.clone().unwrap(),
            title: "Pi task".into(),
        });
        for (pid, name, executable) in [
            (5, "tmux", "/usr/bin/tmux"),
            (20, "tmux", "/usr/bin/tmux"),
            (6, "fish", "/usr/bin/fish"),
            (8, "bash", "/usr/bin/bash"),
            (7, "pi", "/usr/bin/node"),
        ] {
            let active = pid == 7;
            let cwd = if active {
                window.cwd_path.clone()
            } else {
                Some(PathBuf::from(format!("/own-directory/{name}")))
            };
            let node = ProcessInfoNode {
                identity: ProcessIdentity {
                    pid,
                    start_ticks: pid as u64,
                },
                parent_pid: 4,
                depth: 0,
                name: name.into(),
                header: name.into(),
                executable: Some(executable.into()),
                executable_identity: None,
                cwd: cwd.clone(),
                command_line: Some(name.into()),
                command_summary: Some(name.into()),
            };
            let rows = super::super::popups::process_info_rows(
                &window,
                &node,
                format!("/applications/{name}.desktop"),
                Vec::new(),
            );
            let value = |label: &str| {
                rows.iter()
                    .find(|row| row.label == label)
                    .unwrap()
                    .value
                    .clone()
            };
            assert_eq!(value("Process name"), name);
            assert_eq!(value("PID"), pid.to_string());
            assert_eq!(value("Executable path"), executable);
            assert_eq!(
                value("Desktop file"),
                format!("/applications/{name}.desktop")
            );
            assert_eq!(
                value("Working directory"),
                display_path(cwd.as_deref().unwrap())
            );
            assert!(!rows.iter().any(|row| row.label.starts_with("Window ")
                || row.label == "Geometry"
                || row.label == "Applicationlauncher title"));
            assert_eq!(
                rows.iter().any(|row| row.label == "Tmux session"),
                [5, 20].contains(&pid)
            );
            if !active {
                assert!(
                    rows.iter()
                        .filter(|row| row.label != "Tmux session")
                        .all(|row| !row.searched)
                );
            }
        }
    }

    #[test]
    fn info_process_tree_targets_ignore_spinner_titles_but_track_process_changes() {
        let mut window = test_window_info("codex - project - Terminal");
        window.process_chain.push(ProcessChainEntry {
            pid: 4321,
            name: "codex".into(),
            exe_path: Some(PathBuf::from("/usr/bin/codex")),
        });
        let target = WindowProcessTreeTarget::for_window(&window);
        window.title = "codex - \u{280b} project - Terminal".into();
        window.raw_title = "[ ! ] Action Required - project - Terminal".into();
        window.activation_sequence += 1;
        assert_eq!(WindowProcessTreeTarget::for_window(&window), target);
        window.process_chain[0].pid += 1;
        assert_ne!(WindowProcessTreeTarget::for_window(&window), target);
        window.process_chain[0].pid -= 1;
        window.pid = Some(5678);
        assert_ne!(WindowProcessTreeTarget::for_window(&window), target);
    }

    fn test_kwin_payload(title: &str, demands_attention: bool) -> KWinWindowPayload {
        KWinWindowPayload {
            id: "test-window".to_string(),
            title: title.to_string(),
            class: "xfce4-terminal".to_string(),
            pid: 1234,
            desktop_file_name: "xfce4-terminal.desktop".to_string(),
            x: 0,
            y: 0,
            width: 800,
            height: 600,
            minimized: true,
            demands_attention,
            skip_taskbar: false,
            skip_switcher: false,
            last_activated_at_ms: Some(0),
            activation_sequence: 1,
        }
    }

    #[test]
    fn window_feed_reset_discards_stale_pre_snapshot_windows() {
        let mut stale = test_kwin_payload("<EMPTY> — CopyQ", false);
        stale.id = "stale-copyq".to_string();
        let mut current = test_kwin_payload("first title", false);
        current.id = "current-copyq".to_string();
        let mut updated = current.clone();
        updated.title = "http://127.0.0.1/word/angstrom — CopyQ".to_string();

        let events = coalesce_window_feed_events(vec![
            WindowFeedEvent::Upsert(stale),
            WindowFeedEvent::Reset,
            WindowFeedEvent::Upsert(current),
            WindowFeedEvent::Upsert(updated.clone()),
        ]);

        assert_eq!(events.len(), 2);
        assert!(matches!(events.first(), Some(WindowFeedEvent::Reset)));
        assert!(matches!(
            events.get(1),
            Some(WindowFeedEvent::Upsert(payload))
                if payload.id == updated.id && payload.title == updated.title
        ));
    }

    #[test]
    fn window_feed_coalesces_to_the_latest_full_snapshot() {
        let mut first = test_kwin_payload("first", false);
        first.id = "first".into();
        let mut latest = test_kwin_payload("latest", false);
        latest.id = "latest".into();

        let events = coalesce_window_feed_events(vec![
            WindowFeedEvent::Snapshot(vec![first]),
            WindowFeedEvent::Snapshot(vec![latest.clone()]),
        ]);

        assert_eq!(events.len(), 2);
        assert!(matches!(events.first(), Some(WindowFeedEvent::Reset)));
        assert!(matches!(
            events.get(1),
            Some(WindowFeedEvent::Upsert(payload))
                if payload.id == latest.id && payload.title == latest.title
        ));
    }

    #[test]
    fn occupied_window_feed_inbox_keeps_only_latest_delta_without_another_wakeup() {
        let first = test_kwin_payload("first", false);
        let mut first_latest = first.clone();
        first_latest.title = "first latest".into();
        let mut second = test_kwin_payload("second", false);
        second.id = "second".into();
        let mut pending = None;

        assert!(queue_window_feed_update(
            &mut pending,
            vec![WindowFeedEvent::Upsert(first)],
        ));
        assert!(!queue_window_feed_update(
            &mut pending,
            vec![WindowFeedEvent::Upsert(first_latest.clone())],
        ));
        assert!(!queue_window_feed_update(
            &mut pending,
            vec![WindowFeedEvent::Upsert(second.clone())],
        ));
        assert!(matches!(
            pending.as_deref(),
            Some([
                WindowFeedEvent::Upsert(first_payload),
                WindowFeedEvent::Upsert(second_payload),
            ]) if first_payload == &first_latest && second_payload == &second
        ));
    }

    #[test]
    fn process_tree_refresh_is_non_reentrant_and_respects_cache_age() {
        let now = Instant::now();

        assert!(super::super::feed::process_tree_cache_refresh_due(
            None, false, now,
        ));
        assert!(!super::super::feed::process_tree_cache_refresh_due(
            None, true, now,
        ));
        assert!(!super::super::feed::process_tree_cache_refresh_due(
            Some(now),
            false,
            now + Duration::from_millis(499),
        ));
        assert!(super::super::feed::process_tree_cache_refresh_due(
            Some(now),
            false,
            now + Duration::from_millis(500),
        ));
    }

    #[test]
    fn tracker_snapshots_become_incremental_window_events() {
        let mut spinner = test_kwin_payload("codex - ⠇ project - Terminal", false);
        spinner.id = "spinner".into();
        let mut removed = test_kwin_payload("removed", false);
        removed.id = "removed".into();

        let (initial_events, previous) =
            window_feed_events_from_snapshot(None, vec![spinner.clone(), removed]);
        assert!(matches!(
            initial_events.as_slice(),
            [WindowFeedEvent::Snapshot(payloads)] if payloads.len() == 2
        ));

        spinner.title = "codex - ⠧ project - Terminal".into();
        let (events, current) =
            window_feed_events_from_snapshot(Some(&previous), vec![spinner.clone()]);

        assert_eq!(events.len(), 2);
        assert!(matches!(
            events.first(),
            Some(WindowFeedEvent::Upsert(payload)) if payload == &spinner
        ));
        assert!(matches!(
            events.get(1),
            Some(WindowFeedEvent::Remove(id)) if id == "removed"
        ));
        assert_eq!(current.len(), 1);
    }

    fn terminal_record(
        tab_uuid: &str,
        window_uuid: &str,
        title: &str,
        pty: &str,
    ) -> TerminalDbusRecord {
        TerminalDbusRecord {
            terminal_pid: 0,
            window_uuid: window_uuid.to_string(),
            tab_uuid: tab_uuid.to_string(),
            active: true,
            window_title: title.to_string(),
            working_directory: "/home/lewis/Dev/applicationlauncher".to_string(),
            child_pid: 0,
            foreground_pid: 0,
            foreground_pgid: 0,
            pty: pty.to_string(),
        }
    }

    #[test]
    fn terminal_tab_matching_prefers_unique_pty_over_duplicate_titles() {
        let identity = TerminalWindowIdentity {
            normalized_title: normalize_window_sort_title("⠇ xfce4-terminal - Terminal"),
            cwd: Some(PathBuf::from("/home/lewis/Dev/applicationlauncher")),
            ptys: HashSet::from(["/dev/pts/23".to_string()]),
            ..Default::default()
        };
        let records = vec![
            terminal_record(
                "tab-a",
                "window-a",
                "⠋ xfce4-terminal - Terminal",
                "/dev/pts/22",
            ),
            terminal_record(
                "tab-b",
                "window-b",
                "⠴ xfce4-terminal - Terminal",
                "/dev/pts/23",
            ),
        ];

        assert_eq!(select_terminal_tab(&identity, &records).unwrap(), "tab-b");
    }

    #[test]
    fn terminal_tab_matching_allows_a_unique_raw_window_title() {
        let identity = TerminalWindowIdentity {
            normalized_title: normalize_window_sort_title("codex - project - Terminal"),
            ..Default::default()
        };
        let records = vec![
            terminal_record("tab-a", "window-a", "fish - ~ - Terminal", "/dev/pts/22"),
            terminal_record(
                "tab-b",
                "window-b",
                "codex - project - Terminal",
                "/dev/pts/23",
            ),
        ];

        assert_eq!(select_terminal_tab(&identity, &records).unwrap(), "tab-b");
    }

    #[test]
    fn terminal_tab_matching_rejects_ambiguous_titles_without_process_identity() {
        let identity = TerminalWindowIdentity {
            normalized_title: normalize_window_sort_title("fish - ~ - Terminal"),
            ..Default::default()
        };
        let records = vec![
            terminal_record("tab-a", "window-a", "fish - ~ - Terminal", "/dev/pts/22"),
            terminal_record("tab-b", "window-b", "fish - ~ - Terminal", "/dev/pts/23"),
        ];

        assert!(select_terminal_tab(&identity, &records).is_err());
    }

    #[test]
    fn terminal_metadata_matches_a_unique_raw_window_title() {
        let mut fish = terminal_record("tab-fish", "window-fish", "~ - Terminal", "/dev/pts/19");
        fish.child_pid = 4152349;
        fish.foreground_pid = 4152349;
        let mut chatgpt = terminal_record(
            "tab-chatgpt",
            "window-chatgpt",
            "npm run build - Terminal",
            "/dev/pts/17",
        );
        chatgpt.child_pid = 4096357;
        chatgpt.foreground_pid = 4097817;
        let records = vec![fish, chatgpt];

        let matched = terminal_record_for_window_title("~ - Terminal", &records).unwrap();
        assert_eq!(matched.tab_uuid, "tab-fish");
        assert_eq!(matched.foreground_pid, 4152349);
    }

    #[test]
    fn terminal_metadata_matches_blank_dynamic_title_spacing_variants() {
        let mut nvtop = terminal_record("tab-nvtop", "window-nvtop", " - Terminal", "/dev/pts/31");
        nvtop.child_pid = 2816861;
        nvtop.foreground_pid = 2816861;

        assert_eq!(
            normalize_window_sort_title("- Terminal"),
            normalize_window_sort_title(&nvtop.window_title)
        );
        let records = [nvtop];
        let matched = terminal_record_for_window_title("- Terminal", &records).unwrap();
        assert_eq!(matched.tab_uuid, "tab-nvtop");
        assert_eq!(matched.foreground_pid, 2816861);
        assert_eq!(
            terminal_display_title(
                "- Terminal",
                "nvtop",
                Some("nvtop"),
                Some("~/Dev/applicationlauncher"),
                None,
            ),
            "nvtop - ~/Dev/applicationlauncher - Terminal"
        );
    }

    #[test]
    fn terminal_metadata_does_not_guess_between_duplicate_titles() {
        let mut first = terminal_record("tab-a", "window-a", "~ - Terminal", "/dev/pts/19");
        first.child_pid = 100;
        let mut second = terminal_record("tab-b", "window-b", "~ - Terminal", "/dev/pts/20");
        second.child_pid = 200;

        assert!(terminal_record_for_window_title("~ - Terminal", &[first, second]).is_none());
    }

    #[test]
    fn terminal_metadata_uses_server_pid_for_duplicate_generic_titles() {
        let mut htop = terminal_record("tab-htop", "window-htop", "Terminal", "/dev/pts/19");
        htop.terminal_pid = 10;
        htop.child_pid = 100;
        let mut nvtop = terminal_record("tab-nvtop", "window-nvtop", "Terminal", "/dev/pts/20");
        nvtop.terminal_pid = 20;
        nvtop.child_pid = 200;
        let records = [htop, nvtop];

        assert!(terminal_record_for_window_title("Terminal", &records).is_none());
        assert_eq!(
            terminal_record_for_window(20, "Terminal", &records)
                .map(|record| record.tab_uuid.as_str()),
            Some("tab-nvtop")
        );
    }

    #[test]
    fn terminal_metadata_identifies_its_own_shared_server_process() {
        let mut record = terminal_record("tab-a", "window-a", "~ - Terminal", "/dev/pts/19");
        record.child_pid = 100;
        record.foreground_pid = 101;
        let parents = HashMap::from([(101, 100), (100, 10), (10, 1)]);

        assert!(terminal_server_has_dbus_records(
            10,
            &[record.clone()],
            &parents
        ));
        assert!(!terminal_server_has_dbus_records(20, &[record], &parents));
    }

    #[test]
    #[ignore = "requires the patched XFCE4 Terminal D-Bus service"]
    fn live_terminal_dbus_schema_deserializes() {
        let connection = zbus::blocking::Connection::session().unwrap();
        let proxy = zbus::blocking::Proxy::new(
            &connection,
            TERMINAL_DBUS_SERVICE,
            TERMINAL_DBUS_PATH,
            TERMINAL_DBUS_INTERFACE,
        )
        .unwrap();
        let raw_records: Vec<HashMap<String, zbus::zvariant::OwnedValue>> =
            proxy.call("ListTerminals", &()).unwrap();
        let records = parse_terminal_dbus_records(raw_records);

        assert!(!records.is_empty());
        assert!(records.iter().all(|record| !record.tab_uuid.is_empty()));
        assert!(records.iter().all(|record| !record.pty.is_empty()));
    }

    #[test]
    #[ignore = "requires a live KWin session and kdotool"]
    fn live_fast_window_snapshot_reports_discovered_windows() {
        let kdotool = get_kdotool_path();
        let probe = Command::new(&kdotool)
            .args(["search", "--title", ""])
            .output()
            .expect("kdotool search should execute");
        eprintln!(
            "kdotool={} status={} ids={} stderr={}",
            kdotool.display(),
            probe.status,
            String::from_utf8_lossy(&probe.stdout).lines().count(),
            String::from_utf8_lossy(&probe.stderr).trim()
        );
        let windows = get_open_windows_fast(&kdotool, "breeze-dark")
            .expect("the live KWin window snapshot should be available");

        eprintln!("discovered {} application windows", windows.len());
        for window in &windows {
            eprintln!(
                "{}\t{}\t{}\tactive={:?}\texe={:?}\ticon={:?}",
                window.id,
                window.class,
                window.raw_title,
                window.active_process,
                window.exe_path,
                window.icon_path
            );
        }
        assert!(!windows.is_empty());
    }

    #[test]
    #[ignore = "requires a live Tor Browser window, KWin, and kdotool"]
    fn live_tor_browser_window_uses_tor_icon() {
        let kdotool = get_kdotool_path();
        let windows = get_open_windows_fast(&kdotool, "breeze-dark").unwrap();
        let tor_window = windows
            .iter()
            .find(|window| is_tor_browser_identity(&window.class))
            .expect("a live Tor Browser window should be open");
        let icon = tor_window
            .icon_path
            .as_ref()
            .expect("the Tor Browser window should resolve a dedicated icon");
        let icon_name = icon.to_string_lossy().to_lowercase();

        assert!(icon_name.contains("tor"));
        assert!(!icon_name.contains("firefox"));
    }

    #[test]
    #[ignore = "requires a live idle XFCE4 Terminal window, KWin, and the patched D-Bus API"]
    fn live_idle_terminal_does_not_inherit_electron_process() {
        let kdotool = get_kdotool_path();
        let windows = get_open_windows_fast(&kdotool, "breeze-dark").unwrap();
        let idle_terminals = windows
            .iter()
            .filter(|window| {
                is_terminal_class(&window.class.to_lowercase())
                    && normalize_window_sort_title(&window.raw_title)
                        == normalize_window_sort_title("~ - Terminal")
            })
            .collect::<Vec<_>>();

        assert!(!idle_terminals.is_empty());
        assert!(idle_terminals.iter().all(|window| {
            window.active_process.as_deref() != Some("electron")
                && !window
                    .command_line
                    .as_deref()
                    .is_some_and(|command| command.contains("electron"))
        }));
    }

    #[test]
    #[ignore = "read-only foreground probe; requires APPLICATIONLAUNCHER_TEST_TERMINAL_PID and APPLICATIONLAUNCHER_TEST_TERMINAL_PROGRAM"]
    fn live_terminal_dbus_foreground_program_and_title() {
        let pid = std::env::var("APPLICATIONLAUNCHER_TEST_TERMINAL_PID")
            .unwrap()
            .parse::<i32>()
            .unwrap();
        let expected = std::env::var("APPLICATIONLAUNCHER_TEST_TERMINAL_PROGRAM").unwrap();
        let records = fetch_terminal_dbus_records().unwrap();
        let record = records
            .iter()
            .find(|record| record.active && record.terminal_pid == pid as u32)
            .expect("the selected terminal must have active D-Bus metadata");
        let (children, names, parents) = get_process_tree();
        let window = build_window_info(
            "read-only-terminal-probe".into(),
            record.window_title.clone(),
            "xfce4-terminal".into(),
            Some("xfce4-terminal.desktop".into()),
            Some(pid),
            None,
            None,
            "breeze-dark",
            &mut HashMap::new(),
            &children,
            &names,
            &parents,
            &records,
        )
        .unwrap();
        eprintln!(
            "active={:?} process_pid={:?} title={}",
            window.active_process,
            window.process_chain.first().map(|process| process.pid),
            window.title
        );
        assert_eq!(window.active_process.as_deref(), Some(expected.as_str()));
        assert_eq!(window.process_chain.first().unwrap().name, expected);
        assert!(window.title.starts_with(&format!("{expected} - ")));
    }

    #[test]
    fn reconciliation_adds_missing_windows_without_dropping_feed_state() {
        let mut existing = test_window_info("old title");
        existing.id = "existing".to_string();
        existing.demands_attention = true;
        existing.icon_path = Some(PathBuf::from("/tmp/existing.svg"));
        existing.last_activated_at_ms = Some(123_456);
        existing.activation_sequence = 42;
        let mut feed_only = test_window_info("feed only");
        feed_only.id = "feed-only".to_string();
        let mut spinner = test_window_info("codex - ⠇ applicationlauncher - Terminal");
        spinner.id = "spinner".to_string();
        let mut current = vec![existing.clone(), feed_only, spinner.clone()];

        let mut refreshed = test_window_info("new title");
        refreshed.id = "existing".to_string();
        refreshed.desktop_file_name = None;
        refreshed.geometry = None;
        refreshed.minimized = None;
        refreshed.icon_path = None;
        refreshed.last_activated_at_ms = None;
        refreshed.activation_sequence = 0;
        let mut newly_discovered = test_window_info("new window");
        newly_discovered.id = "new".to_string();
        let mut refreshed_spinner = spinner;
        refreshed_spinner.title = "codex - ⠧ applicationlauncher - Terminal".to_string();
        refreshed_spinner.raw_title = refreshed_spinner.title.clone();

        let (changed, search_changed, cache_updates) = merge_reconciled_windows(
            &mut current,
            vec![refreshed, newly_discovered, refreshed_spinner],
        );

        assert!(changed);
        assert!(search_changed);
        assert_eq!(cache_updates.len(), 1);
        assert_eq!(current.len(), 4);
        assert_eq!(cache_updates[0].0.id, "spinner");
        let merged = current
            .iter()
            .find(|window| window.id == "existing")
            .unwrap();
        assert_eq!(merged.title, "new title");
        assert_eq!(merged.desktop_file_name, existing.desktop_file_name);
        assert_eq!(merged.geometry, existing.geometry);
        assert_eq!(merged.minimized, existing.minimized);
        assert_eq!(merged.icon_path, existing.icon_path);
        assert!(merged.demands_attention);
        assert_eq!(merged.last_activated_at_ms, existing.last_activated_at_ms);
        assert_eq!(merged.activation_sequence, existing.activation_sequence);
        assert!(current.iter().any(|window| window.id == "feed-only"));
        assert!(current.iter().any(|window| window.id == "new"));
    }

    #[test]
    fn failed_terminal_metadata_refresh_waits_for_its_retry_deadline() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(5);

        assert!(!super::super::feed::terminal_metadata_refresh_due(
            false,
            true,
            Some(deadline),
            now,
        ));
        assert!(!super::super::feed::terminal_metadata_refresh_due(
            true,
            true,
            Some(deadline),
            deadline,
        ));
        assert!(super::super::feed::terminal_metadata_refresh_due(
            false,
            true,
            Some(deadline),
            deadline,
        ));
    }

    #[test]
    fn icon_cache_separates_terminal_children_from_standalone_apps() {
        let terminal_child = window_icon_cache_key(
            "xfce4-terminal",
            Some("xfce4-terminal"),
            Some("electron"),
            Some(Path::new("/usr/bin/xfce4-terminal")),
        );
        let standalone_app = window_icon_cache_key(
            "electron",
            None,
            None,
            Some(Path::new(
                "/home/lewis/Dev/chatgpt/node_modules/electron/dist/electron",
            )),
        );

        assert_ne!(terminal_child, standalone_app);
        let mut cache = HashMap::new();
        cache.insert(
            terminal_child,
            Some(PathBuf::from("/usr/share/icons/xfce4-terminal.svg")),
        );
        assert!(!cache.contains_key(&standalone_app));
    }

    #[test]
    fn non_terminal_window_does_not_use_terminal_executable_fallback() {
        assert_eq!(
            resolve_window_icon(
                "breeze-dark",
                "application-with-no-installed-icon",
                None,
                None,
                Some(Path::new("/usr/bin/xfce4-terminal")),
            ),
            None
        );
    }

    #[test]
    fn tor_browser_never_uses_firefox_icon_fallback() {
        let icon = resolve_window_icon(
            "breeze-dark",
            "Tor Browser",
            None,
            None,
            Some(Path::new("/opt/tor-browser/Browser/firefox.real")),
        );

        assert!(
            icon.as_ref()
                .is_none_or(|path| { !path.to_string_lossy().to_lowercase().contains("firefox") })
        );
    }

    #[test]
    fn spinner_and_geometry_updates_do_not_invalidate_window_search() {
        let old = test_window_info("codex - ⠇ applicationlauncher - Terminal");
        let mut new = test_window_info("codex - ⠧ applicationlauncher - Terminal");
        new.geometry = Some((100, 80, 1200, 900));
        new.minimized = Some(true);
        new.demands_attention = true;

        assert!(window_search_metadata_equal(&old, &new));
    }

    #[test]
    fn attention_animation_does_not_change_window_order_or_search_metadata() {
        let old = test_window_info("codex - [ . ] Action Required - ~/Dev/sites/dictai - Terminal");
        let new = test_window_info("codex - [ ! ] Action Required - ~/Dev/sites/dictai - Terminal");

        assert_eq!(window_sort_title_key(&old), window_sort_title_key(&new));
        assert!(window_search_metadata_equal(&old, &new));
    }

    #[test]
    fn duplicate_group_ignores_spinner_and_attention_variants() {
        let plain = test_window_info("codex - ~/Dev/iris - Terminal");
        let mut spinner = test_window_info("codex - ⠴ ~/Dev/iris - Terminal");
        spinner.id = "spinner".to_string();
        let mut attention =
            test_window_info("codex - [ ! ] Action Required - ~/Dev/iris - Terminal");
        attention.id = "attention".to_string();

        assert_eq!(
            duplicate_window_group_key(&plain),
            duplicate_window_group_key(&spinner)
        );
        assert_eq!(
            duplicate_window_group_key(&plain),
            duplicate_window_group_key(&attention)
        );
    }

    #[test]
    fn searched_windows_use_activation_order_only_when_enabled() {
        let mut older = test_window_info("recentneedle");
        older.id = "older".into();
        older.last_activated_at_ms = Some(100);
        let mut newer = test_window_info("recentneedle project");
        newer.id = "newer".into();
        newer.last_activated_at_ms = Some(200);
        let mut unknown = older.clone();
        unknown.id = "unknown".into();
        unknown.last_activated_at_ms = None;
        let mut unrelated = test_window_info("zzzzzzzzzzzz");
        unrelated.id = "unrelated".into();
        unrelated.last_activated_at_ms = Some(999);
        let windows = vec![unknown, newer, unrelated, older];
        let documents: Vec<_> = windows.iter().map(window_search_document).collect();
        for query in ["recentneedle", "recentnedle"] {
            let (base, typo) = search_queries(query).unwrap();
            let fuzzy = ranked_window_matches(&windows, &documents, &base, &typo, None, false);
            let recent = ranked_window_matches(&windows, &documents, &base, &typo, None, true);
            assert_eq!(fuzzy[0].window.id, "older", "{query}");
            assert_eq!(
                recent
                    .iter()
                    .map(|item| item.window.id.as_str())
                    .collect::<Vec<_>>(),
                ["newer", "older", "unknown"],
                "{query}"
            );
            assert_eq!(fuzzy.len(), recent.len());
            for item in &recent {
                let original = fuzzy
                    .iter()
                    .find(|original| original.window.id == item.window.id)
                    .unwrap();
                assert_eq!(item.display_title, original.display_title);
                assert_eq!(item.highlight_segments, original.highlight_segments);
                assert_eq!(item.title_is_typo, original.title_is_typo);
                assert!(
                    !item.highlight_segments.is_empty(),
                    "{query}: {}",
                    item.window.id
                );
            }
        }
    }

    #[test]
    fn search_cache_key_tracks_the_window_ordering_toggle() {
        let key = |recent| {
            filtered_search_cache_key(LauncherMode::Windows, "codex", true, recent, 1, 2, 3)
        };
        assert_eq!(key(false), key(false));
        assert_ne!(key(false), key(true));
        assert_ne!(key(true), key(false));
    }

    #[test]
    fn cached_recent_search_reorders_all_row_data_without_reranking() {
        let windows: Vec<_> = [
            ("newest", Some(300)),
            ("oldest", Some(100)),
            ("unknown", None),
            ("middle", Some(200)),
        ]
        .into_iter()
        .map(|(id, timestamp)| {
            let mut window = test_window_info(id);
            window.id = id.into();
            window.last_activated_at_ms = timestamp;
            window
        })
        .collect();
        let titles = windows
            .iter()
            .map(|window| format!("highlighted {}", window.id))
            .collect();
        let highlights = (0..windows.len())
            .map(|index| vec![(index, index + 1, true)])
            .collect();
        let typos = (0..windows.len()).map(|index| index % 2 == 0).collect();
        let mut results = FilteredSearchResults {
            apps: Arc::new(Vec::new()),
            windows: Arc::new(windows),
            app_display_titles: Arc::new(Vec::new()),
            window_display_titles: Arc::new(titles),
            app_highlight_segments: Arc::new(Vec::new()),
            app_name_highlight_segments: Arc::new(Vec::new()),
            window_highlight_segments: Arc::new(highlights),
            app_title_is_typos: Arc::new(Vec::new()),
            window_title_is_typos: Arc::new(typos),
        };
        let previous = results.clone();
        assert!(helpers::sort_cached_windows_by_last_activation(
            &mut results
        ));
        assert_eq!(
            results
                .windows
                .iter()
                .map(|window| window.id.as_str())
                .collect::<Vec<_>>(),
            ["newest", "middle", "oldest", "unknown"]
        );
        Arc::make_mut(&mut results.windows)[2].last_activated_at_ms = Some(400);
        assert!(helpers::sort_cached_windows_by_last_activation(
            &mut results
        ));
        assert_eq!(
            results
                .windows
                .iter()
                .map(|window| window.id.as_str())
                .collect::<Vec<_>>(),
            ["oldest", "newest", "middle", "unknown"]
        );
        for (index, window) in results.windows.iter().enumerate() {
            let old_index = previous
                .windows
                .iter()
                .position(|old| old.id == window.id)
                .unwrap();
            assert_eq!(
                results.window_display_titles[index],
                previous.window_display_titles[old_index]
            );
            assert_eq!(
                results.window_highlight_segments[index],
                previous.window_highlight_segments[old_index]
            );
            assert_eq!(
                results.window_title_is_typos[index],
                previous.window_title_is_typos[old_index]
            );
        }
        assert_eq!(previous.windows[1].last_activated_at_ms, Some(100));
        assert!(Arc::ptr_eq(&results.apps, &previous.apps));
        let ordered = results.clone();
        assert!(helpers::sort_cached_windows_by_last_activation(
            &mut results
        ));
        assert!(Arc::ptr_eq(&results.windows, &ordered.windows));
        assert!(Arc::ptr_eq(
            &results.window_display_titles,
            &ordered.window_display_titles
        ));
        assert!(Arc::ptr_eq(
            &results.window_highlight_segments,
            &ordered.window_highlight_segments
        ));
        assert!(Arc::ptr_eq(
            &results.window_title_is_typos,
            &ordered.window_title_is_typos
        ));
        Arc::make_mut(&mut results.window_display_titles).pop();
        assert!(!helpers::sort_cached_windows_by_last_activation(
            &mut results
        ));
        assert!(Arc::ptr_eq(&results.windows, &ordered.windows));
    }

    #[test]
    fn last_activation_order_puts_newest_windows_first() {
        let mut older = test_window_info("older");
        older.last_activated_at_ms = Some(100);
        older.activation_sequence = 2;
        let mut newer = test_window_info("newer");
        newer.last_activated_at_ms = Some(200);
        newer.activation_sequence = 1;
        let mut unknown = test_window_info("unknown");
        unknown.last_activated_at_ms = None;
        unknown.activation_sequence = 0;

        assert_eq!(
            compare_windows_by_last_activation(&newer, &older),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_windows_by_last_activation(&newer, &unknown),
            std::cmp::Ordering::Less
        );
        let mut tied = newer.clone();
        tied.activation_sequence += 1;
        assert!(window_last_activation_sort_key(&tied) < window_last_activation_sort_key(&newer));
        tied.activation_sequence = newer.activation_sequence;
        tied.title = "alpha".into();
        assert!(window_last_activation_sort_key(&tied) < window_last_activation_sort_key(&newer));
        tied.title = newer.title.clone();
        newer.id = "b".into();
        tied.id = "a".into();
        assert!(window_last_activation_sort_key(&tied) < window_last_activation_sort_key(&newer));
    }

    #[test]
    fn cached_attention_animation_updates_without_reranking() {
        let old = "codex - [ . ] Action Required - ~/Dev/sites/dictai - Terminal";
        let new = "codex - [ ! ] Action Required - ~/Dev/sites/dictai - Terminal";
        let mut display_title = format!("{old} | codex | ~/Dev/sites/dictai");

        refresh_cached_transient_title(&mut display_title, old, new);

        assert_eq!(display_title, format!("{new} | codex | ~/Dev/sites/dictai"));
    }

    #[test]
    fn cached_spinner_transition_updates_without_reranking() {
        let old = "codex - ~/Dev/iris - Terminal";
        let new = "codex - ⠴ ~/Dev/iris - Terminal";
        let mut display_title = format!("{old} | codex | ~/Dev/iris");

        refresh_cached_transient_title(&mut display_title, old, new);

        assert_eq!(display_title, format!("{new} | codex | ~/Dev/iris"));
    }

    #[test]
    fn searchable_window_changes_still_invalidate_search() {
        let old = test_window_info("codex - ⠇ applicationlauncher - Terminal");
        let renamed = test_window_info("codex - ⠧ fuzzy-rank - Terminal");
        let mut changed_process = old.clone();
        changed_process.active_process = Some("htop".to_string());

        assert!(!window_search_metadata_equal(&old, &renamed));
        assert!(!window_search_metadata_equal(&old, &changed_process));
    }

    #[test]
    fn repeated_window_changes_do_not_extend_search_refresh_deadline() {
        let now = Instant::now();
        let first_deadline = window_search_refresh_deadline(None, now);
        let later_event = now + Duration::from_millis(50);

        assert_eq!(
            first_deadline,
            now + Duration::from_millis(WINDOW_SEARCH_REFRESH_INTERVAL_MS)
        );
        assert_eq!(
            window_search_refresh_deadline(Some(first_deadline), later_event),
            first_deadline
        );
    }

    #[test]
    fn unicode_window_title_is_safe_in_typo_visibility_check() {
        let title = "videos — pcmanfm | pcmanfm | /usr/bin/pcmanfm | ~";

        assert!(!visible_title_has_typo_match(title, "mp"));
    }

    #[test]
    fn row_height_can_be_configured_below_thirty_when_content_fits() {
        assert_eq!(
            effective_list_row_height(18.0, 14.0, 2.0, 8.0, 0.0, false),
            18.0
        );
        assert_eq!(
            effective_list_row_height(12.0, 14.0, 4.0, 8.0, 0.0, false),
            18.0
        );
    }

    #[test]
    fn selection_accent_scales_with_compact_rows() {
        let compact = selected_row_accent_size(12.0);
        assert!(compact.x < 3.0);
        assert!(compact.y < 12.0);
        assert_eq!(selected_row_accent_size(52.0), egui::vec2(3.0, 28.0));
    }

    #[test]
    fn codex_code_mode_uses_codex_terminal_title() {
        assert_eq!(terminal_primary_title("codex-code-mode", None), "codex");
        assert_eq!(terminal_primary_title("codex", None), "codex");
    }

    #[test]
    fn canonical_tmux_codex_titles_cover_legacy_tasks_activity_and_real_directories() {
        for (raw, expected) in [
            (
                "Check ChatGPT link access | diet",
                "~/tasks/diet - Check ChatGPT link access",
            ),
            ("codex repair | diet", "~/tasks/diet - codex repair"),
            (
                "codex - ~/tasks/diet - Check ChatGPT link access",
                "~/tasks/diet - Check ChatGPT link access",
            ),
            (
                "codex \u{280b} ~/tasks/diet - Check ChatGPT link access",
                "\u{280b} ~/tasks/diet - Check ChatGPT link access",
            ),
            (
                "[ ! ] Action Required - codex - ~/tasks/diet - Check ChatGPT link access",
                "[ ! ] Action Required ~/tasks/diet - Check ChatGPT link access",
            ),
            ("codex - ~", "~/tasks/diet"),
            (
                "\u{280b} Check ChatGPT link access | diet",
                "\u{280b} ~/tasks/diet - Check ChatGPT link access",
            ),
            (
                "[ ! ] Action Required | Check ChatGPT link access | diet",
                "[ ! ] Action Required ~/tasks/diet - Check ChatGPT link access",
            ),
            (
                "codex | ~/truncated/cwd... | Task | literal",
                "~/tasks/diet - Task - literal",
            ),
            (
                "codex \u{280b} ~/truncated/cwd... | Task | literal",
                "\u{280b} ~/tasks/diet - Task - literal",
            ),
            ("Task | literal | diet", "~/tasks/diet - Task - literal"),
            ("diet | diet", "~/tasks/diet - diet"),
            ("diet", "~/tasks/diet"),
            ("\u{280b} diet", "\u{280b} ~/tasks/diet"),
        ] {
            assert_eq!(
                tmux_codex_display_title(raw, "~/tasks/diet"),
                format!("codex: {expected} - Terminal")
            );
        }
        assert_eq!(
            tmux_codex_display_title("Task | old-project", "~/new folder"),
            "codex: ~/new folder - Task - Terminal"
        );
    }

    #[test]
    fn configured_codex_and_tmux_titles_preserve_the_actual_desktop_field_order() {
        for dynamic_title in [
            "codex - ~/tasks/diet - Check ChatGPT link access",
            "codex \u{280b} ~/tasks/diet - Check ChatGPT link access",
            "[ ! ] Action Required - codex - ~/tasks/diet - Check ChatGPT link access",
            "codex - ~",
            "codex | ~/tasks/diet | Check ChatGPT link access",
            "codex \u{280b} ~/tasks/diet | Check ChatGPT link access",
            "[ ! ] Action Required | codex | ~/tasks/diet | Check ChatGPT link access",
        ] {
            let expected = format!("{dynamic_title} - Terminal");
            for raw_title in [
                dynamic_title.to_string(),
                expected.clone(),
                format!("Terminal - {dynamic_title}"),
            ] {
                assert_eq!(
                    terminal_display_title(
                        &raw_title,
                        "codex",
                        Some("codex resume --last"),
                        Some("~/tasks/diet"),
                        None,
                    ),
                    expected
                );
            }
            let tmux_title = format!("tmux: diet - {expected}");
            assert_eq!(
                terminal_display_title(
                    &tmux_title,
                    "tmux: client",
                    Some("tmux new-session -A -s diet"),
                    Some("~"),
                    None,
                ),
                tmux_title
            );
        }
        assert_eq!(
            terminal_display_title(
                "codex - Terminal",
                "codex",
                None,
                Some("~/tasks/diet"),
                None
            ),
            "codex - ~/tasks/diet - Terminal"
        );
    }

    #[test]
    fn fish_home_directory_keeps_tilde_in_terminal_title() {
        assert_eq!(
            terminal_display_title("~ - Terminal", "fish", Some("fish"), Some("~"), None),
            "fish - ~ - Terminal"
        );
    }

    #[test]
    fn nested_shell_does_not_hide_foreground_codex_process() {
        let ppid_to_children =
            HashMap::from([(1, vec![2]), (2, vec![3]), (3, vec![4]), (4, vec![5])]);
        let pid_to_name = HashMap::from([
            (2, "fish".to_string()),
            (3, "fish".to_string()),
            (4, "codex".to_string()),
            (5, "plasma-browser-".to_string()),
        ]);
        let stats = HashMap::from([
            (
                2,
                ProcessStat {
                    pid: 2,
                    name: "fish".to_string(),
                    ppid: 1,
                    process_group: 2,
                    session: 2,
                    tty: 10,
                    foreground_process_group: 4,
                },
            ),
            (
                3,
                ProcessStat {
                    pid: 3,
                    name: "fish".to_string(),
                    ppid: 2,
                    process_group: 3,
                    session: 2,
                    tty: 10,
                    foreground_process_group: 4,
                },
            ),
            (
                4,
                ProcessStat {
                    pid: 4,
                    name: "codex".to_string(),
                    ppid: 3,
                    process_group: 4,
                    session: 2,
                    tty: 10,
                    foreground_process_group: 4,
                },
            ),
            (
                5,
                ProcessStat {
                    pid: 5,
                    name: "plasma-browser-".to_string(),
                    ppid: 4,
                    process_group: 5,
                    session: 5,
                    tty: 11,
                    foreground_process_group: 5,
                },
            ),
        ]);

        assert_eq!(
            find_terminal_leaf_with_stat_reader(1, &ppid_to_children, &pid_to_name, |pid| stats
                .get(&pid)
                .cloned(),),
            Some((4, "codex".to_string()))
        );
        assert_eq!(
            find_terminal_leaf_with_stat_reader(2, &ppid_to_children, &pid_to_name, |pid| stats
                .get(&pid)
                .cloned(),),
            Some((4, "codex".to_string()))
        );
    }

    #[test]
    fn spinner_is_not_duplicated_on_terminal_cwd_segment() {
        assert_eq!(
            terminal_display_title(
                "⠋ dictai - ⠋ ~ - Terminal",
                "codex",
                Some("codex resume --last"),
                Some("~"),
                None,
            ),
            "codex - ⠋ dictai - ~ - Terminal"
        );
    }

    #[test]
    fn spinner_on_cwd_basename_is_merged_into_full_cwd() {
        assert_eq!(
            terminal_display_title(
                "⠸ dictai - Terminal",
                "codex",
                Some("codex resume --last"),
                Some("~/Dev/sites/dictai"),
                None,
            ),
            "codex - ⠸ ~/Dev/sites/dictai - Terminal"
        );
    }

    #[test]
    fn action_required_status_is_not_duplicated_when_adding_cwd() {
        assert_eq!(
            terminal_display_title(
                "[ . ] Action Required | dictionary-extension - Terminal",
                "codex",
                Some("codex resume --last"),
                Some("~"),
                None,
            ),
            "codex - [ . ] Action Required - dictionary-extension - ~ - Terminal"
        );
    }

    #[test]
    fn codex_child_process_keeps_codex_in_terminal_title() {
        let process_chain = vec![
            ProcessChainEntry {
                pid: 20,
                name: "curl".to_string(),
                exe_path: None,
            },
            ProcessChainEntry {
                pid: 19,
                name: "codex-code-mode".to_string(),
                exe_path: None,
            },
        ];

        let parent_program = terminal_parent_program("curl", &process_chain);
        assert_eq!(parent_program, Some("codex"));
        assert_eq!(
            terminal_display_title(
                "codex - ~/Dev/applicationlauncher - Terminal",
                "curl",
                Some("curl https://example.com"),
                Some("~/Dev/applicationlauncher"),
                parent_program,
            ),
            "codex - curl - ~/Dev/applicationlauncher - Terminal"
        );
    }

    #[test]
    fn ssh_terminal_title_keeps_remote_shell_and_path() {
        let process_chain = vec![
            ProcessChainEntry {
                pid: 20,
                name: "ssh".to_string(),
                exe_path: Some(PathBuf::from("/usr/bin/ssh")),
            },
            ProcessChainEntry {
                pid: 19,
                name: "fish".to_string(),
                exe_path: Some(PathBuf::from("/usr/bin/fish")),
            },
        ];
        let parent_program = terminal_parent_program("ssh", &process_chain);

        assert_eq!(parent_program, Some("fish"));
        assert_eq!(
            terminal_display_title(
                "[lewis] /m/l/w/Program Files - Terminal",
                "ssh",
                Some("ssh lewis@192.168.50.30"),
                Some("~"),
                parent_program,
            ),
            "ssh - [lewis] fish - /m/l/w/Program Files - Terminal"
        );
    }

    #[test]
    fn terminal_leaf_filter_rejects_detached_browser_helpers() {
        let shell = parse_proc_stat("100 (fish) S 10 100 100 34840 200").unwrap();
        let codex = parse_proc_stat("200 (codex) S 100 200 100 34840 200").unwrap();
        let curl = parse_proc_stat("201 (curl) S 200 200 100 34840 200").unwrap();
        let browser = parse_proc_stat("300 (plasma-browser-) S 200 300 300 34841 300").unwrap();

        assert!(is_terminal_foreground_process(&codex, &shell));
        assert!(is_terminal_foreground_process(&curl, &shell));
        assert!(!is_terminal_foreground_process(&browser, &shell));
    }

    #[test]
    fn typo_highlight_for_mpv_has_visible_yellow_word() {
        let title = "what is the generic term for a movie and an episode - Google Search";
        let segments = title_highlight_segments(title, "mpv");

        assert!(
            segments
                .iter()
                .any(|(start, end, is_red)| !*is_red && &title[*start..*end] == "movie"),
            "expected mpv typo match to visibly highlight movie, got {segments:?}"
        );
    }

    #[test]
    fn focus_text_around_typo_match_does_not_panic() {
        let title = "python3 whisper_service.py faster-whisper | /home/lewis/Dev/assistant/.venv/bin/python3 whisper_service.py";
        let focused = focus_text_around_match(title, "whipser", None, None, 40);

        assert!(focused.contains("whisper"));
    }

    #[test]
    fn focused_title_uses_ranked_visible_match_for_mpv_gimp_match() {
        let title = "colour 8-bit non-linear integer, sRGB IEC61966-2.1, 1 layer, 3840x2160, imported image metadata for a screenshot edited in GIMP";
        let fields = [SearchField {
            priority: 0,
            value: "gimp",
        }];
        let candidate = MetadataCandidate {
            key: "gimp",
            fields: &fields,
            score: 0.0,
        };
        let rank = MetadataQuery::new("mpv")
            .unwrap()
            .with_typo_fallback(true)
            .search_rank(candidate)
            .unwrap();
        let focused = focus_text_around_match(title, "mpv", Some("gimp"), Some(&rank), 70);

        assert!(
            focused.to_lowercase().contains("gimp"),
            "expected focused title to include GIMP match, got {focused}"
        );
        let segments =
            title_highlight_segments_with_ranked_field(&focused, "mpv", Some("gimp"), Some(&rank));
        assert!(
            segments.iter().any(|(start, end, is_red)| !*is_red
                && focused[*start..*end].eq_ignore_ascii_case("gimp")),
            "expected focused title to visibly highlight GIMP, got {focused}"
        );
        assert!(
            segments
                .iter()
                .all(|(start, end, _)| end.saturating_sub(*start) < focused.len()),
            "should not highlight the whole focused title: {segments:?}"
        );
    }

    #[test]
    fn strikethrough_titles_match_plain_queries_and_keep_original_highlight_offsets() {
        for mark in ['\u{0335}', '\u{0336}'] {
            let struck: String = "codex".chars().flat_map(|ch| [ch, mark]).collect();
            let title = format!(
                "\u{2807} \u{3bb} {struck} ~/tasks/usbtree - Build USB speed diagnostic CLI - Terminal"
            );
            assert_eq!(search_projection(&struck), "codex");
            let values = vec![(0, title.clone())];
            let document =
                crate::search::PreparedSearchDocument::new("fixture".into(), values.clone());
            let (base, typo) = search_queries("codex").unwrap();
            assert!(base.search_rank_prepared(document.candidate(0.0)).is_some());
            assert_eq!(
                title_match_ranges(&title, "codex")
                    .iter()
                    .map(|(start, end)| &title[*start..*end])
                    .collect::<Vec<_>>(),
                [struck.as_str()]
            );
            let (_, display, segments, is_typo) =
                compute_display_title_and_highlights(&title, &values, &base, &typo, 512)
                    .expect("a plain query must match the decorated title itself");
            assert!(!is_typo);
            assert!(
                segments
                    .iter()
                    .any(|(start, end, exact)| *exact && display[*start..*end] == struck)
            );
            let job = highlighted_title_job_from_segments(&display, 14.0, &segments);
            assert_eq!(job.text, title.replace(mark, ""));
            assert!(
                job.sections
                    .iter()
                    .any(|section| &job.text[section.byte_range.clone()] == "codex"
                        && section.format.strikethrough.width > 0.0
                        && section.format.color == egui::Color32::from_rgb(235, 90, 90))
            );

            let (base, typo) = search_queries("usbtree").unwrap();
            let (_, display, segments, _) =
                compute_display_title_and_highlights(&title, &values, &base, &typo, 512).unwrap();
            assert!(
                segments
                    .iter()
                    .any(|(start, end, exact)| *exact && &display[*start..*end] == "usbtree")
            );
        }
    }

    #[test]
    fn strikethrough_title_typo_matches_remain_yellow() {
        let title = "c\u{0336}o\u{0336}d\u{0336}e\u{0336}x\u{0336} - Terminal";
        let values = vec![(0, title.into())];
        let (base, typo) = search_queries("codxe").unwrap();
        let (_, display, segments, is_typo) =
            compute_display_title_and_highlights(title, &values, &base, &typo, 512)
                .expect("typo fallback should see the undecorated program name");
        assert!(is_typo);
        let job = highlighted_title_job_from_segments(&display, 14.0, &segments);
        assert!(
            job.sections
                .iter()
                .any(|section| &job.text[section.byte_range.clone()] == "codex"
                    && section.format.strikethrough.width > 0.0
                    && section.format.color == egui::Color32::from_rgb(235, 196, 72))
        );
    }

    #[test]
    fn strikethrough_title_rendering_preserves_partial_colors_without_box_glyphs() {
        let struck = "c\u{0336}o\u{0336}d\u{0336}e\u{0336}x\u{0336}";
        let title = format!("\u{0336}prefix {struck} - \u{2807} ~/tasks/usbtree");
        let start = title.find('c').unwrap();
        let second = start + "c\u{0336}".len();
        let fourth = second + "o\u{0336}d\u{0336}".len();
        for segments in [
            vec![],
            vec![(start, start + 1, true), (second, fourth, false)],
        ] {
            let job = highlighted_title_job_from_segments(&title, 14.0, &segments);
            assert_eq!(job.text, "prefix codex - \u{2807} ~/tasks/usbtree");
            let strokes = job
                .sections
                .iter()
                .filter(|section| section.format.strikethrough.width > 0.0)
                .map(|section| (&job.text[section.byte_range.clone()], section.format.color))
                .collect::<Vec<_>>();
            if segments.is_empty() {
                assert_eq!(strokes, [("codex", egui::Color32::WHITE)]);
                assert_eq!(
                    job.sections.len(),
                    3,
                    "equal formatting must coalesce, not create a section per letter"
                );
            } else {
                assert_eq!(
                    strokes,
                    [
                        ("c", egui::Color32::from_rgb(235, 90, 90)),
                        ("od", egui::Color32::from_rgb(235, 196, 72)),
                        ("ex", egui::Color32::WHITE),
                    ]
                );
            }
            let ctx = egui::Context::default();
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.label(job.clone());
                });
            });
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.job.text == job.text)));
        }
    }

    #[test]
    fn history_titles_render_strikethrough_without_boxes_and_keep_popup_styling() {
        for mark in ['\u{0335}', '\u{0336}'] {
            let decorated: String = "Terminal"
                .chars()
                .flat_map(|character| [character, mark])
                .collect();
            let title = format!("codex - \u{2807} ~/tasks/film - {decorated}");
            let original = title.clone();
            let ctx = egui::Context::default();
            let font = egui::FontId::monospace(17.0);
            let color = egui::Color32::from_rgb(200, 180, 150);
            ctx.style_mut(|style| {
                style.override_font_id = Some(font.clone());
                style.visuals.override_text_color = Some(color);
            });
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    helpers::history_title_label(ui, &title);
                });
            });
            let job = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(&text.galley.job),
                    _ => None,
                })
                .expect("the history title must be painted");
            assert_eq!(job.text, "codex - \u{2807} ~/tasks/film - Terminal");
            let struck = job
                .sections
                .iter()
                .filter(|section| section.format.strikethrough.width > 0.0)
                .map(|section| &job.text[section.byte_range.clone()])
                .collect::<Vec<_>>();
            assert_eq!(struck, ["Terminal"]);
            assert!(
                job.sections
                    .iter()
                    .all(|section| section.format.font_id == font && section.format.color == color)
            );
            assert_eq!(
                title, original,
                "rendering must not rewrite the stored title"
            );
        }
    }

    #[test]
    fn history_titles_without_strikethrough_keep_their_original_text() {
        for title in [
            "~ - Terminal",
            "\u{2807} codex - ~/tasks/film - Terminal",
            "Caf\u{00e9} - e\u{0301} - Mousepad",
        ] {
            let ctx = egui::Context::default();
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    helpers::history_title_label(ui, title);
                });
            });
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.job.text == title
                    && text.galley.job.sections.iter().all(|section| section.format.strikethrough.width == 0.0)
            )));
        }
    }

    #[test]
    fn highlighted_title_job_ignores_invalid_ranges() {
        let title = "movie";
        let segments = [(0, title.len() + 10, false), (1, 3, false)];

        highlighted_title_job_from_segments(title, 12.0, &segments);
    }

    #[test]
    fn titleless_application_clients_are_not_listed_as_windows() {
        let mut icon_cache = HashMap::new();
        let ppid_to_children = HashMap::new();
        let pid_to_name = HashMap::new();
        let pid_to_ppid = HashMap::new();
        let window = build_window_info(
            "mousepad-internal-client".to_string(),
            String::new(),
            "Org.xfce.mousepad".to_string(),
            Some("org.xfce.mousepad".to_string()),
            None,
            None,
            Some(false),
            "breeze-dark",
            &mut icon_cache,
            &ppid_to_children,
            &pid_to_name,
            &pid_to_ppid,
            &[],
        );

        assert!(window.is_none());
    }

    #[test]
    fn compact_unregistered_chromium_extension_surfaces_are_not_listed() {
        let mut icon_cache = HashMap::new();
        let window = build_window_info(
            "extension-popup".to_string(),
            "transparency".to_string(),
            "chrome-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-Default".to_string(),
            Some("chrome-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-Default".to_string()),
            Some(1234),
            Some((0, 0, 150, 478)),
            Some(false),
            "breeze-dark",
            &mut icon_cache,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &[],
        );

        assert!(window.is_none());
    }

    #[test]
    fn test_compute_display_title_and_highlights_typo() {
        let base_query = MetadataQuery::new("fiom").unwrap();
        let typo_query = MetadataQuery::new("fiom").unwrap().with_typo_fallback(true);
        let search_values = vec![(0, "fish".to_string())];
        let (_rank, display_title, highlights, title_is_typo) =
            compute_display_title_and_highlights(
                "fish",
                &search_values,
                &base_query,
                &typo_query,
                70,
            )
            .unwrap();
        println!("display_title: {}", display_title);
        println!("highlights: {:?}", highlights);
        println!("title_is_typo: {}", title_is_typo);
        assert_eq!(display_title, "fish");
        assert_eq!(highlights, vec![(0, 4, false)]);
        assert!(title_is_typo);
    }

    #[test]
    fn settings_sanitize_nonfinite_and_out_of_range_values() {
        let settings = LauncherSettings {
            ui_scale: f32::NAN,
            win_row_height: 1_000.0,
            app_scroll_sensitivity: f32::NEG_INFINITY,
            info_scroll_sensitivity: f32::NAN,
            ..LauncherSettings::default()
        }
        .sanitized();

        assert_eq!(settings.ui_scale, LauncherSettings::default().ui_scale);
        assert_eq!(settings.win_row_height, 100.0);
        assert_eq!(
            settings.info_scroll_sensitivity,
            LauncherSettings::default().info_scroll_sensitivity
        );
        assert_eq!(
            settings.app_scroll_sensitivity,
            LauncherSettings::default().app_scroll_sensitivity
        );
    }

    #[test]
    fn search_projection_keeps_unicode_ranges_safe() {
        let projected = search_projection("GIMP — İmage");
        assert_eq!(projected.len(), "GIMP — İmage".chars().count());
        assert_eq!(projected, "gimp -  mage");
        assert!(search_queries("mpv").is_some());
    }
}
