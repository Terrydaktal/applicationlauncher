use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_LOG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 64 * 1024;
const MAX_ARGUMENTS: usize = 1024;
const MAX_PACKAGES: usize = 65_536;
const MAX_TARGETS: usize = 65_536;
const PARSE_BUDGET: Duration = Duration::from_millis(200);

#[derive(Default)]
pub(super) struct InstallHistory {
    first: BTreeMap<String, Option<Arc<[String]>>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Sync,
    Upgrade,
}

struct Request {
    operation: Operation,
    targets: Vec<String>,
}

impl Request {
    fn parse(command: &str) -> Option<Self> {
        // Pacman's log is not an argv receipt; do not guess through ambiguous quoting.
        let args: Vec<_> = command.split_whitespace().take(MAX_ARGUMENTS + 1).collect();
        if args.len() > MAX_ARGUMENTS
            || Path::new(args.first()?).file_name()?.to_str()? != "pacman"
            || args.iter().any(|arg| {
                arg.len() > 4096
                    || arg
                        .chars()
                        .any(|ch| ch.is_control() || matches!(ch, '\'' | '"' | '\\'))
            })
        {
            return None;
        }
        let mut operation = None;
        let mut targets = Vec::new();
        let mut options = true;
        let mut index = 1;
        while let Some(arg) = args.get(index) {
            index += 1;
            if options && *arg == "--" {
                options = false;
            } else if options && arg.starts_with("--") {
                let (option, value) = arg
                    .split_once('=')
                    .map_or((*arg, None), |(k, v)| (k, Some(v)));
                let selected = match option {
                    "--sync" => Some(Operation::Sync),
                    "--upgrade" => Some(Operation::Upgrade),
                    _ => None,
                };
                if let Some(selected) = selected {
                    if value.is_some() || operation.is_some_and(|current| current != selected) {
                        return None;
                    }
                    operation = Some(selected);
                } else if matches!(
                    option,
                    "--root"
                        | "--dbpath"
                        | "--cachedir"
                        | "--config"
                        | "--gpgdir"
                        | "--hookdir"
                        | "--logfile"
                        | "--arch"
                        | "--sysroot"
                        | "--color"
                        | "--ignore"
                        | "--ignoregroup"
                        | "--overwrite"
                        | "--assume-installed"
                ) {
                    if value.is_none() {
                        args.get(index)?;
                        index += 1;
                    } else if value == Some("") {
                        return None;
                    }
                } else if value.is_some()
                    || !matches!(
                        option,
                        "--noconfirm"
                            | "--confirm"
                            | "--needed"
                            | "--asdeps"
                            | "--asexplicit"
                            | "--refresh"
                            | "--sysupgrade"
                            | "--nodeps"
                            | "--quiet"
                            | "--verbose"
                            | "--disable-sandbox"
                            | "--disable-sandbox-filesystem"
                            | "--disable-sandbox-syscalls"
                            | "--disable-download-timeout"
                            | "--noprogressbar"
                            | "--noscriptlet"
                    )
                {
                    return None;
                }
            } else if options && arg.starts_with('-') {
                if *arg == "-" {
                    return None;
                }
                for (offset, flag) in arg.char_indices().skip(1) {
                    match flag {
                        'S' | 'U' => {
                            let selected = if flag == 'S' {
                                Operation::Sync
                            } else {
                                Operation::Upgrade
                            };
                            if operation.is_some_and(|current| current != selected) {
                                return None;
                            }
                            operation = Some(selected);
                        }
                        'b' | 'r' => {
                            if offset + 1 == arg.len() {
                                args.get(index)?;
                                index += 1;
                            }
                            break;
                        }
                        'y' | 'u' | 'd' | 'q' | 'v' => {}
                        _ => return None,
                    }
                }
            } else {
                targets.push((*arg).to_owned());
            }
        }
        (!targets.is_empty()).then_some(Self {
            operation: operation?,
            targets,
        })
    }

    fn resolve(self, changed: &BTreeMap<String, String>) -> Option<Vec<String>> {
        self.targets
            .into_iter()
            .map(|target| {
                let name = match self.operation {
                    Operation::Sync => {
                        let name = target.rsplit('/').next()?;
                        let name = name.split(['<', '>', '=']).next()?;
                        super::valid_package_name(name).then(|| name.to_owned())?
                    }
                    Operation::Upgrade => {
                        let file = target.split(['?', '#']).next()?.rsplit('/').next()?;
                        let stem = [
                            ".pkg.tar.zst",
                            ".pkg.tar.xz",
                            ".pkg.tar.gz",
                            ".pkg.tar.bz2",
                            ".pkg.tar",
                        ]
                        .into_iter()
                        .find_map(|suffix| file.strip_suffix(suffix))?;
                        let (stem, _arch) = stem.rsplit_once('-')?;
                        let (stem, release) = stem.rsplit_once('-')?;
                        let (name, version) = stem.rsplit_once('-')?;
                        if !super::valid_package_name(name) {
                            return None;
                        }
                        let installed = changed.get(name)?;
                        let installed = installed
                            .split_once(':')
                            .map_or(installed.as_str(), |(_, version)| version);
                        if installed != format!("{version}-{release}") {
                            return None;
                        }
                        name.to_owned()
                    }
                };
                // A --needed target omitted from the transaction did not introduce a new dependency.
                Some(changed.contains_key(&name).then_some(name))
            })
            .collect::<Option<Vec<_>>>()
            .map(|targets| targets.into_iter().flatten().collect())
    }
}

#[derive(Default)]
struct Transaction {
    request: Option<Request>,
    installed: Vec<String>,
    changed: BTreeMap<String, String>,
}

impl InstallHistory {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() as u64 > MAX_LOG_BYTES {
            return Err("Pacman log exceeds the history read limit");
        }
        let deadline = Instant::now() + PARSE_BUDGET;
        let mut history = Self::default();
        let mut request = None;
        let mut transaction = None;
        let mut target_count = 0;
        for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
            if index % 256 == 0 && Instant::now() >= deadline {
                return Err("Pacman history parsing exceeded its time limit");
            }
            if line.len() > MAX_LINE_BYTES {
                return Err("Pacman log contains an oversized history record");
            }
            if line.last() != Some(&b'\n') {
                break;
            }
            let Ok(line) = std::str::from_utf8(line) else {
                continue;
            };
            let Some((timestamp, message)) = line.trim_end_matches(['\r', '\n']).split_once("] ")
            else {
                continue;
            };
            if timestamp
                .strip_prefix('[')
                .and_then(super::available::parse_sync_timestamp)
                .is_none()
            {
                continue;
            }
            if let Some(command) = message
                .strip_prefix("[PACMAN] Running '")
                .and_then(|s| s.strip_suffix('\''))
            {
                request = Request::parse(command);
            } else if message == "[ALPM] transaction started" {
                history.finish(transaction.take(), false, &mut target_count)?;
                transaction = Some(Transaction {
                    request: request.take(),
                    ..Default::default()
                });
            } else if message == "[ALPM] transaction completed" {
                history.finish(transaction.take(), true, &mut target_count)?;
                request = None;
            } else if let Some(message) = message.strip_prefix("[ALPM] ") {
                let Some((action, value)) = message.split_once(' ') else {
                    continue;
                };
                if !matches!(
                    action,
                    "installed" | "reinstalled" | "upgraded" | "downgraded"
                ) {
                    continue;
                }
                let Some((name, version)) = value.split_once(" (") else {
                    continue;
                };
                let Some(version) = version.strip_suffix(')') else {
                    continue;
                };
                let version = version.rsplit(" -> ").next().unwrap_or(version);
                if !super::valid_package_name(name)
                    || version.is_empty()
                    || version.len() > 256
                    || version.chars().any(char::is_whitespace)
                    || version.chars().any(char::is_control)
                {
                    continue;
                }
                if let Some(transaction) = transaction.as_mut() {
                    transaction.changed.insert(name.into(), version.into());
                    if action == "installed" {
                        transaction.installed.push(name.into());
                    }
                    if transaction.changed.len() + transaction.installed.len() > MAX_PACKAGES {
                        return Err("Pacman transaction exceeds the history package limit");
                    }
                } else if action == "installed" {
                    history.first.entry(name.into()).or_insert(None);
                }
            }
            if history.first.len() > MAX_PACKAGES {
                return Err("Pacman history exceeds the package limit");
            }
        }
        history.finish(transaction, false, &mut target_count)?;
        Ok(history)
    }

    fn finish(
        &mut self,
        transaction: Option<Transaction>,
        complete: bool,
        target_count: &mut usize,
    ) -> Result<(), &'static str> {
        let Some(transaction) = transaction else {
            return Ok(());
        };
        let targets = complete
            .then_some(transaction.request)
            .flatten()
            .and_then(|request| request.resolve(&transaction.changed));
        *target_count += targets.as_ref().map_or(0, Vec::len);
        if *target_count > MAX_TARGETS
            || self.first.len() + transaction.installed.len() > MAX_PACKAGES
        {
            return Err("Pacman history exceeds the package/target limit");
        }
        let targets: Option<Arc<[String]>> = targets.map(Arc::from);
        for name in transaction.installed {
            self.first.entry(name).or_insert_with(|| targets.clone());
        }
        Ok(())
    }

    pub(super) fn first_targets(&self, package: &str) -> Result<&[String], &'static str> {
        self.first
            .get(package)
            .ok_or("no recorded first installation in the available Pacman log")?
            .as_deref()
            .ok_or("first installation request is incomplete or unsupported")
    }
}

type Cache = Option<(
    PathBuf,
    super::ExecutableIdentity,
    Result<Arc<InstallHistory>, &'static str>,
)>;

pub(super) fn load(path: &Path) -> Result<Arc<InstallHistory>, &'static str> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    load_cached(path, CACHE.get_or_init(Default::default))
}

fn load_cached(path: &Path, cache: &Mutex<Cache>) -> Result<Arc<InstallHistory>, &'static str> {
    let mut file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "Pacman installation history unavailable")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Pacman log metadata unavailable")?;
    if !metadata.is_file() {
        return Err("Pacman installation history is not a regular file");
    }
    let identity = super::executable_identity(&metadata);
    if let Some(result) = cache.lock().ok().and_then(|cache| {
        cache
            .as_ref()
            .filter(|(cached_path, cached_identity, _)| {
                cached_path == path && *cached_identity == identity
            })
            .map(|(_, _, history)| history.clone())
    }) {
        return result;
    }
    let result = if metadata.len() > MAX_LOG_BYTES {
        Err("Pacman log exceeds the history read limit")
    } else {
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_LOG_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Pacman installation history unreadable")?;
        InstallHistory::parse(&bytes).map(Arc::new)
    };
    if file
        .metadata()
        .map(|current| super::executable_identity(&current) != identity)
        .unwrap_or(true)
    {
        return Err("Pacman log changed during the history read");
    }
    if result.as_ref().err().copied() == Some("Pacman history parsing exceeded its time limit") {
        return result;
    }
    if let Ok(mut cache) = cache.lock() {
        *cache = Some((path.to_owned(), identity, result.clone()));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn records(messages: &[&str]) -> String {
        messages
            .iter()
            .map(|message| format!("[2026-05-19T19:28:13+0100] {message}\n"))
            .collect()
    }

    fn transaction(command: &str, events: &[&str]) -> String {
        let mut log = records(&[
            &format!("[PACMAN] Running '{command}'"),
            "[ALPM] transaction started",
        ]);
        log.push_str(&records(events));
        log.push_str(&records(&["[ALPM] transaction completed"]));
        log
    }

    #[test]
    fn first_completed_installation_keeps_request_order_and_ignores_later_reinstalls() {
        let mut log = transaction(
            "/usr/bin/pacman -Sy --needed -- extra/zeta alpha",
            &[
                "[ALPM] installed shared-lib (1.0-1)",
                "[ALPM] installed alpha (1.0-1)",
                "[ALPM] installed zeta (1.0-1)",
            ],
        );
        log.push_str(&transaction(
            "pacman -S beta",
            &[
                "[ALPM] reinstalled shared-lib (1.0-1)",
                "[ALPM] upgraded zeta (1.0-1 -> 2.0-1)",
                "[ALPM] installed beta (1.0-1)",
            ],
        ));
        log.push_str(&transaction(
            "pacman -S alpha",
            &[
                "[ALPM] installed shared-lib (2.0-1)",
                "[ALPM] reinstalled alpha (1.0-1)",
            ],
        ));
        let history = InstallHistory::parse(log.as_bytes()).unwrap();
        for name in ["shared-lib", "zeta", "alpha"] {
            assert_eq!(history.first_targets(name).unwrap(), ["zeta", "alpha"]);
        }
        assert_eq!(history.first_targets("beta").unwrap(), ["beta"]);
        assert!(history.first_targets("missing").is_err());
        assert!(Arc::ptr_eq(
            history.first["shared-lib"].as_ref().unwrap(),
            history.first["zeta"].as_ref().unwrap()
        ));
    }

    #[test]
    fn installer_options_and_versions_are_not_misidentified_as_targets() {
        for command in [
            "pacman -b /mnt/var/lib/pacman -r /mnt -Sy --config=/tmp/conf --cachedir=/mnt/cache --disable-sandbox --noconfirm base base-devel",
            "pacman --sync --root /mnt --dbpath=/mnt/db --color never -- base base-devel",
            "pacman -b/mnt/db -r/mnt -S base>=3 base-devel",
        ] {
            let log = transaction(
                command,
                &[
                    "[ALPM] installed bash (1.0-1)",
                    "[ALPM] installed base (3-3)",
                    "[ALPM] installed base-devel (1-2)",
                ],
            );
            assert_eq!(
                InstallHistory::parse(log.as_bytes())
                    .unwrap()
                    .first_targets("bash")
                    .unwrap(),
                ["base", "base-devel"],
                "{command}"
            );
        }
    }

    #[test]
    fn archive_targets_are_matched_to_transaction_names_and_versions_including_epochs() {
        for command in [
            "pacman --upgrade --noconfirm -- /cache/my-app-2.0-3-x86_64.pkg.tar.zst /cache/other-4.0-1-any.pkg.tar.xz",
            "pacman -U https://example.invalid/my-app-2.0-3-x86_64.pkg.tar.zst?token=secret /cache/other-4.0-1-any.pkg.tar.xz",
        ] {
            let log = transaction(
                command,
                &[
                    "[ALPM] installed dependency (1.0-1)",
                    "[ALPM] upgraded my-app (1.0-1 -> 1:2.0-3)",
                    "[ALPM] reinstalled other (4.0-1)",
                ],
            );
            let history = InstallHistory::parse(log.as_bytes()).unwrap();
            assert_eq!(
                history.first_targets("dependency").unwrap(),
                ["my-app", "other"]
            );
            assert!(
                !history
                    .first_targets("dependency")
                    .unwrap()
                    .join(" ")
                    .contains("secret")
            );
        }
        let mismatch = transaction(
            "pacman -U /cache/app-1.0-1-any.pkg.tar.zst",
            &[
                "[ALPM] installed dependency (1.0-1)",
                "[ALPM] installed app (2.0-1)",
            ],
        );
        assert!(
            InstallHistory::parse(mismatch.as_bytes())
                .unwrap()
                .first_targets("dependency")
                .is_err()
        );
    }

    #[test]
    fn skipped_needed_targets_and_concurrent_queries_do_not_steal_attribution() {
        let log = transaction(
            "pacman -S --needed unrelated root",
            &[
                "[ALPM] installed library (1.0-1)",
                "[PACMAN] Running 'pacman -Qi other'",
                "[ALPM] upgraded root (1.0-1 -> 2.0-1)",
            ],
        );
        assert_eq!(
            InstallHistory::parse(log.as_bytes())
                .unwrap()
                .first_targets("library")
                .unwrap(),
            ["root"]
        );
    }

    #[test]
    fn unsupported_or_incomplete_first_installs_are_not_replaced_by_later_valid_commands() {
        for first in [
            records(&["[ALPM] installed dependency (1.0-1)"]),
            records(&[
                "[ALPM] transaction started",
                "[ALPM] installed dependency (1.0-1)",
                "[ALPM] transaction completed",
            ]),
            records(&[
                "[PACMAN] Running 'pacman -S unknown-root'",
                "[ALPM] transaction started",
                "[ALPM] installed dependency (1.0-1)",
            ]),
            transaction(
                "pacman -S --unknown-option root",
                &["[ALPM] installed dependency (1.0-1)"],
            ),
            transaction("pacman -S -", &["[ALPM] installed dependency (1.0-1)"]),
            transaction(
                "pacman -U '/cache/a b.pkg.tar.zst'",
                &["[ALPM] installed dependency (1.0-1)"],
            ),
        ] {
            let later = transaction(
                "pacman -S root",
                &[
                    "[ALPM] installed dependency (2.0-1)",
                    "[ALPM] installed root (1.0-1)",
                ],
            );
            let log = first + &later;
            assert!(
                InstallHistory::parse(log.as_bytes())
                    .unwrap()
                    .first_targets("dependency")
                    .is_err(),
                "{log}"
            );
        }
    }

    #[test]
    fn partial_last_lines_and_scriptlet_lookalikes_do_not_invent_installations() {
        let mut log = transaction(
            "pacman -S root",
            &[
                "[ALPM] installed root (1.0-1)",
                "[ALPM-SCRIPTLET] installed fake (1.0-1)",
            ],
        );
        log.push_str("[invalid] [ALPM] installed corrupt (1.0-1)\n");
        log.push_str("[2026-05-19T19:28:13+0100] [ALPM] installed partial (1.0-1)");
        let history = InstallHistory::parse(log.as_bytes()).unwrap();
        assert_eq!(history.first_targets("root").unwrap(), ["root"]);
        for name in ["fake", "corrupt", "partial"] {
            assert!(!history.first.contains_key(name));
        }
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "launcher-install-history-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn cache_reuses_parsed_logs_and_tracks_append_truncation_and_replacement() {
        let fixture = Fixture::new();
        let path = fixture.0.join("pacman.log");
        let cache = Mutex::new(None);
        let read = || load_cached(&path, &cache).unwrap();
        let first = transaction(
            "pacman -S root",
            &[
                "[ALPM] installed dependency (1.0-1)",
                "[ALPM] installed root (1.0-1)",
            ],
        );
        std::fs::write(&path, &first).unwrap();
        let before = read();
        assert!(Arc::ptr_eq(&before, &read()));
        let second = transaction("pacman -S later", &["[ALPM] installed later (1.0-1)"]);
        File::options()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(second.as_bytes())
            .unwrap();
        let after = read();
        assert!(!Arc::ptr_eq(&before, &after));
        assert_eq!(after.first_targets("dependency").unwrap(), ["root"]);
        assert_eq!(after.first_targets("later").unwrap(), ["later"]);
        std::fs::write(&path, "").unwrap();
        assert!(read().first.is_empty());
        std::fs::rename(&path, fixture.0.join("old.log")).unwrap();
        std::fs::write(&path, first).unwrap();
        assert_eq!(read().first_targets("dependency").unwrap(), ["root"]);
    }

    #[test]
    fn missing_symlink_nonregular_and_oversized_logs_fail_closed() {
        let fixture = Fixture::new();
        let path = fixture.0.join("pacman.log");
        assert!(load(&path).is_err());
        assert!(load(&fixture.0).is_err());
        File::create(&path)
            .unwrap()
            .set_len(MAX_LOG_BYTES + 1)
            .unwrap();
        assert_eq!(
            load(&path).err(),
            Some("Pacman log exceeds the history read limit")
        );
        let alias = fixture.0.join("link");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(load(&alias).is_err());
        assert!(InstallHistory::parse(&vec![b'x'; MAX_LOG_BYTES as usize + 1]).is_err());
        assert!(InstallHistory::parse(&vec![b'x'; MAX_LINE_BYTES + 1]).is_err());
        let excessive = format!("pacman -S {}", "root ".repeat(MAX_ARGUMENTS));
        assert!(Request::parse(&excessive).is_none());
    }
}
