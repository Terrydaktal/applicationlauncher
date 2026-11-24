use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use eframe::egui::ViewportId;

#[derive(Default)]
struct Requests {
    pending: Option<ViewportId>,
    running: bool,
}

impl Requests {
    fn enqueue(&mut self, id: ViewportId) -> bool {
        self.pending = Some(id);
        if self.running {
            false
        } else {
            self.running = true;
            true
        }
    }

    fn next(&mut self) -> Option<ViewportId> {
        let next = self.pending.take();
        if next.is_none() {
            self.running = false;
        }
        next
    }
}

pub(crate) fn app_id(pid: u32, id: ViewportId) -> String {
    if id == ViewportId::ROOT {
        "applicationlauncher".into()
    } else {
        format!("applicationlauncher-info-{pid}-{:016x}", id.0.value())
    }
}

fn drain_requests(requests: &Mutex<Requests>, mut activate: impl FnMut(ViewportId)) {
    loop {
        let next = requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .next();
        let Some(id) = next else { return };
        activate(id);
    }
}

pub(crate) fn request(id: ViewportId) {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return;
    }
    static REQUESTS: OnceLock<Mutex<Requests>> = OnceLock::new();
    let requests = REQUESTS.get_or_init(Mutex::default);
    if !requests
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .enqueue(id)
    {
        return;
    }
    // Winit's Wayland focus_window is a no-op. Run only on explicit reactivation,
    // with one worker and one latest-wins pending target, never during rendering.
    applicationlauncher::spawn_worker!("window-focus", move |worker| {
        let kdotool = applicationlauncher::process::kdotool_path();
        drain_requests(requests, |id| {
            applicationlauncher::observe! { worker.set_state(if id == ViewportId::ROOT {
                "raising-launcher-window"
            } else {
                "raising-info-window"
            }); }
            let activated = activate_with(std::process::id(), id, |arguments| {
                let mut command = Command::new(&kdotool);
                command.args(arguments);
                let output = applicationlauncher::process::output_with_timeout(
                    command,
                    Duration::from_secs(2),
                )
                .ok()?;
                output.status.success().then_some(output.stdout)
            });
            if !activated {
                applicationlauncher::observe! { applicationlauncher::observability::record(
                    applicationlauncher::observability::Event::new("window-focus", "failed")
                        .reason("target-absent-ambiguous-or-activation-failed"),
                ); }
            }
        });
    });
}

fn activate_with(
    pid: u32,
    id: ViewportId,
    mut run: impl FnMut(&[String]) -> Option<Vec<u8>>,
) -> bool {
    let search = [
        "search".to_owned(),
        "--all".into(),
        "--pid".into(),
        pid.to_string(),
        "--class".into(),
        format!("^{}$", app_id(pid, id)),
    ];
    let Some(output) = run(&search) else {
        return false;
    };
    if output.len() > 256 {
        return false;
    }
    let Some(text) = std::str::from_utf8(&output).ok() else {
        return false;
    };
    let mut ids = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let Some(id) = ids.next().filter(|_| ids.next().is_none()) else {
        return false;
    };
    let id = id
        .strip_prefix('{')
        .and_then(|id| id.strip_suffix('}'))
        .unwrap_or(id);
    if id.len() != 36
        || !id.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return false;
    }
    let id = format!("{{{id}}}");
    run(&[
        "windowstate".into(),
        "--remove".into(),
        "MINIMIZED".into(),
        id.clone(),
        "windowactivate".into(),
        id.clone(),
        "windowraise".into(),
        id,
    ])
    .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: &str = "{01234567-89ab-4cde-8f01-234567890abc}";

    #[test]
    fn launcher_activation_matches_only_its_pid_and_native_identity_not_title_substrings() {
        let mut calls = Vec::new();
        assert!(activate_with(42, ViewportId::ROOT, |args| {
            calls.push(args.to_vec());
            Some(if calls.len() == 1 {
                format!("{WINDOW}\n").into_bytes()
            } else {
                Vec::new()
            })
        }));
        assert_eq!(
            calls[0],
            [
                "search",
                "--all",
                "--pid",
                "42",
                "--class",
                "^applicationlauncher$"
            ]
            .map(str::to_owned)
        );
        assert_eq!(
            calls[1],
            [
                "windowstate",
                "--remove",
                "MINIMIZED",
                WINDOW,
                "windowactivate",
                WINDOW,
                "windowraise",
                WINDOW,
            ]
            .map(str::to_owned)
        );
        assert_eq!(calls.len(), 2);
        assert_ne!(
            app_id(42, ViewportId::ROOT),
            app_id(42, ViewportId::from_hash_of("info-a"))
        );
    }

    #[test]
    fn shortcut_activation_follows_in_flight_info_and_supersedes_older_pending_info() {
        let info = ViewportId::from_hash_of("info-a");
        let other_info = ViewportId::from_hash_of("info-b");
        let requests = Mutex::new(Requests::default());
        assert!(requests.lock().unwrap().enqueue(info));
        let mut activated = Vec::new();
        drain_requests(&requests, |id| {
            activated.push(id);
            if id == info {
                let mut pending = requests.lock().unwrap();
                assert!(!pending.enqueue(other_info));
                assert!(!pending.enqueue(ViewportId::ROOT));
            }
        });
        assert_eq!(activated, [info, ViewportId::ROOT]);
        assert!(!requests.lock().unwrap().running);

        assert!(requests.lock().unwrap().enqueue(other_info));
        drain_requests(&requests, |id| activated.push(id));
        assert_eq!(activated, [info, ViewportId::ROOT, other_info]);
    }

    #[test]
    fn foreground_activation_targets_only_the_exact_pid_and_info_identity_and_preserves_geometry() {
        let viewport = ViewportId::from_hash_of("info-a");
        assert_ne!(app_id(42, viewport), app_id(43, viewport));
        assert_ne!(
            app_id(42, viewport),
            app_id(42, ViewportId::from_hash_of("info-b"))
        );
        let mut calls = Vec::new();
        assert!(activate_with(42, viewport, |args| {
            calls.push(args.to_vec());
            Some(if calls.len() == 1 {
                format!("{WINDOW}\n").into_bytes()
            } else {
                Vec::new()
            })
        }));
        assert_eq!(
            calls,
            [
                vec![
                    "search".to_owned(),
                    "--all".into(),
                    "--pid".into(),
                    "42".into(),
                    "--class".into(),
                    format!("^{}$", app_id(42, viewport))
                ],
                [
                    "windowstate",
                    "--remove",
                    "MINIMIZED",
                    WINDOW,
                    "windowactivate",
                    WINDOW,
                    "windowraise",
                    WINDOW
                ]
                .map(str::to_owned)
                .to_vec(),
            ]
        );
    }

    #[test]
    fn missing_ambiguous_invalid_and_failed_searches_never_activate_an_arbitrary_window() {
        for output in [
            None,
            Some(Vec::new()),
            Some(b"not-a-window-id".to_vec()),
            Some(b"{01234567_89ab_4cde_8f01_234567890abc}".to_vec()),
            Some(b"0123456789ab4cde8f01234567890abc".to_vec()),
            Some(vec![255]),
            Some(format!("{WINDOW}\n{WINDOW}\n").into_bytes()),
            Some(vec![b'x'; 257]),
        ] {
            let mut calls = 0;
            assert!(!activate_with(
                42,
                ViewportId::from_hash_of("info-a"),
                |_| {
                    calls += 1;
                    output.clone()
                }
            ));
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn failed_activation_is_reported_without_retrying_or_focusing_other_windows() {
        let mut calls = 0;
        assert!(!activate_with(
            42,
            ViewportId::from_hash_of("info-a"),
            |_| {
                calls += 1;
                (calls == 1).then(|| WINDOW.as_bytes().to_vec())
            }
        ));
        assert_eq!(calls, 2);
    }

    #[test]
    fn foreground_requests_keep_only_the_latest_pending_target_and_stop_when_idle() {
        let a = ViewportId::from_hash_of("a");
        let b = ViewportId::from_hash_of("b");
        let c = ViewportId::from_hash_of("c");
        let mut requests = Requests::default();
        assert!(requests.enqueue(a));
        assert_eq!(requests.next(), Some(a));
        assert!(!requests.enqueue(b));
        assert!(!requests.enqueue(c));
        assert_eq!(requests.next(), Some(c));
        assert_eq!(requests.next(), None);
        assert!(!requests.running);
        assert!(requests.enqueue(b));
    }

    #[test]
    fn slow_activation_does_not_hold_the_request_lock_or_drop_the_latest_target() {
        let a = ViewportId::from_hash_of("a");
        let b = ViewportId::from_hash_of("b");
        let c = ViewportId::from_hash_of("c");
        let requests = Mutex::new(Requests::default());
        requests.lock().unwrap().enqueue(a);
        let mut activated = Vec::new();
        drain_requests(&requests, |id| {
            activated.push(id);
            let mut pending = requests
                .try_lock()
                .expect("GUI enqueue must not wait for compositor I/O");
            if id == a {
                assert!(!pending.enqueue(b));
                assert!(!pending.enqueue(c));
            }
        });
        assert_eq!(activated, [a, c]);
        assert!(!requests.lock().unwrap().running);
    }
}
