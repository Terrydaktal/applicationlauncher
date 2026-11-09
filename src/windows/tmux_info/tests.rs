use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const FIXTURE: &[u8] = include_bytes!("../../../tests/fixtures/tmux-info.json");

fn target() -> Target {
    Target {
        socket: "/fixture/server.sock".into(),
        server: ProcessIdentity {
            pid: 20,
            start_ticks: 200,
        },
        executable_identity: None,
    }
}

#[test]
fn tmux_inventory_rejects_wrong_servers_sockets_malformed_and_oversized_records() {
    let inventory = Inventory::parse(FIXTURE, &target()).unwrap();
    assert_eq!(inventory.clients().len(), 2);
    assert_eq!(inventory.sessions()[1]["attached_clients"], 0);
    assert!(Inventory::parse(b"not json", &target()).is_err());
    assert!(Inventory::parse(&vec![b' '; MAX_JSON_BYTES], &target()).is_err());
    for key in ["clients", "sessions"] {
        let mut data = inventory.data.clone();
        let duplicate = data[key][0].clone();
        data[key].as_array_mut().unwrap().push(duplicate);
        assert!(Inventory::parse(&serde_json::to_vec(&data).unwrap(), &target()).is_err());
    }
    for (pointer, replacement) in [
        ("/socket", Value::from("/wrong/server.sock")),
        ("/server_identity/0", Value::from(21)),
        ("/server_identity/1", Value::from(201)),
        ("/server_running", Value::Bool(false)),
        ("/servers/tmux/pid", Value::from(21)),
        ("/servers/tmux/started_ticks", Value::from(201)),
        ("/clients", Value::Null),
        ("/clients/0/pid", Value::from(0)),
        ("/sessions/0/id", Value::Null),
        ("/sessions", Value::from(vec![Value::Null])),
        (
            "/clients",
            Value::from(vec![serde_json::json!({}); MAX_RECORDS + 1]),
        ),
    ] {
        let mut data = inventory.data.clone();
        *data.pointer_mut(pointer).unwrap() = replacement;
        assert!(
            Inventory::parse(&serde_json::to_vec(&data).unwrap(), &target()).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn tmux_client_inventory_never_matches_a_reused_ambiguous_or_missing_identity() {
    let mut inventory = Inventory::parse(FIXTURE, &target()).unwrap();
    let identity = ProcessIdentity {
        pid: 5,
        start_ticks: 500,
    };
    assert_eq!(inventory.client(identity).unwrap()["tty"], "/dev/pts/1");
    assert!(
        inventory
            .client(ProcessIdentity {
                start_ticks: 501,
                ..identity
            })
            .is_none()
    );
    assert!(
        inventory
            .client(ProcessIdentity { pid: 9, ..identity })
            .is_none()
    );
    let duplicate = inventory.data["clients"][0].clone();
    inventory.data["clients"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(inventory.client(identity).is_none());
}

#[test]
fn tmux_inventory_command_only_requests_json_for_the_exact_socket() {
    let command = inventory_command(
        Path::new("/fixture/tmux-mosh"),
        Path::new("/fixture/tmux binary"),
        Path::new("/fixture/server socket"),
    );
    assert_eq!(command.get_program(), "/fixture/tmux-mosh");
    assert_eq!(
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        [
            "--tmux",
            "/fixture/tmux binary",
            "--socket",
            "/fixture/server socket",
            "clients",
            "--json"
        ]
    );
    assert!(
        command
            .get_envs()
            .any(|(key, value)| key == "PYTHONPATH" && value.is_none())
    );
    assert!(
        collect(&Target {
            server: ProcessIdentity {
                pid: -1,
                start_ticks: 0
            },
            ..target()
        })
        .is_err()
    );
}

#[test]
fn tmux_inventory_lookup_coalesces_pending_requests_and_never_expires_completed_info() {
    let ctx = egui::Context::default();
    let mut target = target();
    target.socket = "/fixture/coalesced-no-live-IPC.sock".into();
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let (started_tx, started) = std::sync::mpsc::sync_channel(1);
    let (release, released) = std::sync::mpsc::sync_channel(1);
    let first = Lookup::start_with(
        target.clone(),
        ctx.clone(),
        egui::ViewportId::from_hash_of("one"),
        move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            started_tx.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(2)).unwrap();
            Err("fixture-only captured result".into())
        },
    );
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let second = Lookup::start_with(
        target.clone(),
        ctx.clone(),
        egui::ViewportId::from_hash_of("two"),
        |_| panic!("must reuse pending capture"),
    );
    assert!(Arc::ptr_eq(&first.capture, &second.capture));
    assert!(first.result().is_none() && second.result().is_none());
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while first.result().is_none() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    for _ in 0..100 {
        assert!(
            first
                .result()
                .unwrap()
                .unwrap_err()
                .contains("fixture-only")
        );
        assert!(second.result().is_some());
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
    // A newly opened Info gets a fresh capture, without replacing existing Info data.
    let calls = count.clone();
    let third = Lookup::start_with(target, ctx, egui::ViewportId::ROOT, move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Err("new fixture capture".into())
    });
    assert!(!Arc::ptr_eq(&first.capture, &third.capture));
    let deadline = Instant::now() + Duration::from_secs(2);
    while third.result().is_none() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(third.result().unwrap().unwrap_err().contains("new fixture"));
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(
        first
            .result()
            .unwrap()
            .unwrap_err()
            .contains("fixture-only")
    );
}
