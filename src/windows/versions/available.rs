use super::valid_package_name;
use std::collections::HashMap;
use std::fs::File;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const CACHE_TTL: Duration = Duration::from_secs(300);
const FAILURE_TTL: Duration = Duration::from_secs(15);
const MAX_RESULTS: usize = 128;
const MAX_OUTPUT: usize = 64 * 1024;
const MAX_SYNC_LOG: u64 = 1024 * 1024;

fn parse_last_pacman_sync(bytes: &[u8]) -> Option<String> {
    // Ignore an incomplete final line while Pacman may still be writing it.
    let complete = &bytes[..bytes.iter().rposition(|byte| *byte == b'\n')?];
    complete
        .split(|byte| *byte == b'\n')
        .rev()
        .find_map(|line| {
            let line = std::str::from_utf8(line).ok()?;
            let timestamp = line
                .strip_prefix('[')?
                .strip_suffix("] [PACMAN] synchronizing package lists")?;
            let bytes = timestamp.as_bytes();
            if bytes.len() != 24
                || bytes[4] != b'-'
                || bytes[7] != b'-'
                || bytes[10] != b'T'
                || bytes[13] != b':'
                || bytes[16] != b':'
                || !matches!(bytes[19], b'+' | b'-')
                || !bytes.iter().enumerate().all(|(index, byte)| {
                    matches!(index, 4 | 7 | 10 | 13 | 16 | 19) || byte.is_ascii_digit()
                })
            {
                return None;
            }
            Some(format!(
                "{} {} {}:{}",
                &timestamp[..10],
                &timestamp[11..19],
                &timestamp[19..22],
                &timestamp[22..]
            ))
        })
}

fn last_pacman_sync(path: &Path) -> Option<String> {
    type Cache = Option<(super::ExecutableIdentity, Option<String>)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok().filter(|metadata| metadata.is_file())?;
    let identity = super::executable_identity(&metadata);
    if let Some((_, timestamp)) = cache
        .lock()
        .ok()
        .and_then(|cache| cache.as_ref().filter(|(key, _)| *key == identity).cloned())
    {
        return timestamp;
    }

    let offset = metadata.len().saturating_sub(MAX_SYNC_LOG);
    let mut bytes = vec![0; (metadata.len() - offset) as usize];
    file.read_exact_at(&mut bytes, offset).ok()?;
    let tail = if offset > 0 {
        // The bounded tail may start halfway through a log entry.
        &bytes[bytes.iter().position(|byte| *byte == b'\n')? + 1..]
    } else {
        &bytes
    };
    let timestamp = parse_last_pacman_sync(tail);
    if super::executable_identity(&file.metadata().ok()?) != identity {
        return None;
    }
    if let Ok(mut cache) = cache.lock() {
        *cache = Some((identity, timestamp.clone()));
    }
    timestamp
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackageTarget {
    pub(crate) name: String,
    pub(crate) inferred: bool,
}

pub(super) fn target(executable: &Path, installed: Option<&str>) -> Option<PackageTarget> {
    if let Some(name) = installed
        .and_then(|package| package.split_whitespace().next())
        .filter(|name| valid_package_name(name))
    {
        return Some(PackageTarget {
            name: name.into(),
            inferred: false,
        });
    }
    let (name, inferred) = super::installation::package_name(executable)?;
    valid_package_name(&name).then_some(PackageTarget { name, inferred })
}

fn info_command(executable: &Path, package: &str, aur: bool) -> Command {
    let mut command = Command::new(executable);
    command.arg("-Si");
    if aur {
        command.args(["--aur", "--nosudoloop", "--noconfirm"]);
    }
    command
        .args(["--color", "never", "--", package])
        .env("LC_ALL", "C")
        .stdin(Stdio::null());
    command
}

fn parse_info(bytes: &[u8], package: &str) -> Option<(String, String)> {
    if bytes.len() > MAX_OUTPUT {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let (mut repository, mut name, mut version): (Option<&str>, Option<&str>, Option<&str>) =
        (None, None, None);
    // Pacman prints records in configured repository priority order, not version order.
    for line in text.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if name == Some(package)
                && let (Some(repository), Some(version)) = (repository, version)
            {
                return Some((repository.to_owned(), version.to_owned()));
            }
            (repository, name, version) = (None, None, None);
        } else if let Some((key, value)) = line.split_once(':') {
            if !matches!(key.trim(), "Repository" | "Name" | "Version") {
                continue;
            }
            let value = value.trim();
            if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return None;
            }
            match key.trim() {
                "Repository" => repository = Some(value),
                "Name" => name = Some(value),
                "Version" => version = Some(value),
                _ => {}
            }
        }
    }
    None
}

#[derive(Clone)]
struct QueryResult {
    text: String,
    found: bool,
    pacman: bool,
}

impl QueryResult {
    fn display(&self, last_sync: Option<&str>) -> String {
        if self.pacman {
            format!(
                "{}; Last Pacman sync: {}",
                self.text,
                last_sync.unwrap_or("unknown")
            )
        } else {
            self.text.clone()
        }
    }
}

fn query_with(
    package: &str,
    pacman: Option<&Path>,
    paru: Option<&Path>,
    mut run: impl FnMut(Command) -> std::io::Result<Output>,
) -> QueryResult {
    let mut failures = Vec::new();
    for (tool, aur) in [(pacman, false), (paru, true)] {
        let Some(tool) = tool else {
            continue;
        };
        let source = if aur { "Paru/AUR" } else { "Pacman" };
        match run(info_command(tool, package, aur)) {
            Ok(output) if output.stdout.len() + output.stderr.len() > MAX_OUTPUT => {
                failures.push(format!("{source} response too large"));
            }
            Ok(output) if output.status.success() => {
                if let Some((repository, version)) = parse_info(&output.stdout, package)
                    && (!aur || repository.eq_ignore_ascii_case("aur"))
                {
                    return QueryResult {
                        text: format!(
                            "{repository}/{package} {version} ({})",
                            if aur {
                                "Paru/AUR"
                            } else {
                                "Pacman sync database"
                            }
                        ),
                        found: true,
                        pacman: !aur,
                    };
                }
                failures.push(format!("{source} returned no exact package record"));
            }
            Ok(output) => {
                let error = String::from_utf8_lossy(&output.stderr);
                if error.contains(&format!("package '{package}' was not found")) {
                    continue;
                }
                failures.push(format!("{source} query failed"));
            }
            Err(error) => failures.push(format!(
                "{source} {}",
                if error.kind() == std::io::ErrorKind::TimedOut {
                    "timed out"
                } else {
                    "query unavailable"
                }
            )),
        }
    }
    let text = if !failures.is_empty() {
        format!("Unavailable ({})", failures.join("; "))
    } else if pacman.is_none() && paru.is_none() {
        "Unavailable (Pacman and Paru not found)".into()
    } else if paru.is_none() {
        format!("Not found in Pacman sync databases; AUR not checked (Paru unavailable): {package}")
    } else if pacman.is_none() {
        format!("Not found in AUR; Pacman unavailable: {package}")
    } else {
        format!("No exact Pacman/AUR package found: {package}")
    };
    QueryResult {
        text,
        found: false,
        pacman: false,
    }
}

pub(super) fn query(target: &PackageTarget) -> String {
    type Cache = HashMap<(String, Option<super::ExecutableIdentity>), (Instant, QueryResult)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if !valid_package_name(&target.name) {
        return "Unavailable (invalid package name)".into();
    }
    let sync_identity = std::fs::metadata("/var/lib/pacman/sync")
        .ok()
        .map(|metadata| super::executable_identity(&metadata));
    let key = (target.name.clone(), sync_identity);
    let cached = cache.lock().ok().and_then(|cache| {
        cache
            .get(&key)
            .filter(|(expires, _)| *expires > Instant::now())
            .map(|(_, result)| result.clone())
    });
    let result = cached.unwrap_or_else(|| {
        let pacman = applicationlauncher::process::executable_path("pacman");
        let paru = applicationlauncher::process::executable_path("paru");
        // No network or subprocess work runs while holding either the GUI state or cache lock.
        let result = query_with(
            &target.name,
            pacman.as_deref(),
            paru.as_deref(),
            |command| applicationlauncher::process::output_with_timeout(command, QUERY_TIMEOUT),
        );
        if let Ok(mut cache) = cache.lock() {
            cache.retain(|_, (expires, _)| *expires > Instant::now());
            if cache.len() >= MAX_RESULTS
                && let Some(oldest) = cache
                    .iter()
                    .min_by_key(|(_, (expires, _))| *expires)
                    .map(|(key, _)| key.clone())
            {
                cache.remove(&oldest);
            }
            cache.insert(
                key,
                (
                    Instant::now() + if result.found { CACHE_TTL } else { FAILURE_TTL },
                    result.clone(),
                ),
            );
        }
        result
    });
    let last_sync = result
        .pacman
        .then(|| last_pacman_sync(Path::new("/var/log/pacman.log")))
        .flatten();
    let mut text = result.display(last_sync.as_deref());
    if target.inferred {
        text.push_str("; package-name match for local build");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct LogFixture(PathBuf);

    impl LogFixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir().join(format!(
                "launcher-pacman-sync-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&directory).unwrap();
            Self(directory)
        }

        fn log(&self) -> PathBuf {
            self.0.join("pacman.log")
        }
    }

    impl Drop for LogFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn output(success: bool, stdout: &str, stderr: &str) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(if success { 0 } else { 256 }),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn last_pacman_sync_uses_the_latest_complete_sync_record_and_preserves_its_offset() {
        let log = b"[2026-10-07T20:48:01+0100] [PACMAN] synchronizing package lists\n\
            [2026-10-25T01:20:03+0000] [PACMAN] Running 'pacman -Si tmux'\n\
            [2026-10-25T02:21:04-0430] [PACMAN] synchronizing package lists\n\
            [2026-10-25T02:22:05+0000] [ALPM] synchronizing package lists\n\
            [2026-10-25T02:23:06+0000] [PACMAN] synchronizing package lists";
        assert_eq!(
            parse_last_pacman_sync(log).as_deref(),
            Some("2026-10-25 02:21:04 -04:30")
        );
        assert!(parse_last_pacman_sync(b"").is_none());
    }

    #[test]
    fn last_pacman_sync_ignores_corrupt_timestamps_and_non_log_messages() {
        let mut log = b"[2026-10-07T20:48:01+0100] [PACMAN] synchronizing package lists\n".to_vec();
        log.extend_from_slice(
            b"[2026-10-25T02:23:06+0000] [PACMAN] synchronizing package lists extra\n",
        );
        log.extend_from_slice(
            b"[2026-10-25T02:23:06+00\x1b0] [PACMAN] synchronizing package lists\n",
        );
        log.extend_from_slice(
            b"[2026-10-25T02:23:06+00\xff0] [PACMAN] synchronizing package lists\n",
        );
        log.extend_from_slice(b"[invalid] [PACMAN] synchronizing package lists\n");
        assert_eq!(
            parse_last_pacman_sync(&log).as_deref(),
            Some("2026-10-07 20:48:01 +01:00")
        );
    }

    #[test]
    fn last_pacman_sync_cache_tracks_append_truncation_replacement_and_missing_logs() {
        let fixture = LogFixture::new();
        let path = fixture.log();
        assert!(last_pacman_sync(&path).is_none());
        assert!(last_pacman_sync(&fixture.0).is_none());
        std::fs::write(
            &path,
            "[2026-10-07T20:48:01+0100] [PACMAN] synchronizing package lists\n",
        )
        .unwrap();
        for _ in 0..2 {
            assert_eq!(
                last_pacman_sync(&path).as_deref(),
                Some("2026-10-07 20:48:01 +01:00")
            );
        }
        writeln!(
            File::options().append(true).open(&path).unwrap(),
            "[2026-10-25T02:21:04+0000] [PACMAN] synchronizing package lists"
        )
        .unwrap();
        assert_eq!(
            last_pacman_sync(&path).as_deref(),
            Some("2026-10-25 02:21:04 +00:00")
        );
        std::fs::write(
            &path,
            "[2026-10-25T02:22:00+0000] [ALPM] transaction started\n",
        )
        .unwrap();
        assert!(last_pacman_sync(&path).is_none());
        std::fs::rename(&path, fixture.0.join("pacman.log.1")).unwrap();
        std::fs::write(
            &path,
            "[2026-10-26T02:21:04+0000] [PACMAN] synchronizing package lists\n",
        )
        .unwrap();
        assert_eq!(
            last_pacman_sync(&path).as_deref(),
            Some("2026-10-26 02:21:04 +00:00")
        );
    }

    #[test]
    fn last_pacman_sync_log_reads_are_bounded_and_do_not_fabricate_a_date() {
        let fixture = LogFixture::new();
        let path = fixture.log();
        let mut bytes =
            b"[2026-10-07T20:48:01+0100] [PACMAN] synchronizing package lists\n".to_vec();
        bytes.extend_from_slice(&vec![b'x'; MAX_SYNC_LOG as usize]);
        bytes.push(b'\n');
        std::fs::write(&path, &bytes).unwrap();
        assert!(last_pacman_sync(&path).is_none());
        writeln!(
            File::options().append(true).open(&path).unwrap(),
            "[2026-10-25T02:21:04+0000] [PACMAN] synchronizing package lists"
        )
        .unwrap();
        assert_eq!(
            last_pacman_sync(&path).as_deref(),
            Some("2026-10-25 02:21:04 +00:00")
        );
    }

    #[test]
    fn sync_date_is_displayed_next_to_cached_pacman_versions_but_not_aur_or_errors() {
        let result = query_with("tool", Some(Path::new("pacman")), None, |_| {
            Ok(output(
                true,
                "Repository : extra\nName : tool\nVersion : 2.0-1\n",
                "",
            ))
        });
        assert_eq!(
            result.display(Some("2026-10-07 20:48:01 +01:00")),
            "extra/tool 2.0-1 (Pacman sync database); Last Pacman sync: 2026-10-07 20:48:01 +01:00"
        );
        assert!(result.display(None).ends_with("Last Pacman sync: unknown"));
        assert!(
            result
                .clone()
                .display(Some("2026-10-25 02:21:04 +00:00"))
                .ends_with("Last Pacman sync: 2026-10-25 02:21:04 +00:00")
        );
        let aur = query_with("tool", None, Some(Path::new("paru")), |_| {
            Ok(output(
                true,
                "Repository : aur\nName : tool\nVersion : 3.0-1\n",
                "",
            ))
        });
        assert_eq!(aur.display(Some("date")), "aur/tool 3.0-1 (Paru/AUR)");
        let missing = query_with("tool", None, None, |_| panic!("must not execute"));
        assert_eq!(missing.display(Some("date")), missing.text);
    }

    #[test]
    fn package_info_keeps_repository_priority_and_requires_an_exact_package() {
        let text = b"Repository : priority\nName : tool\nVersion : 1.0-1\nDescription : Some text\n\nRepository : extra\nName : tool\nVersion : 9.0-1\n";
        assert_eq!(
            parse_info(text, "tool"),
            Some(("priority".into(), "1.0-1".into()))
        );
        assert!(parse_info(text, "tool-other").is_none());
        assert!(parse_info(b"Repository : extra\nName : tool\n", "tool").is_none());
        let long_description = format!(
            "Repository : extra\nName : tool\nVersion : 1.0\nDescription : {}\n",
            "x".repeat(1024)
        );
        assert!(parse_info(long_description.as_bytes(), "tool").is_some());
        assert!(parse_info(&vec![b'x'; MAX_OUTPUT + 1], "tool").is_none());
    }

    #[test]
    fn package_info_commands_are_literal_read_only_queries_without_refresh_or_install() {
        let command = info_command(Path::new("/test/pacman"), "tool", false);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["-Si", "--color", "never", "--", "tool"]
        );
        let command = info_command(Path::new("/test/paru"), "tool", true);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "-Si",
                "--aur",
                "--nosudoloop",
                "--noconfirm",
                "--color",
                "never",
                "--",
                "tool"
            ]
        );
        for name in [
            "",
            "-Syu",
            "../tool",
            "tool;touch file",
            "tool name",
            "pi/coding-agent",
            "tool\nName : other",
        ] {
            assert!(!valid_package_name(name));
        }
        assert!(valid_package_name("xfce4-terminal-git"));
        assert_eq!(
            target(Path::new("/usr/bin/ssh"), Some("openssh 10.4p1-3")),
            Some(PackageTarget {
                name: "openssh".into(),
                inferred: false
            })
        );
    }

    #[test]
    fn unidentified_local_paths_do_not_guess_packages_from_the_executable_name() {
        for path in ["/unindexed-local/progs", "/unindexed-local/fish"] {
            assert_eq!(target(Path::new(path), None), None);
        }
    }

    #[test]
    fn available_package_prefers_pacman_and_falls_back_to_paru_aur() {
        let mut calls = Vec::new();
        let found = query_with(
            "tool",
            Some(Path::new("pacman")),
            Some(Path::new("paru")),
            |command| {
                calls.push(command.get_program().to_owned());
                Ok(output(
                    true,
                    "Repository : extra\nName : tool\nVersion : 2.0-1\n",
                    "",
                ))
            },
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(found.text, "extra/tool 2.0-1 (Pacman sync database)");
        assert!(found.found);
        calls.clear();
        let found = query_with(
            "tool",
            Some(Path::new("pacman")),
            Some(Path::new("paru")),
            |command| {
                calls.push(command.get_program().to_owned());
                if command.get_program() == "pacman" {
                    Ok(output(false, "", "error: package 'tool' was not found"))
                } else {
                    Ok(output(
                        true,
                        "Repository : aur\nName : tool\nVersion : 3.0-1\n",
                        "",
                    ))
                }
            },
        );
        assert_eq!(calls.len(), 2);
        assert_eq!(found.text, "aur/tool 3.0-1 (Paru/AUR)");
        assert!(found.found);
    }

    #[test]
    fn available_package_distinguishes_absence_timeout_missing_tools_and_bad_responses() {
        let absent = query_with(
            "tool",
            Some(Path::new("pacman")),
            Some(Path::new("paru")),
            |_| Ok(output(false, "", "error: package 'tool' was not found")),
        );
        assert!(!absent.found);
        assert_eq!(absent.text, "No exact Pacman/AUR package found: tool");
        let timeout = query_with("tool", None, Some(Path::new("paru")), |_| {
            Err(std::io::ErrorKind::TimedOut.into())
        });
        assert!(timeout.text.contains("Paru/AUR timed out"));
        let missing = query_with("tool", None, None, |_| panic!("no tool must be executed"));
        assert!(missing.text.contains("not found"));
        let mismatch = query_with("tool", None, Some(Path::new("paru")), |_| {
            Ok(output(
                true,
                "Repository : aur\nName : different\nVersion : 3.0\n",
                "",
            ))
        });
        assert!(!mismatch.found);
        assert!(mismatch.text.starts_with("Unavailable"));
        let oversized = query_with("tool", None, Some(Path::new("paru")), |_| {
            Ok(output(true, &"x".repeat(MAX_OUTPUT + 1), ""))
        });
        assert!(oversized.text.contains("too large"));
    }

    #[test]
    #[ignore = "read-only repository query; requires APPLICATIONLAUNCHER_TEST_PACKAGE_NAME and APPLICATIONLAUNCHER_TEST_PACKAGE_PREFIX"]
    fn live_available_package_version() {
        let name = std::env::var("APPLICATIONLAUNCHER_TEST_PACKAGE_NAME").unwrap();
        let prefix = std::env::var("APPLICATIONLAUNCHER_TEST_PACKAGE_PREFIX").unwrap();
        let requested = std::env::var("APPLICATIONLAUNCHER_TEST_PACKAGE_EXECUTABLE")
            .ok()
            .map(|path| {
                target(Path::new(&path), None)
                    .expect("executable must map to its package through Progs")
            })
            .unwrap_or_else(|| PackageTarget {
                name: name.clone(),
                inferred: false,
            });
        assert_eq!(requested.name, name);
        let result = query(&requested);
        eprintln!("{result}");
        assert!(result.starts_with(&prefix), "{result}");
    }
}
