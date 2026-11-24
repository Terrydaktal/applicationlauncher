//! Slow build-input inspection never occupies the process metadata worker.
use super::*;

pub(super) struct Request {
    pub generation: u64,
    pub revision: u64,
    pub identity: ProcessIdentity,
    pub executable_identity: Option<(u64, u64)>,
    pub target: ProcessVersionTarget,
    pub check: Arc<super::super::freshness::Check>,
}

pub(super) fn inspect(request: &Request) -> String {
    let current = || {
        process_identity_is_current(request.identity, request.executable_identity)
            && (request
                .target
                .launch_context
                .executable_unavailable
                .is_none()
                || super::super::process_argv0(request.identity.pid)
                    == request.target.launch_context.raw_argv0)
    };
    if !current() {
        return "Unknown (process exited or changed before build-input inspection)".into();
    }
    let result = request.check.inspect();
    if current() {
        result
    } else {
        "Unknown (process exited or changed during build-input inspection)".into()
    }
}

pub(super) fn enqueue(state: &mut State, request: &super::Request, version: &mut ProcessVersion) {
    let check = version.source_check.take();
    if version.uncompiled_changes.as_deref() != Some(super::super::freshness::PENDING) {
        return;
    }
    let Some(check) = check else {
        version.uncompiled_changes =
            Some("Unknown (selected build-input image unavailable)".into());
        return;
    };
    if state.sources.len() >= MAX_PENDING {
        version.uncompiled_changes =
            Some("Unknown (build-input queue full; refresh this process to retry)".into());
        return;
    }
    state.sources.push_back(Request {
        generation: request.generation,
        revision: request.revision,
        identity: request.identity,
        executable_identity: request.executable_identity,
        target: request.target.clone(),
        check,
    });
}

pub(super) fn start(
    shared: Arc<(Mutex<State>, Condvar)>,
    ctx: egui::Context,
    viewport_id: egui::ViewportId,
    probe: impl Fn(&Request) -> String + Send + 'static,
) {
    applicationlauncher::spawn_worker!("process-info-source-checks", move |worker| {
        loop {
            applicationlauncher::observe! { worker.set_state("waiting-for-build-input-request"); }
            let request = {
                let (mutex, wake) = &*shared;
                let mut state = mutex.lock().unwrap_or_else(|error| error.into_inner());
                while !state.stopped && state.sources.is_empty() {
                    state = wake.wait(state).unwrap_or_else(|error| error.into_inner());
                }
                if state.stopped {
                    break;
                }
                state.sources.pop_front().unwrap()
            };
            applicationlauncher::observe! { worker.set_state("checking-recorded-build-inputs"); }
            let result = request.check.validated(|| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| probe(&request)))
                    .unwrap_or_else(|_| "Unknown (build-input check worker failed)".into())
            });
            let publish = {
                let mut state = shared.0.lock().unwrap_or_else(|error| error.into_inner());
                if state.stopped {
                    break;
                }
                if state.generation != request.generation
                    || state.active_revisions.get(&request.identity) != Some(&request.revision)
                {
                    continue;
                }
                if let Some(Some(version)) = state.results.get_mut(&request.identity)
                    && version.uncompiled_changes.as_deref()
                        == Some(super::super::freshness::PENDING)
                {
                    // Other workers may have updated package/Pi data meanwhile.
                    // Replace only this field, never the captured whole result.
                    Arc::make_mut(version).uncompiled_changes = Some(result);
                    true
                } else {
                    false
                }
            };
            if publish {
                ctx.request_repaint_of(egui::ViewportId::ROOT);
                ctx.request_repaint_of(viewport_id);
            }
        }
    });
}
