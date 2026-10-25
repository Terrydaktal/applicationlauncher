use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

const MAX_DEPINFO_BYTES: u64 = 1024 * 1024;
const MAX_INPUTS: usize = 8192;
const INPUT_CHECK_BUDGET: Duration = Duration::from_millis(200);

pub(super) fn uncompiled_changes(executable: &Path, binary: &File) -> String {
    for directory in executable.ancestors().skip(1).take(8) {
        if directory.join("build.ninja").is_file() {
            return ninja_status(directory, executable);
        }
        if directory.join("Cargo.toml").is_file() {
            return cargo_status(directory, executable, binary);
        }
    }
    "Unknown (no supported local build metadata)".into()
}

fn ninja_status(directory: &Path, executable: &Path) -> String {
    let Some(ninja) = applicationlauncher::process::executable_path("ninja") else {
        return "Unknown (Ninja unavailable)".into();
    };
    let Ok(target) = executable.strip_prefix(directory) else {
        return "Unknown (Ninja target unavailable)".into();
    };
    let mut command = Command::new(ninja);
    command
        .arg("-C")
        .arg(directory)
        .args(["-n", "-d", "explain", "--"])
        .arg(target)
        .env("LC_ALL", "C")
        .env("NINJA_STATUS", "applicationlauncher-pending-build: ");
    // Dry-run only: even custom commands and manifest regeneration must not run.
    let Some(output) = super::bounded_output(command) else {
        return "Unknown (Ninja dry-run failed or timed out)".into();
    };
    classify_ninja_status(&String::from_utf8_lossy(&output)).into()
}

fn classify_ninja_status(output: &str) -> &'static str {
    let pending = output
        .lines()
        .any(|line| line.starts_with("applicationlauncher-pending-build: "));
    let changed_input = output.lines().any(|line| {
        let Some(reason) = line.strip_prefix("ninja explain: ") else {
            return false;
        };
        (reason.starts_with("output ")
            && (reason.contains(" doesn't exist")
                || reason.contains(" older than most recent input ")))
            || (reason.starts_with("restat of output ")
                && reason.contains(" older than most recent input "))
            || reason.starts_with("command line changed for ")
    });
    if pending && changed_input {
        "Yes (Ninja found changed or missing build inputs)"
    } else if pending {
        // Restat generators (e.g. Meson's vcs_tag) are always scheduled, but may
        // leave identical output. A dry-run cannot prove that sources changed.
        "Unknown (pending checks may be always-run generators, not source changes)"
    } else if output.lines().any(|line| line == "ninja: no work to do.") {
        "No (Ninja target is up to date)"
    } else {
        "Unknown (unrecognised Ninja dry-run output)"
    }
}

fn depinfo_inputs(text: &str) -> Option<Vec<PathBuf>> {
    let joined = text.replace("\\\r\n", "").replace("\\\n", "");
    let line = joined.lines().next()?;
    let mut escaped = false;
    let separator = line.char_indices().find_map(|(index, ch)| {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == ':' && line[index + 1..].starts_with(char::is_whitespace) {
            return Some(index);
        }
        None
    })?;
    let mut inputs = Vec::new();
    let mut word = String::new();
    let mut chars = line[separator + 1..].chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => word.push(chars.next()?),
            '#' => break,
            '$' if chars.peek() == Some(&'$') => {
                chars.next();
                word.push('$');
            }
            ch if ch.is_whitespace() => {
                if !word.is_empty() {
                    inputs.push(PathBuf::from(std::mem::take(&mut word)));
                }
            }
            ch => word.push(ch),
        }
        if inputs.len() > MAX_INPUTS {
            return None;
        }
    }
    if !word.is_empty() {
        inputs.push(PathBuf::from(word));
    }
    (!inputs.is_empty() && inputs.len() <= MAX_INPUTS).then_some(inputs)
}

fn cargo_status(directory: &Path, executable: &Path, binary: &File) -> String {
    let check = || -> Option<bool> {
        let built_at = binary.metadata().ok()?.modified().ok()?;
        let mut bytes = Vec::new();
        File::open(executable.with_extension("d"))
            .ok()?
            .take(MAX_DEPINFO_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_DEPINFO_BYTES {
            return None;
        }
        let mut inputs = depinfo_inputs(std::str::from_utf8(&bytes).ok()?)?;
        // Cargo dep-info is not a dry-run API. Include manifests/config and new
        // source files, and explicitly qualify the timestamp-based result.
        for name in [
            "Cargo.toml",
            "Cargo.lock",
            "build.rs",
            "src",
            ".cargo",
            "rust-toolchain",
            "rust-toolchain.toml",
        ] {
            let path = directory.join(name);
            if path.try_exists().ok()? {
                inputs.push(path);
            }
        }
        newer_inputs(directory, inputs, built_at)
    };
    match check() {
        Some(true) => "Yes (Cargo source/configuration is newer than the on-disk binary)",
        Some(false) => "No newer Cargo source inputs (timestamp check, not a Cargo build)",
        None => "Unknown (Cargo input metadata missing, unsupported or check limit reached)",
    }
    .into()
}

fn optional_cargo_input(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        matches!(
            name.to_str(),
            Some("rust-toolchain" | "rust-toolchain.toml" | "Cargo.lock" | "build.rs" | ".cargo")
        )
    })
}

fn newer_inputs(root: &Path, inputs: Vec<PathBuf>, built_at: SystemTime) -> Option<bool> {
    let deadline = Instant::now() + INPUT_CHECK_BUDGET;
    let mut remaining = MAX_INPUTS;
    let mut visited = HashSet::new();
    let mut pending = inputs;
    // A shared file/time budget bounds directory traversal, including symlinks.
    while let Some(path) = pending.pop() {
        if remaining == 0 || Instant::now() >= deadline {
            return None;
        }
        remaining -= 1;
        let path = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && optional_cargo_input(&path) =>
            {
                continue;
            }
            Err(_) => return None,
        };
        if metadata.modified().ok()? > built_at {
            return Some(true);
        }
        if metadata.is_dir() && visited.insert(fs::canonicalize(&path).ok()?) {
            for entry in fs::read_dir(&path).ok()? {
                if pending.len() >= remaining || Instant::now() >= deadline {
                    return None;
                }
                pending.push(entry.ok()?.path());
            }
        }
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::FileTimes;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("al-build-status-{}-{nonce}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn write(&self, name: &str, contents: &str, modified: SystemTime) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            File::open(&path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(modified))
                .unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn ninja_status_uses_dry_run_and_never_executes_pending_commands() {
        let fixture = Fixture::new();
        let now = SystemTime::now();
        fixture.write(
            "build.ninja",
            "rule test\n  command = touch MUST_NOT_RUN\nbuild app: test input\n",
            now,
        );
        fixture.write("input", "changed", now);
        let executable = fixture.0.join("app");
        assert_eq!(
            ninja_status(&fixture.0, &executable),
            "Yes (Ninja found changed or missing build inputs)"
        );
        assert!(!fixture.0.join("MUST_NOT_RUN").exists());
        assert!(!executable.exists());
        fixture.write("app", "already built", now + Duration::from_secs(60));
        fixture.write("build.ninja", "build app: phony input\n", now);
        assert_eq!(
            ninja_status(&fixture.0, &executable),
            "No (Ninja target is up to date)"
        );
        assert!(!fixture.0.join("MUST_NOT_RUN").exists());
        fixture.write("build.ninja", "not valid Ninja syntax\n", now);
        assert!(ninja_status(&fixture.0, &executable).starts_with("Unknown"));
    }

    #[test]
    fn always_run_ninja_checks_do_not_prove_uncompiled_changes() {
        let fixture = Fixture::new();
        let now = SystemTime::now();
        fixture.write(
            "build.ninja",
            "rule check_revision\n  command = touch MUST_NOT_RUN\n  restat = 1\nbuild FORCE: phony\nbuild revision.h: check_revision FORCE\nbuild app: phony revision.h\n",
            now,
        );
        fixture.write("revision.h", "already current revision", now);
        let executable = fixture.write("app", "already built", now + Duration::from_secs(60));
        let status = ninja_status(&fixture.0, &executable);
        assert!(status.starts_with("Unknown"), "{status}");
        assert!(!fixture.0.join("MUST_NOT_RUN").exists());
    }

    #[test]
    fn real_ninja_input_changes_are_not_hidden_by_always_run_checks() {
        for reason in [
            "output app.o older than most recent input main.c (1 vs 2)",
            "output app.o doesn't exist",
            "command line changed for app.o",
            "restat of output generated.h older than most recent input config (1 vs 2)",
        ] {
            let output = format!(
                "ninja explain: FORCE is dirty\nninja explain: {reason}\napplicationlauncher-pending-build: CC app.o\n"
            );
            assert!(
                classify_ninja_status(&output).starts_with("Yes"),
                "{reason}"
            );
        }
        assert!(classify_ninja_status(
            "ninja explain: PHONY is dirty\nninja explain: revision.h is dirty\napplicationlauncher-pending-build: Generating revision.h\n"
        ).starts_with("Unknown"));
    }

    #[test]
    fn cargo_depinfo_handles_escaped_paths_and_continuations() {
        assert_eq!(
            depinfo_inputs("/tmp/my\\ app: /tmp/a\\ b.rs \\\n /tmp/c\\#d.rs /tmp/$$file.rs\n"),
            Some(vec![
                "/tmp/a b.rs".into(),
                "/tmp/c#d.rs".into(),
                "/tmp/$file.rs".into(),
            ])
        );
        assert!(depinfo_inputs("invalid dep-info").is_none());
        assert!(depinfo_inputs("app: ").is_none());
        assert!(depinfo_inputs(&format!("app: {}", "input ".repeat(MAX_INPUTS + 1))).is_none());
    }

    #[test]
    fn cargo_status_tracks_sources_against_latest_binary_not_git_dirty_state() {
        let fixture = Fixture::new();
        let now = SystemTime::now();
        let old = now - Duration::from_secs(60);
        fixture.write("Cargo.toml", "[package]", old);
        let source = fixture.write("src/main.rs", "fn main() {}", old);
        File::open(fixture.0.join("src"))
            .unwrap()
            .set_times(FileTimes::new().set_modified(old))
            .unwrap();
        let executable = fixture.write("target/release/app", "binary", now);
        fixture.write(
            "target/release/app.d",
            &format!("app: {}\n", source.display()),
            now,
        );
        let binary = File::open(&executable).unwrap();
        assert!(cargo_status(&fixture.0, &executable, &binary).starts_with("No newer Cargo"));
        fixture.write("src/main.rs", "changed", now + Duration::from_secs(60));
        assert!(cargo_status(&fixture.0, &executable, &binary).starts_with("Yes"));
        binary
            .set_times(FileTimes::new().set_modified(now + Duration::from_secs(120)))
            .unwrap();
        assert!(cargo_status(&fixture.0, &executable, &binary).starts_with("No newer Cargo"));
        fixture.write("src/new.rs", "new source", now + Duration::from_secs(180));
        assert!(cargo_status(&fixture.0, &executable, &binary).starts_with("Yes"));
        fs::remove_file(executable.with_extension("d")).unwrap();
        assert!(cargo_status(&fixture.0, &executable, &binary).starts_with("Unknown"));
    }

    #[test]
    fn unsupported_sources_missing_inputs_and_excess_work_are_unknown() {
        let fixture = Fixture::new();
        let binary = fixture.write("app", "binary", SystemTime::now());
        assert!(uncompiled_changes(&binary, &File::open(&binary).unwrap()).starts_with("Unknown"));
        assert_eq!(
            newer_inputs(&fixture.0, vec!["missing.rs".into()], SystemTime::now()),
            None
        );
        assert_eq!(
            newer_inputs(&fixture.0, vec!["rust-toolchain".into()], SystemTime::now()),
            Some(false)
        );
        assert_eq!(
            newer_inputs(
                &fixture.0,
                vec!["app".into(); MAX_INPUTS + 1],
                SystemTime::now()
            ),
            None
        );
    }
}
