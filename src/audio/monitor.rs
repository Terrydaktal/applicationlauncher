//! Per-sink-input PCM monitoring on one named worker. No default source, microphone,
//! mixed-output fallback, sample storage, GUI-thread I/O or growing event queue.

use super::analysis::{Analyzer, FRAME_BYTES, SAMPLE_RATE};
use super::sink_input_can_visualize;
use crate::models::{AudioCacheUpdate, PactlSinkInput, PactlVolumeChannel};
use eframe::egui;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{CStr, CString, c_void};
use std::ptr::{NonNull, null, null_mut};
use std::sync::{Arc, Mutex, TryLockError, Weak};
use std::time::{Duration, Instant};

use libpulse_sys as pa;

const MAX_STREAMS: usize = 32;
const MAX_METADATA: usize = 256;
const BUFFER_BYTES: u32 = SAMPLE_RATE / 10 * FRAME_BYTES as u32;
const FRAGMENT_BYTES: u32 = SAMPLE_RATE / 50 * FRAME_BYTES as u32;
const PUBLISH_INTERVAL: Duration = Duration::from_millis(40);
const STALE_SAMPLES: Duration = Duration::from_millis(160);
pub(crate) const STALE_AUDIO_UPDATE: Duration = Duration::from_millis(400);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(crate) struct AudioInbox(Mutex<Option<AudioCacheUpdate>>);

impl AudioInbox {
    pub(crate) fn take_latest(&self, ctx: &egui::Context) -> Option<AudioCacheUpdate> {
        match self.0.try_lock() {
            Ok(mut slot) => slot.take(),
            Err(TryLockError::WouldBlock) => {
                // Never wait on the GUI thread, but do not lose the final repaint
                // if the measured signal becomes constant during contention.
                ctx.request_repaint_after(PUBLISH_INTERVAL);
                None
            }
            Err(TryLockError::Poisoned(_)) => None,
        }
    }

    fn replace(&self, update: AudioCacheUpdate) {
        if let Ok(mut latest) = self.0.lock() {
            *latest = Some(update);
        }
    }
}

pub(crate) fn start_audio_monitor(ctx: egui::Context) -> Arc<AudioInbox> {
    let inbox = Arc::new(AudioInbox::default());
    let target = Arc::downgrade(&inbox);
    applicationlauncher::observability::spawn_named("audio-monitor", move |worker| {
        let mut backoff = Duration::from_secs(1);
        let mut last_error = None;
        while target.strong_count() > 0 {
            worker.set_state("connecting-playback-monitors");
            let result = Engine::connect(None).and_then(|mut engine| {
                worker.set_state("monitoring-playback-samples");
                let mut previous: Option<AudioCacheUpdate> = None;
                let mut published = Instant::now() - PUBLISH_INTERVAL;
                while target.strong_count() > 0 {
                    engine.step()?;
                    if published.elapsed() >= PUBLISH_INTERVAL {
                        let update = engine.snapshot();
                        let changed = previous.as_ref().is_none_or(|old| {
                            !Arc::ptr_eq(&old.sink_inputs, &update.sink_inputs)
                                || old.visualizations != update.visualizations
                        });
                        if !publish(&target, &ctx, update.clone(), changed) {
                            break;
                        }
                        previous = Some(update);
                        published = Instant::now();
                        if engine.subscribed && engine.started.elapsed() > Duration::from_secs(10) {
                            backoff = Duration::from_secs(1);
                            last_error = None;
                        }
                    }
                }
                Ok(())
            });
            if let Err(reason) = result {
                if last_error != Some(reason) {
                    eprintln!(
                        "Playback visualization unavailable: {reason}; retrying without capture fallback"
                    );
                    last_error = Some(reason);
                }
                worker.set_state("playback-monitor-retry");
                publish(
                    &target,
                    &ctx,
                    AudioCacheUpdate {
                        sink_inputs: Arc::default(),
                        visualizations: HashMap::new(),
                        captured_at: Instant::now(),
                    },
                    true,
                );
                // Retry indefinitely only while the GUI owns the inbox; wait is
                // capped, interruptible by owner drop and never on the GUI thread.
                let deadline = Instant::now() + backoff;
                while target.strong_count() > 0 && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(100));
                }
                backoff = (backoff * 2).min(Duration::from_secs(10));
            } else {
                break;
            }
        }
    });
    inbox
}

fn publish(
    target: &Weak<AudioInbox>,
    ctx: &egui::Context,
    update: AudioCacheUpdate,
    repaint: bool,
) -> bool {
    let Some(inbox) = target.upgrade() else {
        return false;
    };
    inbox.replace(update);
    if repaint {
        ctx.request_repaint();
    }
    true
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Output {
    monitor_source: u32,
    monitor_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Input {
    metadata: PactlSinkInput,
    sink: u32,
    client: u32,
}

#[derive(Default)]
struct Discovery {
    generation: u64,
    dirty: bool,
    invalidated: HashSet<u32>,
    invalidate_all: bool,
    subscribed: Option<bool>,
    query_generation: u64,
    inputs_done: bool,
    outputs_done: bool,
    failed: bool,
    inputs: BTreeMap<u32, Input>,
    outputs: HashMap<u32, Output>,
}

struct Tap {
    stream: NonNull<pa::pa_stream>,
    output: Output,
    client: u32,
    serial: Option<String>,
    analyzer: Analyzer,
    created: Instant,
    last_samples: Option<Instant>,
}

fn playback_monitor_properties() -> Result<NonNull<pa::pa_proplist>, &'static str> {
    let properties = NonNull::new(unsafe { pa::pa_proplist_new() })
        .ok_or("cannot allocate playback monitor properties")?;
    for (key, value) in [
        (c"application.id", c"com.terrydaktal.ApplicationLauncher"),
        (c"media.role", c"production"),
        // PipeWire exports virtual nodes without a recording client. Plasma's
        // microphone indicator excludes these, without hiding real recorders.
        (c"node.virtual", c"true"),
        (c"stream.capture.sink", c"true"),
    ] {
        if unsafe { pa::pa_proplist_sets(properties.as_ptr(), key.as_ptr(), value.as_ptr()) } < 0 {
            unsafe { pa::pa_proplist_free(properties.as_ptr()) };
            return Err("cannot label playback monitor");
        }
    }
    Ok(properties)
}

impl Tap {
    fn connecting(&self) -> bool {
        unsafe { pa::pa_stream_get_state(self.stream.as_ptr()) == pa::PA_STREAM_CREATING }
    }

    fn new(
        context: NonNull<pa::pa_context>,
        input: &Input,
        output: &Output,
    ) -> Result<Self, &'static str> {
        let device = CString::new(output.monitor_name.as_str())
            .map_err(|_| "invalid playback monitor name")?;
        let spec = pa::pa_sample_spec {
            format: pa::PA_SAMPLE_FLOAT32NE,
            rate: SAMPLE_RATE,
            channels: 2,
        };
        // All native objects live exclusively on this worker. Each tap owns its
        // stream, and the context/mainloop outlive all taps (see Engine::drop).
        let properties = playback_monitor_properties()?;
        let stream = NonNull::new(unsafe {
            pa::pa_stream_new_with_proplist(
                context.as_ptr(),
                c"Application Launcher playback meter".as_ptr(),
                &spec,
                null(),
                properties.as_ptr(),
            )
        });
        unsafe { pa::pa_proplist_free(properties.as_ptr()) };
        let stream = stream.ok_or("cannot allocate playback monitor")?;
        let tap = Self {
            stream,
            output: output.clone(),
            client: input.client,
            serial: input.metadata.properties.get("object.serial").cloned(),
            analyzer: Analyzer::default(),
            created: Instant::now(),
            last_samples: None,
        };
        let attributes = pa::pa_buffer_attr {
            maxlength: BUFFER_BYTES,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: FRAGMENT_BYTES,
        };
        unsafe {
            // This MUST precede connect_record. An error must never result in a
            // connection to an unfiltered sink monitor or the default microphone.
            if pa::pa_stream_set_monitor_stream(stream.as_ptr(), input.metadata.index) < 0 {
                return Err("server rejected individual playback monitoring");
            }
            let flags = pa::PA_STREAM_DONT_MOVE
                | pa::PA_STREAM_ADJUST_LATENCY
                | pa::PA_STREAM_DONT_INHIBIT_AUTO_SUSPEND
                | pa::PA_STREAM_START_UNMUTED;
            if pa::pa_stream_connect_record(stream.as_ptr(), device.as_ptr(), &attributes, flags)
                < 0
            {
                return Err("cannot connect individual playback monitor");
            }
        }
        Ok(tap)
    }

    fn read(&mut self, index: u32, now: Instant) -> bool {
        unsafe {
            match pa::pa_stream_get_state(self.stream.as_ptr()) {
                pa::PA_STREAM_READY => {}
                pa::PA_STREAM_CREATING | pa::PA_STREAM_UNCONNECTED => {
                    return now.duration_since(self.created) < REQUEST_TIMEOUT;
                }
                _ => return false,
            }
            if pa::pa_stream_get_monitor_stream(self.stream.as_ptr()) != index
                || pa::pa_stream_get_device_index(self.stream.as_ptr())
                    != self.output.monitor_source
            {
                return false;
            }
            let readable = pa::pa_stream_readable_size(self.stream.as_ptr());
            if readable == usize::MAX || readable > BUFFER_BYTES as usize {
                // Reconnect only this tap. Do not spend CPU analysing a backlog.
                return false;
            }
            // Discard backlog instead of replaying old music after worker stalls.
            let mut budget = BUFFER_BYTES as usize;
            for _ in 0..8 {
                let mut data = null();
                let mut length = 0;
                if pa::pa_stream_peek(self.stream.as_ptr(), &mut data, &mut length) < 0 {
                    return false;
                }
                if length == 0 {
                    break;
                }
                if data.is_null() {
                    self.analyzer.reset();
                    self.last_samples = None;
                } else if budget > 0 {
                    let bytes = std::slice::from_raw_parts(data.cast::<u8>(), length);
                    let length = length.min(budget);
                    let length = length - length % FRAME_BYTES;
                    self.analyzer.push_pcm(&bytes[bytes.len() - length..]);
                    budget -= length;
                    self.last_samples = Some(now);
                }
                if pa::pa_stream_drop(self.stream.as_ptr()) < 0 {
                    return false;
                }
            }
        }
        if self
            .last_samples
            .is_none_or(|time| now.duration_since(time) > STALE_SAMPLES)
        {
            self.analyzer.reset();
        }
        true
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        unsafe {
            pa::pa_stream_disconnect(self.stream.as_ptr());
            pa::pa_stream_unref(self.stream.as_ptr());
        }
    }
}

struct Engine {
    mainloop: NonNull<pa::pa_mainloop>,
    context: NonNull<pa::pa_context>,
    discovery: Box<RefCell<Discovery>>,
    taps: BTreeMap<u32, Tap>,
    retiring_taps: Vec<Tap>,
    inputs: BTreeMap<u32, Input>,
    outputs: HashMap<u32, Output>,
    retry_at: HashMap<u32, Instant>,
    metadata: Arc<Vec<PactlSinkInput>>,
    started: Instant,
    last_step: Instant,
    query_started: Option<Instant>,
    last_query: Instant,
    subscribed: bool,
    subscription_sent: bool,
}

impl Engine {
    fn connect(server: Option<&CStr>) -> Result<Self, &'static str> {
        let mainloop = NonNull::new(unsafe { pa::pa_mainloop_new() })
            .ok_or("cannot allocate audio mainloop")?;
        let context = NonNull::new(unsafe {
            pa::pa_context_new(
                pa::pa_mainloop_get_api(mainloop.as_ptr()),
                c"Application Launcher playback visualization".as_ptr(),
            )
        });
        let Some(context) = context else {
            unsafe {
                pa::pa_mainloop_free(mainloop.as_ptr());
            }
            return Err("cannot allocate audio context");
        };
        let engine = Self {
            mainloop,
            context,
            discovery: Box::new(RefCell::new(Discovery {
                dirty: true,
                ..Discovery::default()
            })),
            taps: BTreeMap::new(),
            retiring_taps: Vec::new(),
            inputs: BTreeMap::new(),
            outputs: HashMap::new(),
            retry_at: HashMap::new(),
            metadata: Arc::default(),
            started: Instant::now(),
            last_step: Instant::now(),
            query_started: None,
            last_query: Instant::now() - Duration::from_secs(30),
            subscribed: false,
            subscription_sent: false,
        };
        unsafe {
            if pa::pa_context_connect(
                context.as_ptr(),
                server.map_or(null(), CStr::as_ptr),
                pa::PA_CONTEXT_NOAUTOSPAWN,
                null(),
            ) < 0
            {
                return Err("audio server connection failed");
            }
        }
        Ok(engine)
    }

    fn userdata(&self) -> *mut c_void {
        // The boxed RefCell has a stable address until after context disconnection.
        (&*self.discovery as *const RefCell<Discovery>)
            .cast_mut()
            .cast()
    }

    fn step(&mut self) -> Result<(), &'static str> {
        if !self.taps.is_empty() && self.last_step.elapsed() > STALE_AUDIO_UPDATE {
            for (_, tap) in std::mem::take(&mut self.taps) {
                self.retire(tap);
            }
            self.retry_at.clear();
        }
        self.last_step = Instant::now();
        unsafe {
            let timeout = if self.taps.is_empty() {
                100_000
            } else {
                10_000
            };
            if pa::pa_mainloop_prepare(self.mainloop.as_ptr(), timeout) < 0
                || pa::pa_mainloop_poll(self.mainloop.as_ptr()) < 0
                || pa::pa_mainloop_dispatch(self.mainloop.as_ptr()) < 0
            {
                return Err("audio mainloop failed");
            }
            match pa::pa_context_get_state(self.context.as_ptr()) {
                pa::PA_CONTEXT_READY => {}
                pa::PA_CONTEXT_FAILED | pa::PA_CONTEXT_TERMINATED => {
                    return Err("audio server disconnected");
                }
                _ if self.started.elapsed() > REQUEST_TIMEOUT => {
                    return Err("audio server connection timed out");
                }
                _ => return Ok(()),
            }
            if !self.subscription_sent {
                pa::pa_context_set_subscribe_callback(
                    self.context.as_ptr(),
                    Some(subscription_event),
                    self.userdata(),
                );
                check_operation(pa::pa_context_subscribe(
                    self.context.as_ptr(),
                    pa::PA_SUBSCRIPTION_MASK_SINK_INPUT | pa::PA_SUBSCRIPTION_MASK_SINK,
                    Some(subscription_ready),
                    self.userdata(),
                ))?;
                self.subscription_sent = true;
            }
        }
        // A creating stream has no server channel to disconnect yet. Retain it
        // until creation completes, then disconnect, or tear down the context on
        // timeout. The active + retiring population never exceeds MAX_STREAMS.
        self.retiring_taps.retain(Tap::connecting);
        if self
            .retiring_taps
            .iter()
            .any(|tap| tap.created.elapsed() > REQUEST_TIMEOUT)
        {
            return Err("retired playback monitor creation timed out");
        }
        match self.discovery.borrow().subscribed {
            Some(true) => self.subscribed = true,
            Some(false) => return Err("audio subscription rejected"),
            None if self.started.elapsed() > REQUEST_TIMEOUT => {
                return Err("audio subscription timed out");
            }
            None => return Ok(()),
        }
        if let Some(started) = self.query_started {
            if started.elapsed() > REQUEST_TIMEOUT {
                return Err("audio discovery timed out");
            }
            let mut discovery = self.discovery.borrow_mut();
            if discovery.inputs_done && discovery.outputs_done {
                if discovery.failed {
                    return Err("audio discovery failed or exceeded its bounds");
                }
                if discovery.query_generation == discovery.generation {
                    self.inputs = std::mem::take(&mut discovery.inputs);
                    self.outputs = std::mem::take(&mut discovery.outputs);
                    discovery.invalidated.clear();
                    discovery.invalidate_all = false;
                    let metadata: Vec<_> = self
                        .inputs
                        .values()
                        .map(|input| input.metadata.clone())
                        .collect();
                    if *self.metadata != metadata {
                        self.metadata = Arc::new(metadata);
                    }
                } else {
                    discovery.dirty = true;
                }
                self.query_started = None;
            }
        }
        if self.query_started.is_none()
            && (self.discovery.borrow().dirty
                || self.last_query.elapsed() > Duration::from_secs(30))
        {
            self.start_query()?;
        }
        let now = Instant::now();
        let discovery = self.discovery.borrow();
        let mut remove = Vec::new();
        for (index, tap) in &mut self.taps {
            let same_target = !discovery.invalidate_all
                && !discovery.invalidated.contains(index)
                && self.inputs.get(index).is_some_and(|input| {
                    sink_input_can_visualize(&input.metadata)
                        && input.client == tap.client
                        && input.metadata.properties.get("object.serial") == tap.serial.as_ref()
                        && self.outputs.get(&input.sink) == Some(&tap.output)
                });
            if !same_target {
                remove.push(*index);
            } else if !tap.read(*index, now) {
                self.retry_at.insert(*index, now + REQUEST_TIMEOUT);
                remove.push(*index);
            }
        }
        for index in remove {
            if let Some(tap) = self.taps.remove(&index)
                && tap.connecting()
            {
                self.retiring_taps.push(tap);
            }
        }
        self.retry_at
            .retain(|index, _| self.inputs.contains_key(index));
        for (index, input) in &self.inputs {
            if self.taps.len() + self.retiring_taps.len() >= MAX_STREAMS {
                break;
            }
            if self.taps.contains_key(index)
                || discovery.invalidate_all
                || discovery.invalidated.contains(index)
                || !sink_input_can_visualize(&input.metadata)
                || self
                    .retry_at
                    .get(index)
                    .is_some_and(|deadline| *deadline > now)
            {
                continue;
            }
            let Some(output) = self.outputs.get(&input.sink) else {
                continue;
            };
            match Tap::new(self.context, input, output) {
                Ok(tap) => {
                    // A successful tap must not delay a subsequent pause/resume
                    // or device change. Back off failures only.
                    self.retry_at.remove(index);
                    self.taps.insert(*index, tap);
                }
                Err(_) => {
                    self.retry_at.insert(*index, now + REQUEST_TIMEOUT);
                }
            }
        }
        Ok(())
    }

    fn retire(&mut self, tap: Tap) {
        if tap.connecting() {
            self.retiring_taps.push(tap);
        }
    }

    fn start_query(&mut self) -> Result<(), &'static str> {
        {
            let mut discovery = self.discovery.borrow_mut();
            discovery.query_generation = discovery.generation;
            discovery.dirty = false;
            discovery.inputs_done = false;
            discovery.outputs_done = false;
            discovery.failed = false;
            discovery.inputs.clear();
            discovery.outputs.clear();
        }
        self.query_started = Some(Instant::now());
        self.last_query = Instant::now();
        unsafe {
            check_operation(pa::pa_context_get_sink_input_info_list(
                self.context.as_ptr(),
                Some(input_info),
                self.userdata(),
            ))?;
            check_operation(pa::pa_context_get_sink_info_list(
                self.context.as_ptr(),
                Some(output_info),
                self.userdata(),
            ))?;
        }
        Ok(())
    }

    fn snapshot(&self) -> AudioCacheUpdate {
        AudioCacheUpdate {
            sink_inputs: Arc::clone(&self.metadata),
            captured_at: Instant::now(),
            visualizations: self
                .taps
                .iter()
                .filter_map(|(index, tap)| {
                    let visual = tap.analyzer.visual();
                    (visual.peak > 0
                        && tap
                            .last_samples
                            .is_some_and(|time| time.elapsed() <= STALE_SAMPLES))
                    .then_some((*index, visual))
                })
                .collect(),
        }
    }
}

#[cfg(test)]
#[path = "monitor_integration_tests.rs"]
mod integration_tests;

impl Drop for Engine {
    fn drop(&mut self) {
        self.taps.clear();
        self.retiring_taps.clear();
        // Disconnect cancels pending native operations before their userdata is
        // freed. Callbacks borrow one stable state; there are no per-request boxes
        // to leak when a server disconnects mid-query.
        unsafe {
            pa::pa_context_set_subscribe_callback(self.context.as_ptr(), None, null_mut());
            pa::pa_context_disconnect(self.context.as_ptr());
            pa::pa_context_unref(self.context.as_ptr());
            pa::pa_mainloop_free(self.mainloop.as_ptr());
        }
    }
}

unsafe fn check_operation(operation: *mut pa::pa_operation) -> Result<(), &'static str> {
    if operation.is_null() {
        return Err("audio request rejected");
    }
    unsafe {
        pa::pa_operation_unref(operation);
    }
    Ok(())
}

// These callbacks run only inside this worker's mainloop dispatch. No GUI state is
// referenced, and no borrow of Discovery is held while the mainloop is dispatched.
extern "C" fn subscription_ready(_: *mut pa::pa_context, success: i32, data: *mut c_void) {
    unsafe { &*data.cast::<RefCell<Discovery>>() }
        .borrow_mut()
        .subscribed = Some(success != 0);
}

extern "C" fn subscription_event(
    _: *mut pa::pa_context,
    event: pa::pa_subscription_event_type_t,
    index: u32,
    data: *mut c_void,
) {
    let mut state = unsafe { &*data.cast::<RefCell<Discovery>>() }.borrow_mut();
    state.generation = state.generation.wrapping_add(1);
    state.dirty = true;
    // Attaching/detaching a monitor itself emits CHANGE on PipeWire. Refresh
    // metadata, then compare identity/device/mute state in step(); replacing a
    // tap merely because of CHANGE creates a self-sustaining graph-edit loop.
    // NEW/REMOVE still invalidate immediately to protect against index reuse.
    if event & pa::PA_SUBSCRIPTION_EVENT_FACILITY_MASK == pa::PA_SUBSCRIPTION_EVENT_SINK_INPUT
        && event & pa::PA_SUBSCRIPTION_EVENT_TYPE_MASK != pa::PA_SUBSCRIPTION_EVENT_CHANGE
    {
        if state.invalidated.len() < MAX_METADATA {
            state.invalidated.insert(index);
        } else {
            state.invalidate_all = true;
        }
    }
}

extern "C" fn input_info(
    _: *mut pa::pa_context,
    info: *const pa::pa_sink_input_info,
    eol: i32,
    data: *mut c_void,
) {
    let mut state = unsafe { &*data.cast::<RefCell<Discovery>>() }.borrow_mut();
    if eol != 0 {
        state.inputs_done = true;
        state.failed |= eol < 0;
        return;
    }
    let Some(info) = (unsafe { info.as_ref() }) else {
        state.failed = true;
        return;
    };
    if state.inputs.len() >= MAX_METADATA {
        state.failed = true;
        return;
    }
    let mut properties = HashMap::new();
    for key in [
        c"application.id",
        c"application.name",
        c"application.icon_name",
        c"application.process.id",
        c"application.process.binary",
        c"media.name",
        c"media.category",
        c"media.class",
        c"window.id",
        c"window.name",
        c"object.id",
        c"object.serial",
    ] {
        let value = unsafe { pa::pa_proplist_gets(info.proplist, key.as_ptr()) };
        if let Some(value) = unsafe { bounded_string(value) } {
            properties.insert(key.to_string_lossy().into_owned(), value);
        }
    }
    let volume = info
        .volume
        .values
        .iter()
        .take(if info.has_volume != 0 {
            usize::from(info.volume.channels).min(32)
        } else {
            0
        })
        .enumerate()
        .map(|(index, value)| {
            (
                index.to_string(),
                PactlVolumeChannel {
                    value_percent: format!(
                        "{:.3}%",
                        f64::from(*value) * 100.0 / f64::from(pa::PA_VOLUME_NORM)
                    ),
                },
            )
        })
        .collect();
    state.inputs.insert(
        info.index,
        Input {
            sink: info.sink,
            client: info.client,
            metadata: PactlSinkInput {
                index: info.index,
                corked: info.corked != 0,
                mute: info.mute != 0,
                volume,
                properties,
            },
        },
    );
}

extern "C" fn output_info(
    _: *mut pa::pa_context,
    info: *const pa::pa_sink_info,
    eol: i32,
    data: *mut c_void,
) {
    let mut state = unsafe { &*data.cast::<RefCell<Discovery>>() }.borrow_mut();
    if eol != 0 {
        state.outputs_done = true;
        state.failed |= eol < 0;
        return;
    }
    let Some(info) = (unsafe { info.as_ref() }) else {
        state.failed = true;
        return;
    };
    if state.outputs.len() >= MAX_METADATA {
        state.failed = true;
        return;
    }
    if info.monitor_source != pa::PA_INVALID_INDEX
        && let Some(name) = unsafe { bounded_string(info.monitor_source_name) }
        && !name.is_empty()
    {
        state.outputs.insert(
            info.index,
            Output {
                monitor_source: info.monitor_source,
                monitor_name: name,
            },
        );
    }
}

unsafe fn bounded_string(value: *const std::ffi::c_char) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let length = unsafe { libc::strnlen(value, 4096) };
    if length >= 4096 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(value.cast::<u8>(), length) };
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AudioVisualization;

    #[test]
    fn inbox_keeps_only_the_latest_sample_even_when_gui_is_suspended() {
        let inbox = AudioInbox::default();
        let ctx = egui::Context::default();
        for peak in 1..=100 {
            inbox.replace(AudioCacheUpdate {
                sink_inputs: Arc::default(),
                captured_at: Instant::now(),
                visualizations: HashMap::from([(
                    7,
                    AudioVisualization {
                        peak,
                        bands: [peak; 8],
                    },
                )]),
            });
        }
        assert_eq!(
            inbox.take_latest(&ctx).unwrap().visualizations[&7].peak,
            100
        );
        assert!(inbox.take_latest(&ctx).is_none());
    }

    #[test]
    fn gui_never_waits_for_the_audio_slot() {
        let inbox = AudioInbox::default();
        let guard = inbox.0.lock().unwrap();
        assert!(inbox.take_latest(&egui::Context::default()).is_none());
        drop(guard);
    }

    #[test]
    fn playback_meters_are_virtual_not_microphone_clients_or_impersonated_apps() {
        let properties = playback_monitor_properties().unwrap();
        unsafe {
            for (key, expected) in [
                (c"node.virtual", c"true"),
                (c"stream.capture.sink", c"true"),
                (c"application.id", c"com.terrydaktal.ApplicationLauncher"),
            ] {
                let value = pa::pa_proplist_gets(properties.as_ptr(), key.as_ptr());
                assert!(!value.is_null());
                assert_eq!(CStr::from_ptr(value), expected);
            }
            pa::pa_proplist_free(properties.as_ptr());
        }
    }

    #[test]
    fn change_events_refresh_metadata_without_invalidating_monitors() {
        let state = RefCell::new(Discovery::default());
        let userdata = (&state as *const RefCell<Discovery>).cast_mut().cast();
        subscription_event(
            null_mut(),
            pa::PA_SUBSCRIPTION_EVENT_SINK_INPUT | pa::PA_SUBSCRIPTION_EVENT_CHANGE,
            7,
            userdata,
        );
        let state = state.borrow();
        assert!(state.dirty);
        assert_eq!(state.generation, 1);
        assert!(
            state.invalidated.is_empty(),
            "attaching a monitor itself changes its sink input; invalidation creates a feedback loop"
        );
    }

    #[test]
    fn remove_and_recreate_invalidates_a_reused_sink_input_index() {
        let state = RefCell::new(Discovery::default());
        let userdata = (&state as *const RefCell<Discovery>).cast_mut().cast();
        for kind in [
            pa::PA_SUBSCRIPTION_EVENT_REMOVE,
            pa::PA_SUBSCRIPTION_EVENT_NEW,
        ] {
            subscription_event(
                null_mut(),
                pa::PA_SUBSCRIPTION_EVENT_SINK_INPUT | kind,
                7,
                userdata,
            );
        }
        assert!(state.borrow().invalidated.contains(&7));
    }

    #[test]
    fn subscription_invalidation_stays_bounded_during_an_event_storm() {
        let state = RefCell::new(Discovery::default());
        let userdata = (&state as *const RefCell<Discovery>).cast_mut().cast();
        for index in 0..MAX_METADATA as u32 * 4 {
            subscription_event(
                null_mut(),
                pa::PA_SUBSCRIPTION_EVENT_SINK_INPUT,
                index,
                userdata,
            );
        }
        let state = state.borrow();
        assert_eq!(state.invalidated.len(), MAX_METADATA);
        assert!(state.invalidate_all && state.dirty);
        assert_eq!(state.generation, MAX_METADATA as u64 * 4);
    }
}
