use super::*;
#[cfg(test)]
mod tests {
    use super::*;
    use fuzzy_rank::metadata::SearchField;

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
            pid: Some(1234),
            last_activated_at_ms: Some(0),
            activation_sequence: 1,
        }
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
            last_activated_at_ms: Some(0),
            activation_sequence: 1,
        }
    }

    #[test]
    fn terminal_attention_sends_every_due_window_once() {
        let now = Instant::now();
        let mut deadlines = HashMap::new();
        let mut handled = HashSet::new();
        let eligible_ids = (0..5)
            .map(|index| format!("terminal-{index}"))
            .collect::<HashSet<_>>();

        let (due, next) = update_terminal_attention_schedule(
            true,
            &eligible_ids,
            &mut deadlines,
            &mut handled,
            now,
        );
        assert!(due.is_empty());
        assert_eq!(next, Some(now + Duration::from_secs(5)));

        let (due, next) = update_terminal_attention_schedule(
            true,
            &eligible_ids,
            &mut deadlines,
            &mut handled,
            now + Duration::from_secs(5),
        );
        assert_eq!(due.len(), 5);
        assert!(next.is_none());
        assert!(handled.is_empty());
        handled.extend(due);

        let (due, _) = update_terminal_attention_schedule(
            true,
            &eligible_ids,
            &mut deadlines,
            &mut handled,
            now + Duration::from_secs(10),
        );
        assert!(due.is_empty());
    }

    #[test]
    fn terminal_attention_clear_cancels_and_rearms_delay() {
        let now = Instant::now();
        let mut deadlines = HashMap::new();
        let mut handled = HashSet::new();
        let terminal_id = "terminal".to_string();
        let eligible_ids = HashSet::from([terminal_id.clone()]);

        update_terminal_attention_schedule(true, &eligible_ids, &mut deadlines, &mut handled, now);
        let no_eligible_ids = HashSet::new();
        let (due, next) = update_terminal_attention_schedule(
            true,
            &no_eligible_ids,
            &mut deadlines,
            &mut handled,
            now + Duration::from_secs(4),
        );
        assert!(due.is_empty());
        assert!(next.is_none());

        update_terminal_attention_schedule(
            true,
            &eligible_ids,
            &mut deadlines,
            &mut handled,
            now + Duration::from_secs(10),
        );
        let (due, _) = update_terminal_attention_schedule(
            true,
            &eligible_ids,
            &mut deadlines,
            &mut handled,
            now + Duration::from_secs(15),
        );
        assert_eq!(due.len(), 1);
        assert_eq!(due[0], terminal_id);
    }

    #[test]
    fn terminal_attention_feed_preserves_back_to_back_prompt_generation() {
        let id = "test-window".to_string();
        let mut windows = HashMap::from([(
            id.clone(),
            test_kwin_payload("codex - [ ! ] Action Required - Terminal", true),
        )]);
        let mut feed_last_seen = HashMap::new();
        let mut deadlines = HashMap::from([(id.clone(), Instant::now())]);
        let mut handled = HashSet::from([id.clone()]);
        let mut exhausted = HashSet::from([id.clone()]);
        let mut retry_attempts = HashMap::from([(id.clone(), 2)]);
        let mut generations = HashMap::from([(id.clone(), 4)]);

        assert!(apply_terminal_attention_feed_upsert(
            // The terminal title can lag behind KWin's attention clear when the
            // next approval appears immediately.
            test_kwin_payload("codex - [ ! ] Action Required - Terminal", false),
            &mut windows,
            &mut feed_last_seen,
            &mut deadlines,
            &mut handled,
            &mut exhausted,
            &mut retry_attempts,
            &mut generations,
        ));
        assert!(!apply_terminal_attention_feed_upsert(
            test_kwin_payload("codex - [ . ] Action Required - Terminal", true),
            &mut windows,
            &mut feed_last_seen,
            &mut deadlines,
            &mut handled,
            &mut exhausted,
            &mut retry_attempts,
            &mut generations,
        ));

        assert!(deadlines.is_empty());
        assert!(handled.is_empty());
        assert!(exhausted.is_empty());
        assert!(retry_attempts.is_empty());
        assert!(!terminal_attention_attempt_is_current(&id, 4, &generations));
        assert!(terminal_attention_attempt_is_current(&id, 5, &generations));
        assert!(terminal_attention_payload_requires_attention(
            windows.get(&id).unwrap(),
            true,
        ));
    }

    #[test]
    fn terminal_attention_success_rechecks_a_continuous_back_to_back_prompt() {
        let id = "test-window".to_string();
        let payload = test_kwin_payload("codex - [ ! ] Action Required - Terminal", false);
        let now = Instant::now();
        let mut deadlines = HashMap::new();
        let mut handled = HashSet::from([id.clone()]);

        record_terminal_attention_success(&id, Some(&payload), &mut deadlines, &mut handled, now);

        assert!(!handled.contains(&id));
        assert_eq!(
            deadlines.get(&id),
            Some(&(now + Duration::from_secs(AUTO_SEND_ENTER_DELAY_SECS)))
        );

        let cleared = test_kwin_payload("codex - project - Terminal", false);
        record_terminal_attention_success(&id, Some(&cleared), &mut deadlines, &mut handled, now);
        assert!(handled.contains(&id));
        assert!(!deadlines.contains_key(&id));
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

    #[test]
    fn terminal_attention_relaunch_rearms_an_existing_prompt() {
        let id = "test-window".to_string();
        let windows = HashMap::from([(
            id.clone(),
            test_kwin_payload("codex - [ ! ] Action Required - Terminal", true),
        )]);
        let mut deadlines = HashMap::from([(id.clone(), Instant::now())]);
        let mut handled = HashSet::from([id.clone()]);
        let mut exhausted = HashSet::from([id.clone()]);
        let mut retry_attempts = HashMap::from([(id.clone(), 2)]);
        let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let in_flight = HashMap::from([(id.clone(), Arc::clone(&cancellation))]);
        let mut generations = HashMap::from([(id.clone(), 7)]);

        rearm_terminal_attention_automation(
            &windows,
            &mut deadlines,
            &mut handled,
            &mut exhausted,
            &mut retry_attempts,
            &in_flight,
            &mut generations,
        );

        assert!(deadlines.is_empty());
        assert!(handled.is_empty());
        assert!(exhausted.is_empty());
        assert!(retry_attempts.is_empty());
        assert!(cancellation.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(generations.get(&id), Some(&8));

        let now = Instant::now();
        let eligible = HashSet::from([id.clone()]);
        let (due, next) =
            update_terminal_attention_schedule(true, &eligible, &mut deadlines, &mut handled, now);
        assert!(due.is_empty());
        assert_eq!(next, Some(now + Duration::from_secs(5)));
    }

    #[test]
    fn terminal_attention_clear_cancels_pending_dbus_send() {
        let cancellation = std::sync::atomic::AtomicBool::new(false);
        assert!(!terminal_attention_send_is_cancelled(Some(&cancellation)));

        cancellation.store(true, std::sync::atomic::Ordering::Release);

        assert!(terminal_attention_send_is_cancelled(Some(&cancellation)));
        assert!(!terminal_attention_send_is_cancelled(None));
    }

    #[test]
    fn terminal_attention_worker_ignores_non_terminal_and_idle_payloads() {
        let payload =
            |id: &str, class: &str, title: &str, demands_attention: bool| KWinWindowPayload {
                id: id.to_string(),
                title: title.to_string(),
                class: class.to_string(),
                pid: 1,
                desktop_file_name: String::new(),
                x: 0,
                y: 0,
                width: 800,
                height: 600,
                minimized: true,
                demands_attention,
                last_activated_at_ms: None,
                activation_sequence: 0,
            };

        let feed_attention = payload(
            "terminal",
            "xfce4-terminal",
            "codex - project - Terminal",
            true,
        );
        assert!(terminal_attention_payload_requires_attention(
            &feed_attention,
            true,
        ));
        assert!(!terminal_attention_payload_requires_attention(
            &feed_attention,
            false,
        ));
        assert!(terminal_attention_payload_requires_attention(
            &payload(
                "terminal-title",
                "xfce4-terminal",
                "[ ! ] Action Required - Terminal",
                false,
            ),
            false,
        ));
        assert!(terminal_attention_payload_requires_attention(
            &payload(
                "terminal-title-fallback",
                "xfce4-terminal",
                "[ ! ] Action Required - Terminal",
                false,
            ),
            true,
        ));
        assert!(!terminal_attention_payload_requires_attention(
            &payload(
                "idle-terminal",
                "xfce4-terminal",
                "fish - ~ - Terminal",
                false,
            ),
            true,
        ));
        assert!(!terminal_attention_payload_requires_attention(
            &payload("browser", "firefox", "Action Required", true),
            true,
        ));
    }

    #[test]
    fn terminal_attention_retries_use_bounded_exponential_backoff() {
        assert_eq!(
            terminal_attention_retry_delay(1),
            Duration::from_millis(750)
        );
        assert_eq!(
            terminal_attention_retry_delay(2),
            Duration::from_millis(1500)
        );
        assert_eq!(
            terminal_attention_retry_delay(3),
            Duration::from_millis(3000)
        );
    }

    fn terminal_record(
        tab_uuid: &str,
        window_uuid: &str,
        title: &str,
        pty: &str,
    ) -> TerminalDbusRecord {
        TerminalDbusRecord {
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
    #[ignore = "requires a live KWin session, kdotool, and an XFCE4 Terminal window"]
    fn live_terminal_attention_reconciliation_recovers_kwin_captions() {
        let kdotool = get_kdotool_path();
        let output = Command::new(kdotool)
            .args(["search", "--classname", "xfce4-terminal"])
            .output()
            .expect("kdotool should find XFCE4 Terminal windows");
        assert!(output.status.success());
        let terminal_windows = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(|id| {
                let mut window = test_window_info("unknown - Terminal");
                window.id = id.to_string();
                (window.id.clone(), window)
            })
            .collect::<HashMap<_, _>>();
        assert!(!terminal_windows.is_empty());

        let expected_ids = terminal_windows.keys().cloned().collect::<HashSet<_>>();
        let shared_windows = Arc::new(std::sync::Mutex::new(terminal_windows));
        let mut payloads = HashMap::new();
        reconcile_terminal_attention_windows_from_kwin(&mut payloads, &shared_windows).unwrap();

        assert!(expected_ids.iter().all(|id| payloads.contains_key(id)));
        assert!(payloads.values().all(|payload| {
            is_xfce4_terminal_class(&payload.class) && !payload.title.trim().is_empty()
        }));
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
}
