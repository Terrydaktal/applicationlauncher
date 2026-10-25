use std::collections::HashSet;
use std::fs::File;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use super::{PackageTarget, ProgsMetadata, installation};

const MAX_COMMAND_BYTES: usize = 16 * 1024;
const MAX_WRAPPER_BYTES: u64 = 64 * 1024;
const MAX_LAUNCH_STEPS: usize = 24;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ApplicationInfo {
    pub(crate) launcher: Option<PathBuf>,
    pub(crate) launcher_resolved: Option<PathBuf>,
    pub(crate) launcher_exists: bool,
    pub(crate) desktop_exists: bool,
    pub(crate) chain: Vec<PathBuf>,
    pub(crate) resolution: String,
    pub(crate) executable: Option<PathBuf>,
    pub(crate) native: Option<bool>,
    pub(crate) installation: Option<String>,
    pub(crate) metadata: ProgsMetadata,
    pub(crate) version: Option<String>,
    pub(crate) package: Option<String>,
    pub(crate) package_target: Option<PackageTarget>,
    pub(crate) latest_package_version: Option<String>,
    pub(crate) build_id: Option<String>,
    pub(crate) uncompiled_changes: Option<String>,
    pub(crate) launcher_installation: Option<String>,
    pub(crate) launcher_metadata: ProgsMetadata,
}

impl ApplicationInfo {
    pub(crate) fn query_package_version(&self) -> Option<String> {
        self.package_target.as_ref().map(super::available::query)
    }
}

#[derive(Debug)]
pub(crate) enum ApplicationInfoUpdate {
    Details(ApplicationInfo),
    PackageVersion(String),
}

fn shell_word(input: &str) -> Option<(String, &str)> {
    let input = input.trim_start();
    let mut word = String::new();
    let (mut quote, mut escaped) = (None, false);
    for (index, ch) in input.char_indices() {
        if escaped {
            word.push(ch);
            escaped = false;
        } else if quote != Some('\'') && ch == '\\' {
            escaped = true;
        } else if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() && matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if quote.is_none() && ch.is_whitespace() {
            return (!word.is_empty()).then_some((word, &input[index..]));
        } else if quote.is_none() && matches!(ch, ';' | '|' | '&' | '<' | '>' | '(' | ')' | '#') {
            return None;
        } else {
            word.push(ch);
        }
    }
    (quote.is_none() && !escaped && !word.is_empty()).then_some((word, ""))
}

fn wrapper_target(script: &str, path: &Path) -> Result<Option<PathBuf>, &'static str> {
    let mut target = None;
    let mut last_command = "";
    let mut wrapper_here = 0;
    let mut wrapper_origin = 0;
    for line in script.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        last_command = line;
        if line == "export CHROME_WRAPPER=\"`readlink -f \"$0\"`\"" {
            wrapper_origin += 1;
        } else if line.contains("CHROME_WRAPPER=") {
            return Err("Unknown (dynamic wrapper origin)");
        }
        if line == "HERE=\"`dirname \"$CHROME_WRAPPER\"`\"" {
            wrapper_here += 1;
        } else if line.contains("HERE=") {
            return Err("Unknown (dynamic wrapper directory)");
        }
        let Some(rest) = line.strip_prefix("exec ").map(str::trim_start) else {
            continue;
        };
        // Redirection-only execs do not choose an application (Chrome uses these).
        let bytes = rest.as_bytes();
        if matches!(bytes.first(), Some(b'<' | b'>'))
            || (bytes.first().is_some_and(u8::is_ascii_digit)
                && matches!(bytes.get(1), Some(b'<' | b'>')))
        {
            continue;
        }
        if target.is_some() {
            return Err("Unknown (multiple wrapper exec targets)");
        }
        let (mut program, mut rest) =
            shell_word(rest).ok_or("Unknown (unsupported wrapper exec syntax)")?;
        if program == "-a" {
            let (_, remaining) = shell_word(rest).ok_or("Unknown (unsupported wrapper argv0)")?;
            (program, rest) =
                shell_word(remaining).ok_or("Unknown (missing wrapper exec target)")?;
        }
        if program == "--" {
            (program, rest) = shell_word(rest).ok_or("Unknown (missing wrapper exec target)")?;
        }
        if rest.contains([';', '|', '&', '`']) || program.starts_with('-') {
            return Err("Unknown (conditional or unsupported wrapper exec)");
        }
        let path = if let Some(relative) = program.strip_prefix("$HERE/") {
            if wrapper_origin != 1 || wrapper_here != 1 || relative.contains(['$', '`']) {
                return Err("Unknown (unverified wrapper directory)");
            }
            path.parent()
                .ok_or("Unknown (wrapper directory unavailable)")?
                .join(relative)
        } else if program.contains(['$', '`']) || !Path::new(&program).is_absolute() {
            return Err("Unknown (wrapper target is not a literal absolute path)");
        } else {
            PathBuf::from(program)
        };
        target = Some((path, line));
    }
    match target {
        Some((path, line)) if line == last_command => Ok(Some(path)),
        Some(_) => Err("Unknown (wrapper exec is not its final command)"),
        None => Ok(None),
    }
}

#[derive(Default)]
struct Resolution {
    chain: Vec<PathBuf>,
    stamps: Vec<(PathBuf, super::ExecutableIdentity)>,
    launcher_resolved: Option<PathBuf>,
    executable: Option<(PathBuf, File)>,
    reason: String,
}

impl Resolution {
    fn unchanged(&self) -> bool {
        self.stamps.iter().all(|(path, identity)| {
            std::fs::metadata(path)
                .is_ok_and(|metadata| super::executable_identity(&metadata) == *identity)
        })
    }
}

fn resolve(launcher: &Path) -> Resolution {
    let mut result = Resolution::default();
    let resolved = (|| -> Result<(PathBuf, File), &'static str> {
        let mut path = launcher.to_owned();
        let mut seen = HashSet::new();
        for _ in 0..MAX_LAUNCH_STEPS {
            if !path.is_absolute() {
                return Err("Unknown (relative launch path has no verified working directory)");
            }
            if !seen.insert(path.clone()) {
                return Err("Unknown (launch chain contains a cycle)");
            }
            result.chain.push(path.clone());
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|_| "Unknown (launch target missing or inaccessible)")?;
            if metadata.file_type().is_symlink() {
                // Following metadata also lets the final recheck detect a retargeted link.
                let target = std::fs::metadata(&path)
                    .map_err(|_| "Unknown (broken or cyclic launch symlink)")?;
                result
                    .stamps
                    .push((path.clone(), super::executable_identity(&target)));
                let target = std::fs::read_link(&path)
                    .map_err(|_| "Unknown (launch symlink cannot be read)")?;
                path = if target.is_absolute() {
                    target
                } else {
                    path.parent()
                        .ok_or("Unknown (launch directory unavailable)")?
                        .join(target)
                };
                continue;
            }
            let canonical = std::fs::canonicalize(&path)
                .map_err(|_| "Unknown (launch path cannot be resolved)")?;
            if canonical != path {
                path = canonical;
                continue;
            }
            let file = File::options()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
                .open(&path)
                .map_err(|_| "Unknown (launch file cannot be opened)")?;
            let metadata = file
                .metadata()
                .map_err(|_| "Unknown (launch metadata unavailable)")?;
            if !metadata.is_file() {
                return Err("Unknown (launch target is not a regular file)");
            }
            result.launcher_resolved.get_or_insert_with(|| path.clone());
            result
                .stamps
                .push((path.clone(), super::executable_identity(&metadata)));
            if super::is_elf(&file) {
                return Ok((path, file));
            }
            if metadata.len() > MAX_WRAPPER_BYTES {
                return Err("Unknown (launch script exceeds inspection limit)");
            }
            let mut bytes = vec![0; metadata.len() as usize];
            file.read_exact_at(&mut bytes, 0)
                .map_err(|_| "Unknown (launch script changed or cannot be read)")?;
            let script = std::str::from_utf8(&bytes)
                .map_err(|_| "Unknown (launch file is neither ELF nor a supported script)")?;
            let interpreter = script
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("#!"))
                .and_then(|line| {
                    let mut words = line.split_whitespace();
                    let first = words.next()?;
                    if Path::new(first).file_name()? == "env" {
                        match words.next()? {
                            "-S" => words.next(),
                            command if !command.starts_with('-') => Some(command),
                            _ => None,
                        }
                    } else {
                        Some(first)
                    }
                })
                .and_then(|word| Path::new(word).file_name())
                .and_then(|name| name.to_str());
            match interpreter {
                Some("bash" | "sh" | "dash" | "zsh") => {
                    path = wrapper_target(script, &path)?
                        .ok_or("Unknown (shell script has no supported final exec target)")?;
                }
                Some(_) => return Ok((path, file)),
                None => return Err("Unknown (launch file has no recognized interpreter)"),
            }
        }
        Err("Unknown (launch chain exceeds inspection limit)")
    })();
    match resolved {
        Ok(executable) if result.unchanged() => {
            result.reason =
                "Statically resolved from launch files; not a running-process identity".into();
            result.executable = Some(executable);
        }
        Ok(_) => result.reason = "Unknown (launch chain changed during inspection)".into(),
        Err(reason) => result.reason = reason.into(),
    }
    result
}

pub(crate) fn inspect_application(exec: &str, desktop_file: &Path) -> ApplicationInfo {
    let mut result = ApplicationInfo {
        desktop_exists: desktop_file.is_file(),
        ..Default::default()
    };
    if exec.len() > MAX_COMMAND_BYTES {
        result.resolution = "Unknown (desktop command exceeds inspection limit)".into();
        return result;
    }
    let Some(launcher) = crate::search::executable_path_from_exec(exec) else {
        result.resolution = "Unknown (desktop command cannot be resolved)".into();
        return result;
    };
    result.launcher_exists = launcher.is_file();
    let (label, metadata) = installation::inspect(&launcher);
    result.launcher_installation = Some(label);
    result.launcher_metadata = metadata;
    result.launcher = Some(launcher.clone());
    if launcher.file_name().is_some_and(|name| name == "env") {
        result.resolution =
            "Unknown (environment launcher requires explicit target resolution)".into();
        return result;
    }
    let resolution = resolve(&launcher);
    result.chain = resolution.chain.clone();
    result.launcher_resolved = resolution.launcher_resolved.clone();
    result.resolution = resolution.reason.clone();
    let Some((executable, file)) = &resolution.executable else {
        return result;
    };
    let name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let version = super::probe_executable(file, name);
    let package = super::installed_package(executable);
    let (installation, metadata) = installation::inspect_with_owner(executable, package.as_deref());
    result.package_target = super::available::target(executable, package.as_deref());
    result.uncompiled_changes = Some(if package.is_some() {
        "Not applicable (package-managed installation)".into()
    } else {
        super::freshness::uncompiled_changes(executable, file)
    });
    result.executable = Some(executable.clone());
    result.native = Some(super::is_elf(file));
    result.version = version.version;
    result.build_id = version.build_id;
    result.package = package;
    result.installation = Some(installation);
    result.metadata = metadata;
    if !resolution.unchanged() {
        return ApplicationInfo {
            launcher: result.launcher,
            launcher_exists: result.launcher_exists,
            desktop_exists: result.desktop_exists,
            chain: result.chain,
            resolution: "Unknown (launch chain changed during inspection; reopen Show Info)".into(),
            launcher_installation: result.launcher_installation,
            launcher_metadata: result.launcher_metadata,
            ..Default::default()
        };
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "launcher-app-info-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn file(&self, name: &str, content: impl AsRef<[u8]>) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, content).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn chrome_launch_chain_follows_symlinks_env_shebangs_and_the_verified_here_template() {
        let root = Fixture::new();
        let binary = root.file("chrome", b"\x7fELF");
        let browser = root.file("google-chrome", "#!/bin/bash\nexport CHROME_WRAPPER=\"`readlink -f \"$0\"`\"\nHERE=\"`dirname \"$CHROME_WRAPPER\"`\"\nexec < /dev/null\nexec > >(exec cat)\nexec 2> >(exec cat >&2)\nexec -a \"$0\" \"$HERE/chrome\" \"$@\"\n");
        let stable = root.file(
            "google-chrome-stable",
            format!(
                "#!/bin/bash\nexec {} $CHROME_USER_FLAGS \"$@\"\n",
                browser.display()
            ),
        );
        let wrapper = root.file("google-chrome-fast", format!("#!/usr/bin/env bash\nif (($# == 0)); then\nset -- --new-window https://www.google.com/\nfi\nexec {} \"$@\"\n", stable.display()));
        let link = root.0.join("chrome-link");
        symlink("google-chrome-fast", &link).unwrap();
        let result = resolve(&link);
        assert_eq!(
            result.executable.as_ref().map(|(path, _)| path),
            Some(&binary),
            "{}",
            result.reason
        );
        assert_eq!(result.launcher_resolved.as_ref(), Some(&wrapper));
        assert_eq!(result.chain, [link, wrapper, stable, browser, binary]);
        assert!(result.unchanged());
    }

    #[test]
    fn literal_wrapper_paths_support_spaces_and_exec_argv0_without_running_a_shell() {
        let root = Fixture::new();
        let binary = root.file("my browser", b"\x7fELF");
        for script in [
            format!("exec \"{}\" \"$@\"", binary.display()),
            format!("exec -a \"browser\" -- '{}' \"$@\"", binary.display()),
        ] {
            assert_eq!(
                wrapper_target(&script, &root.0).unwrap(),
                Some(binary.clone())
            );
        }
    }

    #[test]
    fn ambiguous_dynamic_relative_and_conditional_wrappers_are_not_guessed_or_executed() {
        let root = Fixture::new();
        let marker = root.0.join("must-not-exist");
        for line in [
            "exec /first\nexec /second",
            "if true; then\nexec /first\nfi",
            "exec /first || /second",
            "exec /first; /second",
            "exec $PROGRAM \"$@\"",
            "exec $(choose-program) \"$@\"",
            "exec ./local-program \"$@\"",
            "exec -unknown /first",
            "HERE=/unverified\nexec \"$HERE/chrome\" \"$@\"",
        ] {
            let path = root.file(
                "wrapper",
                format!("#!/bin/bash\ntouch '{}'\n{line}\n", marker.display()),
            );
            let result = resolve(&path);
            assert!(result.executable.is_none(), "{line}");
            assert!(result.reason.starts_with("Unknown"), "{}", result.reason);
            assert!(!marker.exists());
        }
    }

    #[test]
    fn launch_resolution_rejects_cycles_missing_files_and_oversized_scripts() {
        let root = Fixture::new();
        let first = root.file("first", "");
        let second = root.file("second", format!("#!/bin/bash\nexec {}\n", first.display()));
        std::fs::write(&first, format!("#!/bin/bash\nexec {}\n", second.display())).unwrap();
        let cyclic = resolve(&first);
        assert!(cyclic.executable.is_none() && cyclic.reason.contains("cycle"));
        let oversized = root.file("oversized", vec![b'x'; MAX_WRAPPER_BYTES as usize + 1]);
        assert!(resolve(&oversized).reason.contains("limit"));
        assert!(resolve(&root.0.join("missing")).reason.contains("missing"));
        assert!(resolve(&root.0).reason.contains("regular file"));
        let dangling = root.0.join("dangling");
        symlink("missing", &dangling).unwrap();
        assert!(resolve(&dangling).reason.contains("broken"));
    }

    #[test]
    fn launch_resolution_has_a_fixed_hop_bound() {
        let root = Fixture::new();
        for index in 0..=MAX_LAUNCH_STEPS {
            root.file(
                &format!("wrapper-{index}"),
                format!(
                    "#!/bin/sh\nexec {}\n",
                    root.0.join(format!("wrapper-{}", index + 1)).display()
                ),
            );
        }
        let result = resolve(&root.0.join("wrapper-0"));
        assert!(result.executable.is_none());
        assert!(result.reason.contains("limit"));
        assert_eq!(result.chain.len(), MAX_LAUNCH_STEPS);
    }

    #[test]
    fn changing_a_wrapper_or_installation_link_invalidates_the_captured_chain() {
        let root = Fixture::new();
        let first = root.file("binary-first", b"\x7fELF");
        let second = root.file("binary-second", b"\x7fELF");
        let wrapper = root.file(
            "wrapper",
            format!("#!/bin/bash\nexec {}\n", first.display()),
        );
        let link = root.0.join("installed");
        symlink(&wrapper, &link).unwrap();
        let result = resolve(&link);
        assert!(result.unchanged());
        std::fs::write(
            &wrapper,
            format!("#!/bin/bash\nexec {}\n", second.display()),
        )
        .unwrap();
        assert!(!result.unchanged());
        let result = resolve(&link);
        assert!(result.unchanged());
        std::fs::remove_file(&link).unwrap();
        symlink(&first, &link).unwrap();
        assert!(!result.unchanged());
    }

    #[test]
    fn interpreted_applications_keep_the_script_identity_not_the_runtime_identity() {
        let root = Fixture::new();
        let script = root.file("my-app", "#!/usr/bin/env python3\nprint('not executed')\n");
        let result = resolve(&script);
        let (path, file) = result.executable.as_ref().unwrap();
        assert_eq!(path, &script);
        assert!(!super::super::is_elf(file));
        let version = super::super::probe_executable(file, "python3");
        assert!(version.version.is_none() && version.build_id.is_none());
    }

    #[test]
    fn fifo_and_shell_scripts_named_like_allowlisted_applications_are_never_executed() {
        let root = Fixture::new();
        let marker = root.0.join("must-not-exist");
        let script = root.file("htop", format!("#!/bin/sh\ntouch '{}'\n", marker.display()));
        let version = super::super::probe_executable(&File::open(script).unwrap(), "htop");
        assert!(version.version.is_none() && version.build_id.is_none());
        assert!(!marker.exists());
        let fifo = root.0.join("fifo");
        let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(resolve(&fifo).reason.contains("regular file"));
    }

    #[test]
    #[ignore = "read-only installed application inspection; requires APPLICATIONLAUNCHER_TEST_APPLICATION_EXEC and APPLICATIONLAUNCHER_TEST_APPLICATION_PATH"]
    fn live_installed_application_resolves_the_wrappers_and_uses_the_real_package_metadata() {
        let exec = std::env::var("APPLICATIONLAUNCHER_TEST_APPLICATION_EXEC").unwrap();
        let path =
            PathBuf::from(std::env::var("APPLICATIONLAUNCHER_TEST_APPLICATION_PATH").unwrap());
        let info = inspect_application(&exec, Path::new("/nonexistent-test.desktop"));
        assert_eq!(info.executable.as_ref(), Some(&path), "{}", info.resolution);
        assert!(
            info.version.is_some() && info.build_id.is_some(),
            "{info:?}"
        );
        assert!(
            info.package.is_some()
                && info
                    .installation
                    .as_deref()
                    .unwrap()
                    .contains("Package-managed")
        );
        assert!(
            info.metadata
                .description
                .as_deref()
                .is_some_and(|description| !description.contains("Local custom tool"))
        );
        eprintln!(
            "{}",
            serde_json::json!({
                "executable": info.executable,
                "version": info.version,
                "package": info.package,
                "installation": info.installation,
                "description": info.metadata.description,
                "capabilities": info.metadata.capabilities,
                "build_id": info.build_id,
                "uncompiled_changes": info.uncompiled_changes,
                "launch_chain": info.chain,
            })
        );
        if std::env::var_os("APPLICATIONLAUNCHER_TEST_APPLICATION_PACKAGE_QUERY").is_some() {
            eprintln!("available_package={:?}", info.query_package_version());
        }
    }
}
