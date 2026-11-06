use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const CACHE_TTL: Duration = Duration::from_secs(300);
const FAILURE_TTL: Duration = Duration::from_secs(15);
const MAX_RESULTS: usize = 128;
const MAX_OUTPUT: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NpmTarget {
    pub(crate) name: String,
    pub(crate) upstream: bool,
}

impl NpmTarget {
    pub(crate) fn package(name: &str) -> Option<Self> {
        valid_name(name).then(|| Self {
            name: name.into(),
            upstream: false,
        })
    }
}

pub(super) fn valid_name(name: &str) -> bool {
    let component = |part: &str| {
        part.as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    !name.is_empty()
        && name.len() <= 214
        && if let Some(scoped) = name.strip_prefix('@') {
            scoped
                .split_once('/')
                .is_some_and(|(scope, package)| component(scope) && component(package))
        } else {
            component(name)
        }
}

fn native_upstream(name: &str, version: Option<&str>) -> Option<NpmTarget> {
    // A native fork is not an npm installation. Require its version identity,
    // not just a PATH command named codex, and label the registry as upstream.
    if matches!(name, "codex" | "codex-code-mode")
        && version
            .and_then(|version| version.strip_prefix("codex-cli "))
            .is_some_and(valid_version)
    {
        Some(NpmTarget {
            name: "@openai/codex".into(),
            upstream: true,
        })
    } else {
        None
    }
}

pub(super) fn target(executable: &Path, version: Option<&str>) -> Option<NpmTarget> {
    if let Some(application) = super::node::package_for_entry(
        executable,
        None,
        "Executable matches package bin entry (installed metadata)",
    ) {
        return NpmTarget::package(&application.package);
    }
    if let Some(name) = super::installation::npm_package(executable) {
        return NpmTarget::package(&name);
    }
    native_upstream(executable.file_name()?.to_str()?, version)
}

fn valid_version(version: &str) -> bool {
    if version.len() > 128 || !version.is_ascii() {
        return false;
    }
    let (version, build) = version
        .split_once('+')
        .map_or((version, None), |(version, build)| (version, Some(build)));
    let (core, pre) = version.split_once('-').unwrap_or((version, ""));
    let numbers: Vec<_> = core.split('.').collect();
    let identifiers = |suffix: &str| {
        suffix.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    };
    numbers.len() == 3
        && numbers
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && (!version.contains('-') || identifiers(pre))
        && build.is_none_or(identifiers)
}

fn view_command(npm: &Path, name: &str) -> Command {
    let mut command = Command::new(npm);
    command
        .args([
            "--global",
            "--ignore-scripts",
            "--no-update-notifier",
            "--fetch-retries=0",
            "--fetch-timeout=2000",
            "--logs-max=0",
            "--prefer-online",
            "--json",
            "view",
            "--",
            name,
            "dist-tags.latest",
        ])
        // Do not load a viewed application's project-local npm configuration.
        .current_dir("/")
        .stdin(Stdio::null());
    command
}

fn query_with(
    name: &str,
    npm: Option<&Path>,
    run: impl FnOnce(Command) -> std::io::Result<Output>,
) -> Result<String, &'static str> {
    if !valid_name(name) {
        return Err("invalid npm package identity");
    }
    let npm = npm.ok_or("npm is not installed or not on PATH")?;
    let output = run(view_command(npm, name)).map_err(|error| {
        if error.kind() == std::io::ErrorKind::TimedOut {
            "npm registry lookup timed out"
        } else {
            "npm registry lookup could not run"
        }
    })?;
    // Never display npm stderr: registry errors may contain credentials or URLs.
    if !output.status.success() {
        return Err("npm registry lookup failed (offline, unavailable or unauthorized package)");
    }
    if output.stdout.len() > MAX_OUTPUT {
        return Err("npm registry response exceeds the inspection limit");
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| "npm registry did not return a latest version")?;
    // npm releases differ: some return a scalar, others a one-element array.
    let version = value
        .as_str()
        .or_else(|| {
            let values = value.as_array()?;
            (values.len() == 1).then(|| values[0].as_str()).flatten()
        })
        .ok_or("npm registry did not return one latest version")?;
    if !valid_version(version) {
        return Err("npm registry returned an invalid latest version");
    }
    Ok(version.to_owned())
}

struct CachedVersion {
    at: Instant,
    result: Result<String, &'static str>,
}

type Cache = HashMap<String, CachedVersion>;

fn cached_query(
    cache: &Mutex<Cache>,
    name: &str,
    now: Instant,
    query: impl FnOnce() -> Result<String, &'static str>,
) -> Result<String, &'static str> {
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(name)
        .filter(|hit| {
            now.saturating_duration_since(hit.at)
                < if hit.result.is_ok() {
                    CACHE_TTL
                } else {
                    FAILURE_TTL
                }
        })
    {
        return hit.result.clone();
    }
    // Network/process work never holds the cache mutex or blocks other popups.
    let result = query();
    let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
    if cache.len() >= MAX_RESULTS
        && let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, hit)| hit.at)
            .map(|(name, _)| name.clone())
    {
        cache.remove(&oldest);
    }
    cache.insert(
        name.into(),
        CachedVersion {
            at: now,
            result: result.clone(),
        },
    );
    result
}

pub(crate) fn query(target: &NpmTarget) -> String {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    if !valid_name(&target.name) {
        return "Unavailable (invalid npm package identity)".into();
    }
    let result = cached_query(
        CACHE.get_or_init(Default::default),
        &target.name,
        Instant::now(),
        || {
            let npm = applicationlauncher::process::executable_path("npm");
            query_with(&target.name, npm.as_deref(), |command| {
                applicationlauncher::process::output_with_timeout(command, QUERY_TIMEOUT)
            })
        },
    );
    match result {
        Ok(version) => format!(
            "{} {version} (npm latest{})",
            target.name,
            if target.upstream {
                "; upstream reference"
            } else {
                ""
            }
        ),
        Err(reason) => format!("Unavailable ({reason})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn output(status: i32, stdout: &[u8]) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(status << 8),
            stdout: stdout.into(),
            stderr: b"private-registry://username:secret@host".to_vec(),
        }
    }

    #[test]
    fn npm_identities_are_packages_not_commands_ranges_paths_or_urls() {
        for name in [
            "typescript",
            "@openai/codex",
            "@earendil-works/pi-coding-agent",
            "a.b_c-d",
        ] {
            assert!(valid_name(name), "{name}");
        }
        for name in [
            "",
            "--version",
            "@scope",
            "@/pkg",
            "@scope/a/b",
            "x@latest",
            "file:foo",
            "git+ssh://foo",
            "../foo",
            "https://example.com",
            "foo bar",
            "foo\nbar",
            "_private",
        ] {
            assert!(!valid_name(name), "{name}");
        }
        assert!(!valid_name(&"x".repeat(215)));
    }

    #[test]
    fn native_codex_has_an_explicit_upstream_reference_not_an_npm_installation() {
        let target = native_upstream("codex", Some("codex-cli 0.159.1")).unwrap();
        assert_eq!(target.name, "@openai/codex");
        assert!(target.upstream);
        assert!(native_upstream("other", Some("codex-cli 0.159.1")).is_none());
        assert!(native_upstream("codex", Some("unrelated 0.159.1")).is_none());
        assert!(native_upstream("codex", None).is_none());
    }

    #[test]
    fn latest_tag_accepts_scalar_or_single_result_not_a_version_list() {
        for bytes in [br#""1.2.3""#.as_slice(), br#"["1.2.3"]"#] {
            assert_eq!(
                query_with("pkg", Some(Path::new("npm")), |_| Ok(output(0, bytes))),
                Ok("1.2.3".into())
            );
        }
        for version in ["0.159.1", "1.2.3-rc.1", "1.2.3-fork0.1+build.5"] {
            assert!(valid_version(version));
        }
        for version in ["1", "1.2", "1.2.3+", "1.2.3-", "1.2.3+x+y", "1.2.3\n"] {
            assert!(!valid_version(version), "{version}");
        }
    }

    #[test]
    fn npm_view_is_read_only_bounded_and_ignores_project_hooks() {
        let version = query_with(
            "@openai/codex",
            Some(Path::new("/usr/bin/npm")),
            |command| {
                let args: Vec<_> = command
                    .get_args()
                    .map(|arg| arg.to_str().unwrap())
                    .collect();
                assert_eq!(command.get_current_dir(), Some(Path::new("/")));
                for flag in [
                    "--global",
                    "--ignore-scripts",
                    "--no-update-notifier",
                    "--fetch-retries=0",
                    "--fetch-timeout=2000",
                    "--logs-max=0",
                ] {
                    assert!(args.contains(&flag));
                }
                assert_eq!(
                    &args[args.len() - 4..],
                    &["view", "--", "@openai/codex", "dist-tags.latest"]
                );
                Ok(output(0, br#""0.160.0""#))
            },
        )
        .unwrap();
        assert_eq!(version, "0.160.0");
    }

    #[test]
    fn malformed_offline_and_oversized_responses_are_bounded_and_redacted() {
        for bytes in [
            b"".as_slice(),
            br#"{"error":"secret"}"#,
            br#""1.2.3\nsecret""#,
            br#""not-a-version""#,
            br#"["1.2.3","1.2.4"]"#,
        ] {
            assert!(query_with("pkg", Some(Path::new("npm")), |_| Ok(output(0, bytes))).is_err());
        }
        assert!(
            query_with("pkg", Some(Path::new("npm")), |_| Ok(output(
                0,
                &vec![b'x'; MAX_OUTPUT + 1]
            )))
            .is_err()
        );
        assert!(
            !query_with("pkg", Some(Path::new("npm")), |_| Ok(output(1, b"secret")))
                .unwrap_err()
                .contains("secret")
        );
        assert_eq!(
            query_with("pkg", None, |_| unreachable!()),
            Err("npm is not installed or not on PATH")
        );
        assert_eq!(
            query_with("pkg", Some(Path::new("npm")), |_| Err(
                std::io::ErrorKind::TimedOut.into()
            )),
            Err("npm registry lookup timed out")
        );
        assert!(query_with("--bad", Some(Path::new("npm")), |_| unreachable!()).is_err());
    }

    #[test]
    #[ignore = "read-only live registry check; requires npm and network access"]
    fn installed_npm_registry_reports_scoped_tool_versions() {
        for name in ["@openai/codex", "@earendil-works/pi-coding-agent"] {
            let target = NpmTarget::package(name).unwrap();
            let result = query(&target);
            println!("{result}");
            assert!(result.starts_with(&format!("{name} ")), "{result}");
            assert!(result.ends_with("(npm latest)"), "{result}");
            assert_eq!(query(&target), result);
        }
    }

    #[test]
    fn cache_has_separate_success_failure_expiry_and_a_fixed_bound() {
        let cache = Mutex::new(Cache::default());
        let now = Instant::now();
        assert_eq!(
            cached_query(&cache, "pkg", now, || Ok("1.2.3".into())),
            Ok("1.2.3".into())
        );
        assert!(
            cached_query(
                &cache,
                "pkg",
                now + CACHE_TTL - Duration::from_secs(1),
                || unreachable!()
            )
            .is_ok()
        );
        assert!(
            cached_query(&cache, "pkg", now + CACHE_TTL, || {
                assert!(cache.try_lock().is_ok());
                Err("offline")
            })
            .is_err()
        );
        assert!(
            cached_query(
                &cache,
                "pkg",
                now + CACHE_TTL + FAILURE_TTL - Duration::from_secs(1),
                || unreachable!()
            )
            .is_err()
        );
        assert!(
            cached_query(&cache, "pkg", now + CACHE_TTL + FAILURE_TTL, || Ok(
                "2.0.0".into()
            ))
            .is_ok()
        );
        for index in 0..MAX_RESULTS + 3 {
            cached_query(&cache, &format!("pkg-{index}"), now, || Ok("1.0.0".into())).unwrap();
        }
        assert_eq!(cache.lock().unwrap().len(), MAX_RESULTS);
    }
}
