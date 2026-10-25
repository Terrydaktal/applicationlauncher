use std::fs::File;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

const MAX_MANIFEST_BYTES: u64 = 128 * 1024;
const MAX_COMMAND_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct NodeApplication {
    pub(crate) package: String,
    pub(crate) installed_version: String,
    pub(crate) entry_point: PathBuf,
    pub(crate) manifest: PathBuf,
    pub(crate) identification: &'static str,
}

fn read_file(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

pub(super) fn probe(pid: i32, program: &str) -> Option<NodeApplication> {
    let bytes = read_file(
        Path::new(&format!("/proc/{pid}/cmdline")),
        MAX_COMMAND_BYTES,
    )?;
    let args = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(std::str::from_utf8)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    let mut candidates = Vec::new();
    if valid_command(program) && !is_runtime_name(program) {
        if let Some(path) = applicationlauncher::process::executable_path(program) {
            candidates.push(path);
        }
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(PathBuf::from(home).join(".local/bin").join(program));
        }
    }
    identify(program, &args, &cwd, &candidates)
}

fn is_runtime_name(name: &str) -> bool {
    matches!(name, "node" | "nodejs")
}

fn valid_command(command: &str) -> bool {
    !command.is_empty()
        && command.len() <= 255
        && !command.starts_with('-')
        && command
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
}

fn identify(
    program: &str,
    args: &[&str],
    cwd: &Path,
    candidates: &[PathBuf],
) -> Option<NodeApplication> {
    let argv0 = Path::new(args.first()?);
    if argv0
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_runtime_name)
    {
        // Accept an unambiguous script argument, not --eval, a project cwd's
        // package.json, or guessed parsing of arbitrary Node switches.
        let script = args.get(1).filter(|arg| !arg.starts_with('-'))?;
        return package_for_entry(
            &cwd.join(script),
            None,
            "Process script argument (installed metadata)",
        );
    }
    if argv0.components().count() > 1 {
        return package_for_entry(
            &cwd.join(argv0),
            None,
            "Process script path (installed metadata)",
        );
    }
    if !valid_command(program) || is_runtime_name(program) || args != [program] {
        return None;
    }
    let mut found: Option<NodeApplication> = None;
    for path in candidates.iter().take(4) {
        let Some(application) = package_for_entry(
            path,
            Some(program),
            "Command lookup after process-title rewrite (inferred; installed metadata)",
        ) else {
            continue;
        };
        if let Some(previous) = &found {
            if previous.entry_point != application.entry_point {
                return None;
            }
        } else {
            found = Some(application);
        }
    }
    found
}

fn manifest_text(value: &Value, key: &str, limit: usize) -> Option<String> {
    let text = value.get(key)?.as_str()?;
    (!text.is_empty() && text.len() <= limit && !text.chars().any(char::is_control))
        .then(|| text.to_owned())
}

fn package_for_entry(
    entry: &Path,
    command: Option<&str>,
    identification: &'static str,
) -> Option<NodeApplication> {
    let entry = std::fs::canonicalize(entry).ok()?;
    if !std::fs::metadata(&entry).is_ok_and(|metadata| metadata.is_file()) {
        return None;
    }
    for root in entry.ancestors().skip(1).take(8) {
        let manifest = root.join("package.json");
        if !manifest.try_exists().ok()? {
            continue;
        }
        let bytes = read_file(&manifest, MAX_MANIFEST_BYTES)?;
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        let Some(package) = manifest_text(&value, "name", 256) else {
            continue;
        };
        let installed_version = manifest_text(&value, "version", 128)?;
        let matching_bin = |name: &str, bin: &Value| {
            let Some(bin) = bin.as_str() else {
                return false;
            };
            let bin = Path::new(bin);
            command.is_none_or(|command| command == name)
                && !bin.components().any(|component| {
                    matches!(
                        component,
                        Component::ParentDir | Component::RootDir | Component::Prefix(_)
                    )
                })
                && std::fs::canonicalize(root.join(bin)).ok().as_ref() == Some(&entry)
        };
        let bin = value.get("bin")?;
        let matches = match bin {
            Value::String(_) => matching_bin(package.rsplit('/').next()?, bin),
            Value::Object(bins) => bins
                .iter()
                .take(64)
                .any(|(name, bin)| matching_bin(name, bin)),
            _ => false,
        };
        if !matches {
            return None;
        }
        return Some(NodeApplication {
            package,
            installed_version,
            entry_point: entry,
            manifest,
            identification,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let directory = std::env::temp_dir()
                .join(format!("al-node-package-{}-{nonce}", std::process::id()));
            std::fs::create_dir(&directory).unwrap();
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(directory)
        }

        fn install(&self, directory: &str, version: &str) -> PathBuf {
            let root = self.0.join(directory);
            std::fs::create_dir_all(root.join("dist")).unwrap();
            // Invalid JS on purpose: inspecting package metadata must not run it.
            std::fs::write(root.join("dist/cli.js"), "THIS_MUST_NEVER_EXECUTE").unwrap();
            std::fs::write(
                root.join("package.json"),
                serde_json::json!({
                    "name": "@earendil-works/pi-coding-agent", "version": version,
                    "bin": {"pi": "dist/cli.js"}
                })
                .to_string(),
            )
            .unwrap();
            root.join("dist/cli.js")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn rewritten_pi_title_resolves_the_declared_package_through_its_symlink() {
        let fixture = Fixture::new();
        let entry = fixture.install("0.84.2", "0.84.2");
        let link = fixture.0.join("pi");
        symlink(&entry, &link).unwrap();
        let application = identify("pi", &["pi"], &fixture.0, &[link.clone()]).unwrap();
        assert_eq!(application.package, "@earendil-works/pi-coding-agent");
        assert_eq!(application.installed_version, "0.84.2");
        assert_eq!(application.entry_point, entry);
        assert!(application.identification.contains("inferred"));
        let next = fixture.install("0.85.0", "0.85.0");
        std::fs::remove_file(&link).unwrap();
        symlink(&next, &link).unwrap();
        assert_eq!(
            identify("pi", &["pi"], &fixture.0, &[link])
                .unwrap()
                .installed_version,
            "0.85.0"
        );
    }

    #[test]
    fn explicit_script_arguments_are_resolved_without_guessing_from_the_cwd() {
        let fixture = Fixture::new();
        let entry = fixture.install("application", "0.84.2");
        let application = identify(
            "node",
            &["/usr/bin/node", entry.to_str().unwrap()],
            &fixture.0,
            &[],
        )
        .unwrap();
        assert_eq!(application.entry_point, entry);
        assert!(
            application
                .identification
                .starts_with("Process script argument")
        );
        assert!(
            identify(
                "node",
                &["node", "--eval", "code"],
                &fixture.0.join("application"),
                &[]
            )
            .is_none()
        );
        assert!(identify("unrelated", &["unrelated"], &fixture.0, &[entry.clone()]).is_none());
        let second = fixture.install("other", "0.85.0");
        assert!(identify("pi", &["pi"], &fixture.0, &[entry, second]).is_none());
    }

    #[test]
    fn malformed_oversized_and_unrelated_manifests_are_not_application_versions() {
        let fixture = Fixture::new();
        let entry = fixture.install("application", "0.84.2");
        let manifest = fixture.0.join("application/package.json");
        for contents in [
            "not JSON".to_owned(),
            " ".repeat(MAX_MANIFEST_BYTES as usize + 1),
            r#"{"name":"wrong","version":"26.4.0","bin":{"pi":"../outside.js"}}"#.to_owned(),
            r#"{"name":"wrong","version":"26.4.0","bin":{"other":"dist/cli.js"}}"#.to_owned(),
        ] {
            std::fs::write(&manifest, contents).unwrap();
            assert!(identify("pi", &["pi"], &fixture.0, &[entry.clone()]).is_none());
        }
    }
}
