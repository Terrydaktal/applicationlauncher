//! Presentation adapter for the shared, passive artifact-specific build inspector.
use std::fs::File;
use std::path::Path;

use program_metadata::build_status::{self, artifact::ArtifactStamp};

pub(super) const PENDING: &str = "Checking recorded build inputs…";
const CHANGED: &str = "Unknown (on-disk executable or launch path changed during inspection)";

/// Keeps the selected image and all of its publishing aliases alive while the
/// metadata worker moves on. No build inspection runs when creating this job.
pub(crate) struct Check {
    binary: File,
    stamp: Option<ArtifactStamp>,
    selection: Selection,
    launcher: Option<program_metadata::launch::PathResolution>,
}

enum Selection {
    Installed(super::installation::InstalledExecutable),
    Candidate(program_metadata::launch::PathResolution),
}

impl std::fmt::Debug for Check {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceCheck")
            .field("path", &self.path())
            .finish_non_exhaustive()
    }
}

impl Check {
    pub(super) fn installed(
        installed: super::installation::InstalledExecutable,
        binary: File,
        launcher: Option<program_metadata::launch::PathResolution>,
    ) -> Self {
        Self::new(Selection::Installed(installed), binary, launcher)
    }

    pub(super) fn candidate(
        resolved: program_metadata::launch::PathResolution,
        binary: File,
    ) -> Self {
        Self::new(Selection::Candidate(resolved), binary, None)
    }

    pub(super) fn with_expected_stamp(mut self, stamp: ArtifactStamp) -> Self {
        self.stamp = Some(stamp);
        self
    }

    fn new(
        selection: Selection,
        binary: File,
        launcher: Option<program_metadata::launch::PathResolution>,
    ) -> Self {
        let stamp = binary
            .metadata()
            .ok()
            .map(|metadata| ArtifactStamp::from_metadata(&metadata));
        Self {
            binary,
            stamp,
            selection,
            launcher,
        }
    }

    fn path(&self) -> &Path {
        match &self.selection {
            Selection::Installed(installed) => &installed.path,
            Selection::Candidate(resolved) => &resolved.path,
        }
    }

    pub(super) fn unchanged(&self) -> bool {
        let Ok(metadata) = self.binary.metadata() else {
            return false;
        };
        self.stamp == Some(ArtifactStamp::from_metadata(&metadata))
            && std::fs::metadata(self.path())
                .is_ok_and(|metadata| self.stamp == Some(ArtifactStamp::from_metadata(&metadata)))
            && self
                .launcher
                .as_ref()
                .is_none_or(|launcher| launcher.unchanged())
            && match &self.selection {
                Selection::Installed(installed) => installed.unchanged(&metadata),
                Selection::Candidate(resolved) => resolved.unchanged(),
            }
    }

    pub(super) fn inspect(&self) -> String {
        self.validated(|| inspect_changes(self.path(), &self.binary, true))
    }

    pub(super) fn validated(&self, inspect: impl FnOnce() -> String) -> String {
        if !self.unchanged() {
            return CHANGED.into();
        }
        let result = inspect();
        if self.unchanged() {
            result
        } else {
            CHANGED.into()
        }
    }
}

pub(super) fn uncompiled_changes(executable: &Path, binary: &File) -> String {
    inspect_changes(executable, binary, false)
}

fn inspect_changes(executable: &Path, binary: &File, background: bool) -> String {
    let Ok(before) = binary
        .metadata()
        .map(|metadata| ArtifactStamp::from_metadata(&metadata))
    else {
        return "Unknown (selected executable cannot be inspected)".into();
    };
    let checkout = super::installation::source_checkout_root(executable);
    let report = if background {
        // Only the dedicated source-check worker opts into the longer artifact
        // allowance; synchronous callers retain their existing strict deadline.
        build_status::inspect_artifact_background(executable, checkout.as_deref())
    } else {
        build_status::inspect_artifact(
            executable,
            checkout.as_deref(),
            std::time::Instant::now() + std::time::Duration::from_millis(1500),
        )
    };
    if report.artifact_stamp != Some(before)
        || !binary
            .metadata()
            .is_ok_and(|metadata| ArtifactStamp::from_metadata(&metadata) == before)
        || !std::fs::metadata(executable)
            .is_ok_and(|metadata| ArtifactStamp::from_metadata(&metadata) == before)
    {
        return "Unknown (on-disk executable changed during inspection)".into();
    }
    build_status::format_uncompiled_binary(&report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use std::time::UNIX_EPOCH;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "launcher-shared-freshness-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, contents: &str, seconds: u64) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            File::open(&path)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
                .unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn shared_ninja_queries_preserve_response_files_and_never_execute_build_commands() {
        let fixture = Fixture::new();
        let output = fixture.write("app", "\u{7f}ELF-built-output", 200);
        fixture.write("source.c", "changed source", 300);
        fixture.write("important.rsp", "irreplaceable response file", 100);
        fixture.write("build.ninja", "rule link\n  command = touch MUST_NOT_RUN\n  rspfile = important.rsp\n  rspfile_content = overwritten\nbuild app: link source.c\n", 100);
        let status = uncompiled_changes(&output, &File::open(&output).unwrap());
        assert_eq!(
            status,
            "1 newer · 0 missing · added: unknown · 2 inputs checked"
        );
        assert_eq!(
            fs::read_to_string(fixture.0.join("important.rsp")).unwrap(),
            "irreplaceable response file"
        );
        assert!(!fixture.0.join("MUST_NOT_RUN").exists());
        fixture.write("source.c", "old source", 100);
        let status = uncompiled_changes(&output, &File::open(&output).unwrap());
        assert_eq!(
            status,
            "0 newer · 0 missing · added: unknown · 2 inputs checked · full build status unknown"
        );
    }

    #[test]
    fn shared_cargo_receipts_track_exact_artifact_without_running_project_build_scripts() {
        let fixture = Fixture::new();
        fixture.write(
            "Cargo.toml",
            "[package]\nname='app'\nversion='1.0.0'\n",
            100,
        );
        fixture.write("build.rs", "fn main() { panic!(\"MUST_NOT_RUN\"); }", 100);
        let source = fixture.write("src/main.rs", "fn main() {}", 300);
        let output = fixture.write("target/release/app", "\u{7f}ELF-built-output", 200);
        fixture.write(
            "target/release/app.d",
            &format!("{}: {}\n", output.display(), source.display()),
            200,
        );
        let status = uncompiled_changes(&output, &File::open(&output).unwrap());
        assert!(status.starts_with("1 newer · "), "{status}");
        fixture.write("src/main.rs", "fn main() {}", 100);
        let status = uncompiled_changes(&output, &File::open(&output).unwrap());
        assert!(status.starts_with("0 newer · "), "{status}");
        assert!(!fixture.0.join("MUST_NOT_RUN").exists());
    }

    #[test]
    fn retained_pinned_old_artifact_cannot_receive_replacement_build_status() {
        let fixture = Fixture::new();
        let output = fixture.write("app", "\u{7f}ELF-old", 100);
        let pinned = File::open(&output).unwrap();
        let replacement = fixture.write("replacement", "\u{7f}ELF-new", 200);
        fs::rename(replacement, &output).unwrap();
        assert!(uncompiled_changes(&output, &pinned).contains("changed during inspection"));
    }

    #[test]
    fn unsupported_sources_remain_unknown_instead_of_using_repository_wide_timestamps_as_proof() {
        let fixture = Fixture::new();
        let output = fixture.write("app", "\u{7f}ELF-output", 100);
        fixture.write("build.py", "raise RuntimeError('MUST_NOT_RUN')", 300);
        let status = uncompiled_changes(&output, &File::open(&output).unwrap());
        assert!(status.starts_with("Unknown"), "{status}");
        assert!(!status.contains("0 newer"), "{status}");
        assert!(!status.contains("0 missing"), "{status}");
        assert!(!fixture.0.join("MUST_NOT_RUN").exists());
    }
}
