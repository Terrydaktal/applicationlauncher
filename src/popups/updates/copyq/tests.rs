use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::time::Instant;

fn state() -> State {
    State {
        process: ProcessIdentity {
            pid: 123,
            start_ticks: 456,
        },
        busy: false,
        result: None,
    }
}

fn popup() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("copyq-no-service-fixture")
}

fn frame(
    ctx: &egui::Context,
    state: &State,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Option<Action>) {
    let mut action = None;
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |_| {});
            action = dialog(ctx, popup(), state);
        },
    );
    (output, action)
}

fn settled(ctx: &egui::Context, state: &State) -> egui::FullOutput {
    for _ in 0..3 {
        assert!(frame(ctx, state, vec![]).1.is_none());
    }
    frame(ctx, state, vec![]).0
}

fn text(output: &egui::FullOutput) -> String {
    output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn position(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing {label}: {}", text(output)))
}

fn click(pos: egui::Pos2) -> Vec<egui::Event> {
    let button = |pressed| egui::Event::PointerButton {
        pos,
        pressed,
        button: egui::PointerButton::Primary,
        modifiers: egui::Modifiers::NONE,
    };
    vec![egui::Event::PointerMoved(pos), button(true), button(false)]
}

#[test]
fn copyq_dialog_requires_confirmation_and_explains_the_exact_service_restart() {
    for scale in [1.0, 2.0] {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(scale);
        let state = state();
        let output = settled(&ctx, &state);
        let content = text(&output);
        for expected in [
            "Update CopyQ",
            "Main CopyQ process: 123",
            "briefly close and reopen",
            "commands will be interrupted",
            "does not build or install",
            &format!("systemctl --user restart {SERVICE}"),
        ] {
            assert!(content.contains(expected), "missing {expected}: {content}");
        }
        assert_eq!(
            frame(&ctx, &state, click(position(&output, RESTART))).1,
            Some(Action::Restart)
        );
    }
}

#[test]
fn copyq_cancel_escape_backdrop_and_default_enter_never_confirm_a_restart() {
    for choice in ["Cancel", "Escape", "Backdrop", "Enter"] {
        let ctx = egui::Context::default();
        let state = state();
        let output = settled(&ctx, &state);
        let events = match choice {
            "Cancel" => click(position(&output, "Cancel")),
            "Backdrop" => click(egui::pos2(2.0, 2.0)),
            _ => vec![egui::Event::Key {
                key: if choice == "Escape" {
                    egui::Key::Escape
                } else {
                    egui::Key::Enter
                },
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        };
        assert_eq!(
            frame(&ctx, &state, events).1,
            Some(Action::Close),
            "{choice}"
        );
    }
}

#[test]
fn copyq_busy_success_and_failure_disable_repeated_restarts() {
    for (busy, result) in [
        (true, None),
        (false, Some(Ok(()))),
        (false, Some(Err("fixture failure".into()))),
    ] {
        let ctx = egui::Context::default();
        let state = State {
            busy,
            result,
            ..state()
        };
        let output = settled(&ctx, &state);
        assert!(
            frame(&ctx, &state, click(position(&output, RESTART)))
                .1
                .is_none()
        );
        let output = settled(&ctx, &state);
        assert_eq!(
            frame(&ctx, &state, click(position(&output, "Close"))).1,
            Some(Action::Close)
        );
    }
}

#[test]
fn copyq_opening_is_read_only_and_modal_state_belongs_to_its_info_window() {
    let ctx = egui::Context::default();
    let process = state().process;
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        assert!(
            super::super::show(
                ctx,
                popup(),
                Some(crate::models::InfoUpdateTarget::CopyQ(process))
            )
            .is_none()
        );
    });
    let first = ctx
        .data(|data| data.get_temp::<Shared>(state_id(popup())))
        .unwrap();
    assert!(!first.lock().unwrap().busy);
    assert!(first.lock().unwrap().result.is_none());
    let second_popup = egui::ViewportId::from_hash_of("other-copyq-info");
    open(
        &ctx,
        second_popup,
        ProcessIdentity {
            pid: 124,
            ..process
        },
    );
    let second = ctx
        .data(|data| data.get_temp::<Shared>(state_id(second_popup)))
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    first.lock().unwrap().busy = true;
    open(
        &ctx,
        popup(),
        ProcessIdentity {
            pid: 125,
            ..process
        },
    );
    assert!(Arc::ptr_eq(
        &first,
        &ctx.data(|data| data.get_temp::<Shared>(state_id(popup())))
            .unwrap()
    ));
    assert_eq!(first.lock().unwrap().process, process);
    assert_eq!(second.lock().unwrap().process.pid, 124);
}

#[test]
fn copyq_restart_uses_only_the_requested_user_service_without_a_shell() {
    let command = restart_command();
    assert_eq!(command.get_program(), "/usr/bin/systemctl");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        ["--user", "restart", SERVICE]
    );
    // Invalid process identities must fail before any command can be executed.
    for process in [
        ProcessIdentity {
            pid: -1,
            start_ticks: 0,
        },
        ProcessIdentity {
            pid: 1,
            start_ticks: 1,
        },
    ] {
        assert!(restart(process).unwrap_err().contains("changed or exited"));
    }
}

fn wait_until_finished(shared: &Shared) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while shared.lock().unwrap().busy {
        assert!(Instant::now() < deadline, "restart fixture did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn copyq_restart_worker_runs_once_and_never_queues_another_window_or_retries() {
    for succeeds in [true, false] {
        let ctx = egui::Context::default();
        let first = Arc::new(Mutex::new(state()));
        let second = Arc::new(Mutex::new(state()));
        let in_flight = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let worker_count = count.clone();
        let (started, has_started) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        request(
            first.clone(),
            ctx.clone(),
            popup(),
            in_flight.clone(),
            move |identity| {
                assert_eq!(identity, state().process);
                worker_count.fetch_add(1, Ordering::SeqCst);
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
                if succeeds {
                    Ok(())
                } else {
                    Err("fixture service failure".into())
                }
            },
        );
        has_started.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(first.lock().unwrap().busy);
        request(
            first.clone(),
            ctx.clone(),
            popup(),
            in_flight.clone(),
            |_| panic!("duplicate restart"),
        );
        request(
            second.clone(),
            ctx.clone(),
            popup(),
            in_flight.clone(),
            |_| panic!("queued restart"),
        );
        assert!(!second.lock().unwrap().busy);
        assert!(
            second
                .lock()
                .unwrap()
                .result
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap_err()
                .contains("No second restart was queued")
        );
        release.send(()).unwrap();
        wait_until_finished(&first);
        assert_eq!(
            first.lock().unwrap().result.as_ref().unwrap().is_ok(),
            succeeds
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while in_flight.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        request(first, ctx, popup(), in_flight, |_| {
            panic!("automatic retry")
        });
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
