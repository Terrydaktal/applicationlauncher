use super::*;

const CHECK_INTERVAL: Duration = Duration::from_secs(1);

struct PendingCheck {
    snapshot: Arc<ProcessTreeSnapshot>,
    receiver: Receiver<HashSet<ProcessIdentity>>,
}

#[derive(Default)]
pub(super) struct ProcessPresence {
    pending: Option<PendingCheck>,
    next_check: Option<Instant>,
}

impl ProcessPresence {
    pub(super) fn poll(
        &mut self,
        snapshot: &mut Arc<ProcessTreeSnapshot>,
        ctx: &egui::Context,
        viewport: egui::ViewportId,
    ) {
        if let Some(check) = self.pending.take() {
            match check.receiver.try_recv() {
                Ok(exited) => {
                    // A completed refresh must never receive an older scan's removals.
                    if Arc::ptr_eq(snapshot, &check.snapshot) {
                        remove_exited(snapshot, &exited);
                    }
                    self.next_check = Some(Instant::now() + CHECK_INTERVAL);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.next_check = Some(Instant::now() + CHECK_INTERVAL);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => self.pending = Some(check),
            }
        }
        if snapshot.nodes.is_empty() {
            return;
        }
        let delay = self
            .next_check
            .map(|next| next.saturating_duration_since(Instant::now()))
            .unwrap_or_default();
        if self.pending.is_none() && delay.is_zero() {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            let captured = snapshot.clone();
            let repaint = ctx.clone();
            applicationlauncher::spawn_worker!("window-process-presence", move |worker| {
                applicationlauncher::observe! { worker.set_state("checking-captured-identities"); }
                let exited = exited_processes(&captured, Path::new("/proc"));
                let _ = sender.send(exited);
                repaint.request_repaint_of(egui::ViewportId::ROOT);
                repaint.request_repaint_of(viewport);
            });
            self.pending = Some(PendingCheck {
                snapshot: snapshot.clone(),
                receiver,
            });
        }
        ctx.request_repaint_after_for(
            if delay.is_zero() {
                CHECK_INTERVAL
            } else {
                delay
            },
            egui::ViewportId::ROOT,
        );
    }
}

pub(super) fn exited_processes(
    snapshot: &ProcessTreeSnapshot,
    root: &Path,
) -> HashSet<ProcessIdentity> {
    let deadline = Instant::now() + CAPTURE_BUDGET;
    snapshot
        .nodes
        .iter()
        .take(MAX_TREE_PROCESSES)
        .take_while(|_| Instant::now() < deadline)
        .filter(|node| definitely_exited(root, node.identity))
        .map(|node| node.identity)
        .collect()
}

fn definitely_exited(root: &Path, identity: ProcessIdentity) -> bool {
    let mut bytes = Vec::new();
    let read = File::open(root.join(identity.pid.to_string()).join("stat"))
        .and_then(|file| file.take(MAX_STAT_BYTES as u64 + 1).read_to_end(&mut bytes));
    if let Err(error) = read {
        return matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH));
    }
    // Permission failures and malformed/partial reads are uncertainty, not exit evidence.
    if bytes.len() > MAX_STAT_BYTES {
        return false;
    }
    let Ok(stat) = std::str::from_utf8(&bytes) else {
        return false;
    };
    let Some(process) = parse_proc_stat(stat) else {
        return false;
    };
    if process.pid != identity.pid {
        return false;
    }
    let Some((_, fields)) = stat.rsplit_once(')') else {
        return false;
    };
    let mut fields = fields.split_whitespace();
    let state = fields.next();
    let Some(started) = fields.nth(18).and_then(|value| value.parse::<u64>().ok()) else {
        return false;
    };
    started != identity.start_ticks || matches!(state, Some("Z" | "X" | "x"))
}

pub(super) fn remove_exited(
    snapshot: &mut Arc<ProcessTreeSnapshot>,
    exited: &HashSet<ProcessIdentity>,
) {
    if !snapshot
        .nodes
        .iter()
        .any(|node| exited.contains(&node.identity))
    {
        return;
    }
    let mut ancestors: Vec<(usize, usize)> = Vec::new();
    let mut nodes = Vec::new();
    for node in snapshot.nodes.iter() {
        while ancestors
            .last()
            .is_some_and(|(depth, _)| *depth >= node.depth)
        {
            ancestors.pop();
        }
        if !exited.contains(&node.identity) {
            let mut retained = node.clone();
            retained.depth = ancestors.last().map_or(0, |(_, depth)| depth + 1);
            ancestors.push((node.depth, retained.depth));
            nodes.push(retained);
        }
    }
    *snapshot = Arc::new(ProcessTreeSnapshot {
        nodes: nodes.into(),
        notice: snapshot.notice.clone(),
        #[cfg(test)]
        text: snapshot.text.clone(),
    });
}

#[cfg(test)]
mod tests {
    use super::super::helper_tests::{node, snapshot};
    use super::*;

    #[test]
    fn late_presence_results_never_prune_a_new_capture() {
        let old = snapshot(vec![node(10, 0, 0, "/fixture/old")]);
        let fresh = snapshot(vec![node(10, 0, 0, "/fixture/new")]);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender.send(HashSet::from([old.nodes[0].identity])).unwrap();
        let mut presence = ProcessPresence {
            pending: Some(PendingCheck {
                snapshot: old,
                receiver,
            }),
            next_check: None,
        };
        let mut current = fresh.clone();
        presence.poll(
            &mut current,
            &egui::Context::default(),
            egui::ViewportId::ROOT,
        );
        assert!(Arc::ptr_eq(&current, &fresh));
        assert!(presence.pending.is_none());
        assert!(presence.next_check.unwrap() > Instant::now());
    }

    #[test]
    fn failed_presence_worker_keeps_metadata_and_waits_before_retrying() {
        let original = snapshot(vec![node(10, 0, 0, "/fixture/unchanged")]);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        drop(sender);
        let mut presence = ProcessPresence {
            pending: Some(PendingCheck {
                snapshot: original.clone(),
                receiver,
            }),
            next_check: None,
        };
        let mut current = original.clone();
        let ctx = egui::Context::default();
        for _ in 0..10 {
            presence.poll(&mut current, &ctx, egui::ViewportId::ROOT);
        }
        assert!(Arc::ptr_eq(&current, &original));
        assert!(presence.pending.is_none());
        assert!(presence.next_check.unwrap() > Instant::now());
    }
}
