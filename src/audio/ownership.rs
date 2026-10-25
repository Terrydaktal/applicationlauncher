//! Verified same-executable audio helpers, resolved only during worker metadata discovery.
use crate::models::PactlSinkInput;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;

const MAX_ANCESTORS: usize = 16;
const MAX_STAT_BYTES: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessIdentity {
    parent: i32,
    start_ticks: u64,
    executable: (u64, u64),
}

fn parse_stat(text: &str, pid: i32) -> Option<(i32, u64)> {
    let (prefix, fields) = text.rsplit_once(") ")?;
    if prefix.split_whitespace().next()?.parse::<i32>().ok()? != pid {
        return None;
    }
    let mut fields = fields.split_whitespace();
    let parent = fields.nth(1)?.parse::<i32>().ok()?;
    let start_ticks = fields.nth(17)?.parse().ok()?;
    (parent >= 0).then_some((parent, start_ticks))
}

fn read_stat(pid: i32) -> Option<(i32, u64)> {
    let mut text = String::new();
    File::open(format!("/proc/{pid}/stat"))
        .ok()?
        .take(MAX_STAT_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= MAX_STAT_BYTES)
        .then(|| parse_stat(&text, pid))
        .flatten()
}

fn inspect(pid: i32) -> Option<ProcessIdentity> {
    let (parent, start_ticks) = read_stat(pid)?;
    let executable = std::fs::metadata(format!("/proc/{pid}/exe")).ok()?;
    (read_stat(pid)? == (parent, start_ticks)).then_some(ProcessIdentity {
        parent,
        start_ticks,
        executable: (executable.dev(), executable.ino()),
    })
}

fn ancestors_with(pid: i32, mut inspect: impl FnMut(i32) -> Option<ProcessIdentity>) -> Vec<i32> {
    if pid <= 1 {
        return Vec::new();
    }
    let Some(original) = inspect(pid) else {
        return Vec::new();
    };
    let mut current = original;
    let mut ancestors = Vec::new();
    // Each step moves to a verified older parent; stop at another executable,
    // a missing process, init, a cycle or the fixed depth limit.
    for _ in 0..MAX_ANCESTORS {
        let parent = current.parent;
        if parent <= 1 {
            break;
        }
        if parent == pid || ancestors.contains(&parent) {
            return Vec::new();
        }
        let Some(identity) = inspect(parent) else {
            break;
        };
        if identity.executable != original.executable || identity.start_ticks > current.start_ticks
        {
            break;
        }
        ancestors.push(parent);
        current = identity;
    }
    if inspect(pid) != Some(original) {
        ancestors.clear();
    }
    ancestors
}

pub(super) fn resolve(sinks: &mut [PactlSinkInput]) {
    let mut resolved = HashMap::new();
    for sink in sinks {
        sink.process_ancestors = sink
            .properties
            .get("application.process.id")
            .and_then(|pid| pid.parse::<i32>().ok())
            .filter(|pid| *pid > 1)
            .map(|pid| {
                resolved
                    .entry(pid)
                    .or_insert_with(|| ancestors_with(pid, inspect))
                    .clone()
            })
            .unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(parent: i32, start_ticks: u64, inode: u64) -> ProcessIdentity {
        ProcessIdentity {
            parent,
            start_ticks,
            executable: (1, inode),
        }
    }

    #[test]
    fn browser_audio_helpers_stop_before_launching_shells_or_terminal_windows() {
        let processes = HashMap::from([
            (400, process(300, 40, 1)),
            (300, process(200, 30, 1)),
            (200, process(100, 20, 2)),
            (100, process(1, 10, 3)),
        ]);
        assert_eq!(
            ancestors_with(400, |pid| processes.get(&pid).copied()),
            vec![300]
        );
    }

    #[test]
    fn vanished_reused_or_reparented_audio_processes_discard_their_ancestry() {
        for changed in [None, Some(process(200, 41, 1)), Some(process(250, 40, 1))] {
            let mut reads = 0;
            let ancestors = ancestors_with(300, |pid| {
                if pid == 300 {
                    reads += 1;
                    if reads == 1 {
                        Some(process(200, 40, 1))
                    } else {
                        changed
                    }
                } else {
                    Some(process(1, 20, 1))
                }
            });
            assert!(ancestors.is_empty(), "{changed:?}");
        }
        assert!(ancestors_with(300, |_| None).is_empty());
    }

    #[test]
    fn cycles_newer_parents_and_depth_are_bounded() {
        let cyclic = HashMap::from([(300, process(200, 20, 1)), (200, process(300, 20, 1))]);
        assert!(ancestors_with(300, |pid| cyclic.get(&pid).copied()).is_empty());
        let newer = HashMap::from([(300, process(200, 20, 1)), (200, process(1, 30, 1))]);
        assert!(ancestors_with(300, |pid| newer.get(&pid).copied()).is_empty());
        let mut reads = 0;
        let ancestors = ancestors_with(1000, |pid| {
            reads += 1;
            Some(process(pid - 1, pid as u64, 1))
        });
        assert_eq!(ancestors.len(), MAX_ANCESTORS);
        assert_eq!(reads, MAX_ANCESTORS + 2);
        assert!(ancestors_with(0, |_| panic!("invalid PID must not be inspected")).is_empty());
    }

    #[test]
    fn stat_identity_handles_parentheses_in_names_and_rejects_wrong_pids() {
        let stat = format!("300 (audio ) process) S 200 {} 40 0", "0 ".repeat(17));
        assert_eq!(parse_stat(&stat, 300), Some((200, 40)));
        assert_eq!(parse_stat(&stat, 301), None);
        assert_eq!(parse_stat("300 (chrome) S 200", 300), None);
    }
}
