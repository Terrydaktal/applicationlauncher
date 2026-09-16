//! Opt-in tests use a private PulseAudio daemon and null sinks, never the user's
//! audio server, output devices, browser, terminals or desktop session.

use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

struct PrivateServer {
    root: PathBuf,
    child: Child,
}

impl PrivateServer {
    fn start() -> Self {
        let binary = std::env::var_os("APPLICATIONLAUNCHER_TEST_PULSEAUDIO")
            .expect("set APPLICATIONLAUNCHER_TEST_PULSEAUDIO to a PulseAudio executable");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("al-private-audio-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let log = fs::File::create(root.join("server.log")).unwrap();
        let mut command = Command::new(binary);
        // A private audio socket is not enough: PulseAudio also tries to claim
        // a session-bus name. Do not contact the desktop bus or sibling fixtures.
        command.env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", root.join("no-session-bus").display()),
        );
        command.args(["-n", "--daemonize=no", "--use-pid-file=no", "--exit-idle-time=-1", "--disable-shm=yes", "--log-target=stderr", "--log-level=error"])
            .arg(format!("--load=module-native-protocol-unix socket={} auth-anonymous=1", root.join("native").display()))
            .arg("--load=module-null-sink sink_name=fixture_output rate=16000 channels=2")
            .arg("--load=module-null-sink sink_name=fixture_other rate=16000 channels=2")
            .arg("--load=module-sine-source source_name=fixture_microphone frequency=6500 rate=16000")
            .env("HOME", &root).env("PULSE_RUNTIME_PATH", root.join("runtime"))
            .env("PULSE_STATE_PATH", root.join("state"))
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
        if let Some(modules) = std::env::var_os("APPLICATIONLAUNCHER_TEST_PULSE_MODULES") {
            command.arg(format!(
                "--dl-search-path={}",
                PathBuf::from(modules).display()
            ));
        }
        let child = command.spawn().expect("start private PulseAudio");
        let mut server = Self { root, child };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !server.root.join("native").exists() {
            assert!(
                server.child.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "private audio server did not start: {}",
                fs::read_to_string(server.root.join("server.log")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        server
    }

    fn address(&self) -> CString {
        CString::new(format!("unix:{}", self.root.join("native").display())).unwrap()
    }

    fn stop(&mut self) {
        // Only this test's freshly spawned non-GUI child; no process-name signals.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for PrivateServer {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct Playback {
    stream: NonNull<pa::pa_stream>,
    phase: f32,
    frequency: f32,
    amplitude: f32,
}

impl Playback {
    fn new(engine: &Engine, name: &CStr, frequency: f32, amplitude: f32) -> Self {
        let spec = pa::pa_sample_spec {
            format: pa::PA_SAMPLE_FLOAT32NE,
            rate: SAMPLE_RATE,
            channels: 2,
        };
        let stream = NonNull::new(unsafe {
            pa::pa_stream_new(engine.context.as_ptr(), name.as_ptr(), &spec, null())
        })
        .unwrap();
        let attributes = pa::pa_buffer_attr {
            maxlength: BUFFER_BYTES,
            tlength: FRAGMENT_BYTES * 2,
            prebuf: 0,
            minreq: FRAGMENT_BYTES / 2,
            fragsize: u32::MAX,
        };
        assert_eq!(
            unsafe {
                pa::pa_stream_connect_playback(
                    stream.as_ptr(),
                    c"fixture_output".as_ptr(),
                    &attributes,
                    pa::PA_STREAM_ADJUST_LATENCY,
                    null(),
                    null_mut(),
                )
            },
            0
        );
        Self {
            stream,
            frequency,
            amplitude,
            phase: 0.0,
        }
    }

    fn index(&self) -> u32 {
        unsafe { pa::pa_stream_get_index(self.stream.as_ptr()) }
    }

    fn feed(&mut self) {
        unsafe {
            if pa::pa_stream_get_state(self.stream.as_ptr()) != pa::PA_STREAM_READY {
                return;
            }
            let length = pa::pa_stream_writable_size(self.stream.as_ptr());
            assert_ne!(length, usize::MAX);
            let frames = length.min(BUFFER_BYTES as usize) / FRAME_BYTES;
            if frames == 0 {
                return;
            }
            let mut pcm = Vec::with_capacity(frames * 2);
            for _ in 0..frames {
                let value = self.phase.sin() * self.amplitude;
                pcm.extend_from_slice(&[value, -value]);
                self.phase = (self.phase
                    + std::f32::consts::TAU * self.frequency / SAMPLE_RATE as f32)
                    % std::f32::consts::TAU;
            }
            assert_eq!(
                pa::pa_stream_write(
                    self.stream.as_ptr(),
                    pcm.as_ptr().cast(),
                    pcm.len() * 4,
                    None,
                    0,
                    pa::PA_SEEK_RELATIVE
                ),
                0
            );
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        unsafe {
            pa::pa_stream_disconnect(self.stream.as_ptr());
            pa::pa_stream_unref(self.stream.as_ptr());
        }
    }
}

fn pump_until(
    engine: &mut Engine,
    players: &mut [&mut Playback],
    mut ready: impl FnMut(&Engine) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        for player in &mut *players {
            player.feed();
        }
        engine.step().unwrap();
        if ready(engine) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "audio condition timed out: inputs={:?}, taps={}, visuals={:?}",
            engine.metadata,
            engine.taps.len(),
            engine.snapshot().visualizations
        );
    }
}

#[test]
#[ignore = "requires APPLICATIONLAUNCHER_TEST_PULSEAUDIO; creates a private null-sink server"]
fn isolated_server_stream_isolation_pause_move_and_disconnect() {
    let mut server = PrivateServer::start();
    let mut engine = Engine::connect(Some(&server.address())).unwrap();
    pump_until(&mut engine, &mut [], |engine| {
        engine.subscribed && engine.outputs.len() == 2
    });
    unsafe {
        check_operation(pa::pa_context_set_default_source(
            engine.context.as_ptr(),
            c"fixture_microphone".as_ptr(),
            None,
            null_mut(),
        ))
        .unwrap();
    }
    let mut bass = Playback::new(&engine, c"fixture-bass", 120.0, 0.5);
    let mut quiet = Playback::new(&engine, c"fixture-silent", 4000.0, 0.0);
    pump_until(&mut engine, &mut [&mut bass, &mut quiet], |engine| {
        engine.taps.len() == 2 && !engine.snapshot().visualizations.is_empty()
    });
    let bass_id = bass.index();
    let quiet_id = quiet.index();
    let deadline = Instant::now() + Duration::from_millis(350);
    while Instant::now() < deadline {
        bass.feed();
        quiet.feed();
        engine.step().unwrap();
        let update = engine.snapshot();
        assert!(
            !update.visualizations.contains_key(&quiet_id),
            "silent stream captured the other application or the fake microphone"
        );
    }
    let frame = engine.snapshot().visualizations[&bass_id];
    assert!(
        frame.bands[1] > frame.bands[6] + 25,
        "wrong stream spectrum: {frame:?}"
    );
    eprintln!(
        "isolated bass peak={}, bands={:?}; silent stream and default microphone excluded",
        frame.peak, frame.bands
    );

    // Silent retained playback streams must stop even without a cork/MPRIS change.
    bass.amplitude = 0.0;
    pump_until(&mut engine, &mut [&mut bass, &mut quiet], |engine| {
        !engine.snapshot().visualizations.contains_key(&bass_id)
    });
    quiet.amplitude = 0.5;
    pump_until(&mut engine, &mut [&mut bass, &mut quiet], |engine| {
        engine.snapshot().visualizations.contains_key(&quiet_id)
    });
    assert!(!engine.snapshot().visualizations.contains_key(&bass_id));
    let frame = engine.snapshot().visualizations[&quiet_id];
    assert!(
        frame.bands[6] > frame.bands[1] + 25,
        "treble did not replace bass: {frame:?}"
    );

    // Rapid back-to-back pause/resume must not inherit the failure retry delay.
    for _ in 0..2 {
        unsafe {
            check_operation(pa::pa_stream_cork(
                quiet.stream.as_ptr(),
                1,
                None,
                null_mut(),
            ))
            .unwrap();
        }
        pump_until(&mut engine, &mut [&mut bass, &mut quiet], |engine| {
            engine
                .inputs
                .get(&quiet_id)
                .is_some_and(|input| input.metadata.corked)
                && !engine.snapshot().visualizations.contains_key(&quiet_id)
        });
        let resumed = Instant::now();
        unsafe {
            check_operation(pa::pa_stream_cork(
                quiet.stream.as_ptr(),
                0,
                None,
                null_mut(),
            ))
            .unwrap();
        }
        pump_until(&mut engine, &mut [&mut bass, &mut quiet], |engine| {
            engine.snapshot().visualizations.contains_key(&quiet_id)
        });
        assert!(
            resumed.elapsed() < Duration::from_secs(1),
            "resume inherited retry delay"
        );
    }

    // A device move must reconnect to the new sink's monitor, never a default source.
    unsafe {
        check_operation(pa::pa_context_move_sink_input_by_name(
            engine.context.as_ptr(),
            quiet_id,
            c"fixture_other".as_ptr(),
            None,
            null_mut(),
        ))
        .unwrap();
    }
    pump_until(&mut engine, &mut [&mut bass, &mut quiet], |engine| {
        engine
            .taps
            .get(&quiet_id)
            .is_some_and(|tap| tap.output.monitor_name == "fixture_other.monitor")
            && engine.snapshot().visualizations.contains_key(&quiet_id)
    });

    unsafe {
        check_operation(pa::pa_context_set_sink_input_mute(
            engine.context.as_ptr(),
            quiet_id,
            1,
            None,
            null_mut(),
        ))
        .unwrap();
    }
    pump_until(&mut engine, &mut [&mut bass, &mut quiet], |engine| {
        engine
            .inputs
            .get(&quiet_id)
            .is_some_and(|input| input.metadata.mute)
            && !engine.snapshot().visualizations.contains_key(&quiet_id)
    });
    drop(quiet);
    pump_until(&mut engine, &mut [&mut bass], |engine| {
        !engine.inputs.contains_key(&quiet_id) && !engine.taps.contains_key(&quiet_id)
    });
    drop(bass);
    pump_until(&mut engine, &mut [], |engine| {
        engine.inputs.is_empty() && engine.taps.is_empty()
    });

    server.stop();
    let deadline = Instant::now() + Duration::from_secs(3);
    while engine.step().is_ok() {
        assert!(Instant::now() < deadline, "server disconnect not detected");
    }
    assert!(engine.snapshot().visualizations.is_empty());
}

#[test]
#[ignore = "explicit read-only live playback probe; requires APPLICATIONLAUNCHER_TEST_LIVE_AUDIO=1"]
fn live_playback_monitor_probe() {
    assert_eq!(
        std::env::var("APPLICATIONLAUNCHER_TEST_LIVE_AUDIO").as_deref(),
        Ok("1")
    );
    let mut engine = Engine::connect(None).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut seen = HashSet::new();
    let mut monitors = HashMap::<_, HashSet<u32>>::new();
    while Instant::now() < deadline {
        engine.step().unwrap();
        seen.extend(engine.snapshot().visualizations.keys().copied());
        for (index, tap) in &engine.taps {
            let monitor = unsafe { pa::pa_stream_get_index(tap.stream.as_ptr()) };
            if monitor != pa::PA_INVALID_INDEX {
                monitors
                    .entry((
                        *index,
                        tap.client,
                        tap.output.monitor_source,
                        tap.serial.clone(),
                    ))
                    .or_default()
                    .insert(monitor);
            }
        }
    }
    let ready = engine
        .taps
        .values()
        .filter(|tap| unsafe {
            pa::pa_stream_get_state(tap.stream.as_ptr()) == pa::PA_STREAM_READY
        })
        .count();
    eprintln!(
        "live playback probe: {} outputs, {} playback streams, {ready} ready individual monitors, {} streams with measured sound; no samples saved",
        engine.outputs.len(),
        engine.inputs.len(),
        seen.len()
    );
    assert!(engine.subscribed && !engine.outputs.is_empty());
    assert!(
        monitors.values().all(|ids| ids.len() == 1),
        "healthy monitor identities churned: {monitors:?}"
    );
    let eligible = engine
        .inputs
        .values()
        .filter(|input| {
            sink_input_can_visualize(&input.metadata) && engine.outputs.contains_key(&input.sink)
        })
        .count();
    assert_eq!(
        ready,
        eligible.min(MAX_STREAMS),
        "eligible playback monitors failed to connect"
    );
    let outputs = source_outputs(&mut engine);
    let own_indices: HashSet<_> = engine
        .taps
        .values()
        .map(|tap| unsafe { pa::pa_stream_get_index(tap.stream.as_ptr()) })
        .collect();
    let mut verified = 0;
    for (index, client, is_virtual) in outputs {
        if own_indices.contains(&index) {
            verified += 1;
            assert!(is_virtual, "playback meter must identify itself as virtual");
            assert_eq!(
                client,
                pa::PA_INVALID_INDEX,
                "PipeWire must export this as a virtual stream for Plasma, not a microphone client"
            );
        }
    }
    assert_eq!(verified, own_indices.len());
    assert!(verified > 0, "live verification requires a playback stream");
    eprintln!(
        "verified {verified} playback meters are virtual streams excluded by Plasma's microphone indicator"
    );

    if std::env::var("APPLICATIONLAUNCHER_TEST_FIREFOX_BRIDGE").as_deref() == Ok("1") {
        let ctx = egui::Context::default();
        let inbox = crate::audio::start_firefox_audio_bridge(ctx.clone());
        let deadline = Instant::now() + Duration::from_secs(2);
        let processes = loop {
            if let Some(update) = inbox.take_latest(&ctx) {
                assert!(
                    !update.processes.is_empty(),
                    "live Firefox bridge is unavailable or invalid"
                );
                break update.processes;
            }
            assert!(Instant::now() < deadline, "bridge reader did not respond");
            engine.step().unwrap();
        };
        let tracked = applicationlauncher::tracker::TrackerClient::connect()
            .unwrap()
            .windows()
            .unwrap();
        let windows: Vec<_> = tracked
            .iter()
            .filter(|win| processes.iter().any(|process| process.pid == win.pid))
            .map(|win| crate::audio::tests::firefox_window(&win.id, win.pid, &win.title))
            .collect();
        let snapshot = engine.snapshot();
        let mut cache = crate::audio::build_window_audio_cache(&windows, &snapshot.sink_inputs);
        crate::audio::apply_firefox_attribution(
            &mut cache,
            &windows,
            &snapshot.sink_inputs,
            &processes,
        );
        let expected: HashSet<_> = processes
            .iter()
            .flat_map(|process| process.windows.iter())
            .filter(|win| !win.audible.is_empty())
            .map(|win| &win.title)
            .collect();
        let measured: Vec<_> = windows
            .iter()
            .filter(|win| {
                cache
                    .visualization_sinks
                    .get(&win.id)
                    .is_some_and(|indices| {
                        indices
                            .iter()
                            .any(|index| snapshot.visualizations.contains_key(index))
                    })
            })
            .collect();
        assert!(
            !measured.is_empty(),
            "playing Firefox window has no measured waveform: expected={}, mapped={}, pcm={}",
            expected.len(),
            cache.visualization_sinks.len(),
            snapshot.visualizations.len()
        );
        assert!(
            measured.iter().all(|win| expected.contains(&win.title)),
            "waveform assigned to a non-playing window"
        );
        eprintln!(
            "live bridge: {} Firefox windows, {} with audible tabs, {} with measured waveforms; no quiet window marked",
            windows.len(),
            expected.len(),
            measured.len()
        );
    }
}

fn source_outputs(engine: &mut Engine) -> Vec<(u32, u32, bool)> {
    struct Query {
        done: bool,
        result: Vec<(u32, u32, bool)>,
    }
    extern "C" fn callback(
        _: *mut pa::pa_context,
        info: *const pa::pa_source_output_info,
        end: i32,
        data: *mut c_void,
    ) {
        let query = unsafe { &mut *data.cast::<Query>() };
        if end != 0 {
            query.done = true;
            return;
        }
        let info = unsafe { &*info };
        let virtual_value =
            unsafe { pa::pa_proplist_gets(info.proplist, c"node.virtual".as_ptr()) };
        let is_virtual =
            !virtual_value.is_null() && unsafe { CStr::from_ptr(virtual_value) } == c"true";
        query.result.push((info.index, info.client, is_virtual));
    }
    let mut query = Query {
        done: false,
        result: Vec::new(),
    };
    let operation = unsafe {
        pa::pa_context_get_source_output_info_list(
            engine.context.as_ptr(),
            Some(callback),
            (&mut query as *mut Query).cast(),
        )
    };
    assert!(!operation.is_null());
    let deadline = Instant::now() + Duration::from_secs(3);
    while !query.done && Instant::now() < deadline {
        engine.step().unwrap();
    }
    unsafe {
        pa::pa_operation_cancel(operation);
        pa::pa_operation_unref(operation);
    }
    assert!(query.done, "source-output query timed out");
    query.result
}

#[test]
#[ignore = "requires APPLICATIONLAUNCHER_TEST_PULSEAUDIO; private monitor lifetime regression"]
fn sink_input_changes_do_not_recreate_healthy_monitors() {
    let server = PrivateServer::start();
    let mut engine = Engine::connect(Some(&server.address())).unwrap();
    pump_until(&mut engine, &mut [], |engine| {
        engine.subscribed && !engine.outputs.is_empty()
    });
    let mut player = Playback::new(&engine, c"fixture-metadata-change", 440.0, 0.2);
    pump_until(&mut engine, &mut [&mut player], |engine| {
        !engine.snapshot().visualizations.is_empty()
    });
    let index = player.index();
    let monitor = unsafe { pa::pa_stream_get_index(engine.taps[&index].stream.as_ptr()) };
    for _ in 0..20 {
        // PipeWire emits this when attaching a playback monitor, without any
        // identity/device/mute change. Simulate that exact notification.
        subscription_event(
            null_mut(),
            pa::PA_SUBSCRIPTION_EVENT_SINK_INPUT | pa::PA_SUBSCRIPTION_EVENT_CHANGE,
            index,
            engine.userdata(),
        );
        player.feed();
        engine.step().unwrap();
        assert_eq!(
            engine
                .taps
                .get(&index)
                .map(|tap| unsafe { pa::pa_stream_get_index(tap.stream.as_ptr()) }),
            Some(monitor),
            "metadata notifications must not disconnect a healthy playback monitor"
        );
        pump_until(&mut engine, &mut [&mut player], |engine| {
            engine.query_started.is_none() && !engine.discovery.borrow().dirty
        });
    }
    assert_eq!(source_outputs(&mut engine).len(), 1);
}

#[test]
#[ignore = "requires APPLICATIONLAUNCHER_TEST_PULSEAUDIO; private creation/cancellation race test"]
fn cancelled_creating_monitors_do_not_leak_server_recorders() {
    let server = PrivateServer::start();
    let mut engine = Engine::connect(Some(&server.address())).unwrap();
    pump_until(&mut engine, &mut [], |engine| {
        engine.subscribed && engine.outputs.len() == 2
    });
    let mut player = Playback::new(&engine, c"fixture-retirement", 440.0, 0.0);
    pump_until(&mut engine, &mut [&mut player], |engine| {
        !engine.inputs.is_empty()
    });
    let input = engine.inputs.values().next().unwrap().clone();
    let output = engine.outputs[&input.sink].clone();
    for _ in 0..24 {
        let tap = Tap::new(engine.context, &input, &output).unwrap();
        assert!(tap.connecting());
        engine.retire(tap);
        pump_until(&mut engine, &mut [&mut player], |engine| {
            engine.retiring_taps.is_empty()
        });
    }
    assert_eq!(
        source_outputs(&mut engine).len(),
        engine.taps.len(),
        "retired monitors must be deleted at the server, not merely forgotten locally"
    );
}
