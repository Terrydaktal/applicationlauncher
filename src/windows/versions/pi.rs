//! Read Pi's explicit startup receipt, never infer loaded JavaScript from Node's ELF.
use std::fs::{File, Metadata};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::node::NodeApplication;

pub(crate) const PACKAGE: &str = "@earendil-works/pi-coding-agent";
const MAX_BYTES: u64 = 512 * 1024;
const MAX_AGE_MS: u64 = 90_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum BuildStatus {
    Current,
    RestartRequired,
    InstallationUpdateRequired,
    #[default]
    Unknown,
}

impl BuildStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Current => "Current",
            Self::RestartRequired => "Restart required",
            Self::InstallationUpdateRequired => "Installation needs updating",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PiBuildEvidence {
    pub(crate) status: BuildStatus,
    pub(crate) reason: String,
    pub(crate) running_version: Option<String>,
    pub(crate) running_entry: Option<PathBuf>,
    pub(crate) running_sdk: Option<String>,
    pub(crate) running_extensions: Option<String>,
    pub(crate) latest_sdk: Option<String>,
    pub(crate) latest_extensions: Option<String>,
    pub(crate) patches: Option<String>,
    pub(crate) checked_at_ms: Option<u64>,
    pub(crate) check_age_seconds: Option<u64>,
    pub(crate) scope: Option<String>,
    pub(crate) extension_count: Option<usize>,
    pub(crate) record_path: Option<PathBuf>,
    pub(crate) manifest_path: Option<PathBuf>,
}

impl PiBuildEvidence {
    fn unknown(reason: &str, record_path: Option<PathBuf>) -> Self {
        Self {
            reason: reason.into(),
            record_path,
            ..Default::default()
        }
    }
}

fn text<'a>(value: &'a Value, key: &str, max: usize) -> Option<&'a str> {
    let text = value.get(key)?.as_str()?;
    (!text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)).then_some(text)
}

fn hash(value: &Value, key: &str) -> Option<String> {
    let value = text(value, key, 64)?;
    (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

fn absolute(value: &Value, key: &str) -> Option<PathBuf> {
    let value = PathBuf::from(text(value, key, 4096)?);
    (value.is_absolute()
        && !value
            .components()
            .any(|p| matches!(p, Component::ParentDir)))
    .then_some(value)
}

fn same_metadata(left: &Metadata, right: &Metadata) -> bool {
    (
        left.dev(),
        left.ino(),
        left.len(),
        left.mtime(),
        left.mtime_nsec(),
        left.ctime(),
        left.ctime_nsec(),
    ) == (
        right.dev(),
        right.ino(),
        right.len(),
        right.mtime(),
        right.mtime_nsec(),
        right.ctime(),
        right.ctime_nsec(),
    )
}

fn read_json(path: &Path, uid: u32, private: bool) -> Option<Value> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.len() > MAX_BYTES
        || metadata.nlink() != 1
        || metadata.mode() & 0o022 != 0
        || (private && metadata.mode() & 0o077 != 0)
    {
        return None;
    }
    let mut bytes = Vec::new();
    (&file).take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_BYTES || !same_metadata(&metadata, &file.metadata().ok()?) {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn private_directory(path: &Path, uid: u32) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o077 == 0
    })
}

fn process_start(pid: i32, uid: u32) -> Option<u64> {
    let proc = PathBuf::from(format!("/proc/{pid}"));
    if std::fs::metadata(&proc).ok()?.uid() != uid {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(proc.join("stat"))
        .ok()?
        .take(8193)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 8192 {
        return None;
    }
    let stat = std::str::from_utf8(&bytes).ok()?;
    if stat.split_whitespace().next()?.parse::<i32>().ok()? != pid {
        return None;
    }
    stat[stat.rfind(')')? + 1..]
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

fn hash_list(value: &Value, key: &str, max: usize) -> bool {
    value
        .get(key)
        .and_then(Value::as_array)
        .is_some_and(|items| {
            !items.is_empty()
                && items.len() <= max
                && items.iter().all(|item| {
                    let Some(path) = text(item, "path", 4096) else {
                        return false;
                    };
                    let path = Path::new(path);
                    !path.is_absolute()
                        && path.components().all(|component| {
                            matches!(component, Component::Normal(_))
                                // Patch sources can live outside the installed package.
                                // We only validate this inventory, never open these paths.
                                || (key == "patches" && component == Component::ParentDir)
                        })
                        && hash(item, "sha256").is_some()
                })
        })
}

pub(crate) fn probe(application: &NodeApplication, pid: i32) -> PiBuildEvidence {
    let uid = unsafe { libc::geteuid() };
    let Some(start_ticks) = process_start(pid, uid) else {
        return PiBuildEvidence::unknown(
            "Process identity is unavailable or belongs to another user",
            None,
        );
    };
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else {
        return PiBuildEvidence::unknown("XDG_RUNTIME_DIR is unavailable", None);
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64);
    let evidence = inspect(application, pid, start_ticks, uid, &runtime, now_ms);
    if process_start(pid, uid) == Some(start_ticks) {
        evidence
    } else {
        PiBuildEvidence::unknown(
            "Process exited or PID was reused during inspection",
            evidence.record_path,
        )
    }
}

fn inspect(
    application: &NodeApplication,
    pid: i32,
    start_ticks: u64,
    uid: u32,
    runtime: &Path,
    now_ms: u64,
) -> PiBuildEvidence {
    let directory = runtime.join("pi/processes");
    let path = directory.join(format!("{pid}-{start_ticks}.json"));
    let unknown = |reason: &str| PiBuildEvidence::unknown(reason, Some(path.clone()));
    if !runtime.is_absolute()
        || ![runtime, &runtime.join("pi"), &directory]
            .into_iter()
            .all(|path| private_directory(path, uid))
    {
        return unknown(
            "Runtime receipt directory is missing or not private and owned by this user",
        );
    }
    let Some(record) = read_json(&path, uid, true) else {
        return unknown(
            "No valid startup receipt; restart Pi to publish its loaded build identity",
        );
    };
    if record.get("schema").and_then(Value::as_str) != Some("coherence.pi.process-build.v1")
        || record.get("pid").and_then(Value::as_i64) != Some(pid as i64)
        || record.get("uid").and_then(Value::as_u64) != Some(uid as u64)
        || record.get("start_ticks").and_then(Value::as_str)
            != Some(start_ticks.to_string().as_str())
        || record.get("package").and_then(Value::as_str) != Some(PACKAGE)
    {
        return unknown("Startup receipt does not match this process identity");
    }
    let Some(version) = text(&record, "version", 128) else {
        return unknown("Invalid running version in startup receipt");
    };
    let (Some(entry), Some(package_manifest)) = (
        absolute(&record, "entry_point"),
        absolute(&record, "package_manifest"),
    ) else {
        return unknown("Invalid running application paths in startup receipt");
    };
    if package_manifest
        .parent()
        .map(|root| root.join("dist/cli.js"))
        != Some(entry.clone())
    {
        return unknown("Running application paths are inconsistent");
    }
    let (Some(running), Some(latest)) = (record.get("running"), record.get("latest")) else {
        return unknown("Incomplete startup receipt");
    };
    let (Some(running_sdk), Some(running_extensions), Some(latest_sdk), Some(latest_extensions)) = (
        hash(running, "sdk_fingerprint"),
        hash(running, "extensions_fingerprint"),
        hash(latest, "sdk_fingerprint"),
        hash(latest, "extensions_fingerprint"),
    ) else {
        return unknown("Incomplete code or extension fingerprint evidence");
    };
    let Some(extensions) = running
        .get("extensions")
        .and_then(Value::as_array)
        .filter(|list| {
            list.len() <= 512
                && list.iter().all(|value| {
                    value.as_str().is_some_and(|value| {
                        value.len() <= 4096
                            && !value.chars().any(char::is_control)
                            && Path::new(value).is_absolute()
                    })
                })
        })
    else {
        return unknown("Invalid loaded extension list");
    };
    let Some(scope) = text(&record, "scope", 4096) else {
        return unknown("Missing fingerprint coverage statement");
    };
    let (Some(checked), Some(captured)) = (
        latest.get("checked_at_unix_ms").and_then(Value::as_u64),
        record.get("captured_at_unix_ms").and_then(Value::as_u64),
    ) else {
        return unknown("Missing receipt timestamps");
    };
    let mut evidence = PiBuildEvidence {
        reason: "Receipt comparison could not be established".into(),
        running_version: Some(version.into()),
        running_entry: Some(entry),
        running_sdk: Some(running_sdk),
        running_extensions: Some(running_extensions),
        latest_sdk: Some(latest_sdk),
        latest_extensions: Some(latest_extensions),
        checked_at_ms: Some(checked),
        check_age_seconds: Some(now_ms.saturating_sub(checked) / 1000),
        scope: Some(scope.into()),
        extension_count: Some(extensions.len()),
        record_path: Some(path),
        ..Default::default()
    };
    if checked > now_ms.saturating_add(5_000)
        || captured > checked.saturating_add(1_000)
        || now_ms.saturating_sub(checked) > MAX_AGE_MS
    {
        evidence.reason = "Startup comparison is stale or has inconsistent timestamps".into();
        return evidence;
    }
    let (Some(manifest), Some(latest_entry)) = (
        absolute(latest, "manifest_path"),
        absolute(latest, "entry_point"),
    ) else {
        evidence.reason = "Latest installed manifest is unavailable".into();
        return evidence;
    };
    evidence.manifest_path = Some(manifest.clone());
    let Some(root) = application.manifest.parent() else {
        return evidence;
    };
    if manifest != root.join("coherence-build.json") || latest_entry != application.entry_point {
        evidence.reason = "Selected installed Pi and receipt's latest installation differ; reopen details after installation settles".into();
        return evidence;
    }
    let Some(installed) = read_json(&manifest, uid, false) else {
        evidence.reason = "Installed build manifest is missing, unsafe or malformed".into();
        return evidence;
    };
    if installed.get("schema").and_then(Value::as_str) != Some("coherence.pi.installed-build.v1")
        || installed.get("package").and_then(Value::as_str) != Some(PACKAGE)
        || installed.get("version").and_then(Value::as_str)
            != Some(application.installed_version.as_str())
        || installed.get("entry_point").and_then(Value::as_str) != Some("dist/cli.js")
        || hash(&installed, "sdk_fingerprint").is_none()
        || !hash_list(&installed, "sdk_files", 4096)
        || !hash_list(&installed, "patches", 128)
    {
        evidence.reason = "Installed manifest and current comparison do not agree".into();
        return evidence;
    }
    let patches = text(latest, "patches_status", 32).unwrap_or("unknown");
    evidence.patches = Some(patches.into());
    let status = text(latest, "status", 64).unwrap_or("unknown");
    let same_code = evidence.running_sdk == evidence.latest_sdk;
    let same_extensions = evidence.running_extensions == evidence.latest_extensions;
    let reason = text(latest, "reason", 1024).unwrap_or("No reason recorded");
    if patches == "outdated" && status == "installation_update_required" {
        evidence.status = BuildStatus::InstallationUpdateRequired;
    } else if patches == "verified"
        && installed.get("patches_verified").and_then(Value::as_bool) == Some(true)
        && hash(&installed, "sdk_fingerprint") == evidence.latest_sdk
    {
        if status == "current" && same_code && same_extensions {
            evidence.status = BuildStatus::Current;
        } else if status == "restart_required" && (!same_code || !same_extensions) {
            evidence.status = BuildStatus::RestartRequired;
        }
    }
    evidence.reason = if evidence.status == BuildStatus::Unknown {
        "Receipt does not establish a complete, consistent current build comparison".into()
    } else {
        reason.into()
    };
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    struct Fixture {
        root: PathBuf,
        application: NodeApplication,
        record: Value,
        manifest: Value,
    }

    impl Fixture {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root =
                std::env::temp_dir().join(format!("al-pi-receipt-{}-{nonce}", std::process::id()));
            for directory in [
                root.clone(),
                root.join("pi"),
                root.join("pi/processes"),
                root.join("installed"),
            ] {
                std::fs::create_dir(&directory).unwrap();
                std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            let application = NodeApplication {
                package: PACKAGE.into(),
                installed_version: "0.84.2".into(),
                entry_point: root.join("installed/dist/cli.js"),
                manifest: root.join("installed/package.json"),
                identification: "Synthetic fixture, never execute JavaScript",
            };
            let manifest = serde_json::json!({
                "schema": "coherence.pi.installed-build.v1", "package": PACKAGE, "version": "0.84.2",
                "entry_point": "dist/cli.js", "sdk_fingerprint": "a".repeat(64), "patches_verified": true,
                "sdk_files": [{"path": "pi-coding-agent/dist/cli.js", "sha256": "b".repeat(64)}],
                "patches": [{"path": "patches/usage.py", "sha256": "c".repeat(64)}],
            });
            let record = serde_json::json!({
                "schema": "coherence.pi.process-build.v1", "package": PACKAGE, "version": "0.84.2",
                "pid": 42, "uid": unsafe {libc::geteuid()}, "start_ticks": "99",
                "entry_point": application.entry_point, "package_manifest": application.manifest,
                "captured_at_unix_ms": 1_000_000,
                "running": {"sdk_fingerprint": "a".repeat(64), "extensions_fingerprint": "d".repeat(64), "extensions": [root.join("extension.mjs")]},
                "latest": {"entry_point": application.entry_point, "manifest_path": root.join("installed/coherence-build.json"),
                    "sdk_fingerprint": "a".repeat(64), "extensions_fingerprint": "d".repeat(64),
                    "checked_at_unix_ms": 1_000_000, "status": "current", "patches_status": "verified", "reason": "SDK and loaded extensions match the installation"},
                "scope": "JavaScript in the Pi SDK and the selected extension import graph; no chat data",
            });
            Self {
                root,
                application,
                record,
                manifest,
            }
        }

        fn record_path(&self) -> PathBuf {
            self.root.join("pi/processes/42-99.json")
        }

        fn write(&self) {
            std::fs::write(self.record_path(), self.record.to_string()).unwrap();
            std::fs::set_permissions(self.record_path(), std::fs::Permissions::from_mode(0o600))
                .unwrap();
            std::fs::write(
                self.root.join("installed/coherence-build.json"),
                self.manifest.to_string(),
            )
            .unwrap();
        }

        fn inspect(&self, now: u64) -> PiBuildEvidence {
            inspect(
                &self.application,
                42,
                99,
                unsafe { libc::geteuid() },
                &self.root,
                now,
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn matching_fresh_receipt_establishes_loaded_pi_without_executing_it() {
        let fixture = Fixture::new();
        fixture.write();
        let evidence = fixture.inspect(1_001_000);
        assert_eq!(evidence.status, BuildStatus::Current, "{}", evidence.reason);
        assert_eq!(evidence.running_version.as_deref(), Some("0.84.2"));
        assert_eq!(evidence.extension_count, Some(1));
        assert_eq!(evidence.check_age_seconds, Some(1));
        assert_eq!(evidence.running_sdk, evidence.latest_sdk);
    }

    #[test]
    fn changed_sdk_or_extension_requires_restart_and_preserves_startup_identity() {
        for field in ["sdk_fingerprint", "extensions_fingerprint"] {
            let mut fixture = Fixture::new();
            let running = fixture.record["running"].clone();
            fixture.record["latest"][field] = Value::from("e".repeat(64));
            fixture.record["latest"]["status"] = Value::from("restart_required");
            if field == "sdk_fingerprint" {
                fixture.manifest[field] = Value::from("e".repeat(64));
            }
            fixture.write();
            assert_eq!(
                fixture.inspect(1_001_000).status,
                BuildStatus::RestartRequired
            );
            assert_eq!(fixture.record["running"], running);
        }
    }

    #[test]
    fn patch_source_change_is_distinct_from_an_old_running_build() {
        let mut fixture = Fixture::new();
        fixture.record["latest"]["status"] = Value::from("installation_update_required");
        fixture.record["latest"]["patches_status"] = Value::from("outdated");
        fixture.write();
        assert_eq!(
            fixture.inspect(1_001_000).status,
            BuildStatus::InstallationUpdateRequired
        );
        fixture.record["latest"]["status"] = Value::from("current");
        fixture.write();
        assert_eq!(fixture.inspect(1_001_000).status, BuildStatus::Unknown);
    }

    #[test]
    fn external_relative_patch_inventory_and_changed_sdk_produce_actionable_status() {
        let mut fixture = Fixture::new();
        fixture.manifest["patches"][0]["path"] =
            Value::from("../../../../tasks/vllm-coherence/scripts/patch-pi-footer");
        fixture.write();
        assert_eq!(fixture.inspect(1_001_000).status, BuildStatus::Current);
        // The installed JS changed without regeneration of its qualification manifest.
        fixture.record["latest"]["sdk_fingerprint"] = Value::from("f".repeat(64));
        fixture.record["latest"]["patches_status"] = Value::from("outdated");
        fixture.record["latest"]["status"] = Value::from("installation_update_required");
        fixture.write();
        assert_eq!(
            fixture.inspect(1_001_000).status,
            BuildStatus::InstallationUpdateRequired
        );
    }

    #[test]
    fn old_receipts_and_inconsistent_current_claims_never_report_current() {
        let mut fixture = Fixture::new();
        fixture.write();
        let stale = fixture.inspect(1_090_001);
        assert_eq!(stale.status, BuildStatus::Unknown);
        assert_eq!(stale.running_version.as_deref(), Some("0.84.2"));
        assert_eq!(fixture.inspect(900_000).status, BuildStatus::Unknown);
        fixture.record["running"]["extensions_fingerprint"] = Value::from("f".repeat(64));
        fixture.write();
        assert_eq!(fixture.inspect(1_001_000).status, BuildStatus::Unknown);
        fixture.record["running"]["extensions_fingerprint"] = Value::from("d".repeat(64));
        fixture.manifest["patches_verified"] = Value::Bool(false);
        fixture.write();
        assert_eq!(fixture.inspect(1_001_000).status, BuildStatus::Unknown);
    }

    #[test]
    fn pid_reuse_or_cross_user_receipts_are_rejected() {
        for (key, value) in [
            ("pid", Value::from(43)),
            ("start_ticks", Value::from("100")),
            (
                "uid",
                Value::from(u64::from(unsafe { libc::geteuid() }) + 1),
            ),
        ] {
            let mut fixture = Fixture::new();
            fixture.record[key] = value;
            fixture.write();
            assert_eq!(fixture.inspect(1_001_000).status, BuildStatus::Unknown);
            assert!(fixture.inspect(1_001_000).running_version.is_none());
        }
    }

    #[test]
    fn missing_malformed_oversized_and_symlink_receipts_are_unknown() {
        let fixture = Fixture::new();
        assert_eq!(fixture.inspect(1_001_000).status, BuildStatus::Unknown);
        fixture.write();
        for bytes in [b"not JSON".to_vec(), vec![b' '; MAX_BYTES as usize + 1]] {
            std::fs::write(fixture.record_path(), bytes).unwrap();
            assert_eq!(fixture.inspect(1_001_000).status, BuildStatus::Unknown);
        }
        fixture.write();
        let target = fixture.root.join("moved-receipt.json");
        std::fs::rename(fixture.record_path(), &target).unwrap();
        symlink(&target, fixture.record_path()).unwrap();
        assert_eq!(fixture.inspect(1_001_000).status, BuildStatus::Unknown);
    }

    #[test]
    fn non_private_receipts_and_directories_are_not_trusted() {
        for which in ["file", "directory"] {
            let fixture = Fixture::new();
            fixture.write();
            let path = if which == "file" {
                fixture.record_path()
            } else {
                fixture.root.join("pi/processes")
            };
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(fixture.inspect(1_001_000).status, BuildStatus::Unknown);
        }
    }

    #[test]
    fn missing_changed_or_unsafe_manifest_is_not_a_current_installation() {
        for defect in ["hash", "entry", "traversal", "missing", "symlink"] {
            let mut fixture = Fixture::new();
            match defect {
                "hash" => fixture.manifest["sdk_fingerprint"] = Value::from("f".repeat(64)),
                "entry" => fixture.manifest["entry_point"] = Value::from("../outside.js"),
                "traversal" => {
                    fixture.manifest["sdk_files"][0]["path"] = Value::from("../outside.js")
                }
                _ => (),
            }
            fixture.write();
            let manifest = fixture.root.join("installed/coherence-build.json");
            if defect == "missing" {
                std::fs::remove_file(&manifest).unwrap();
            }
            if defect == "symlink" {
                let target = fixture.root.join("elsewhere.json");
                std::fs::rename(&manifest, &target).unwrap();
                symlink(target, manifest).unwrap();
            }
            assert_eq!(
                fixture.inspect(1_001_000).status,
                BuildStatus::Unknown,
                "{defect}"
            );
        }
    }

    #[test]
    fn restoring_the_matching_installation_clears_restart_status() {
        let mut fixture = Fixture::new();
        fixture.record["running"]["sdk_fingerprint"] = Value::from("f".repeat(64));
        fixture.record["latest"]["status"] = Value::from("restart_required");
        fixture.write();
        assert_eq!(
            fixture.inspect(1_001_000).status,
            BuildStatus::RestartRequired
        );
        fixture.record["latest"]["sdk_fingerprint"] = Value::from("f".repeat(64));
        fixture.manifest["sdk_fingerprint"] = Value::from("f".repeat(64));
        fixture.record["latest"]["status"] = Value::from("current");
        fixture.write();
        assert_eq!(fixture.inspect(1_001_000).status, BuildStatus::Current);
    }

    #[test]
    #[ignore = "Requires an owned, freshly generated Pi producer fixture; set COHERENCE_PI_E2E_RECORD"]
    fn actual_pi_producer_receipt_is_understood_by_applicationlauncher() {
        let path = PathBuf::from(
            std::env::var_os("COHERENCE_PI_E2E_RECORD").expect("producer fixture path"),
        );
        let uid = unsafe { libc::geteuid() };
        let record = read_json(&path, uid, true).expect("safe producer fixture");
        let latest = record.get("latest").unwrap();
        let manifest = absolute(latest, "manifest_path").unwrap();
        let installed = read_json(&manifest, uid, false).expect("safe installed producer manifest");
        let application = NodeApplication {
            package: PACKAGE.into(),
            installed_version: text(&installed, "version", 128).unwrap().into(),
            entry_point: absolute(latest, "entry_point").unwrap(),
            manifest: manifest.parent().unwrap().join("package.json"),
            identification: "Actual Pi producer contract fixture",
        };
        let pid = record.get("pid").and_then(Value::as_i64).unwrap() as i32;
        let start_ticks = text(&record, "start_ticks", 32).unwrap().parse().unwrap();
        let runtime = path.parent().unwrap().parent().unwrap().parent().unwrap();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let evidence = inspect(&application, pid, start_ticks, uid, runtime, now_ms);
        let expected =
            std::env::var("COHERENCE_PI_E2E_EXPECT").unwrap_or_else(|_| "Current".into());
        assert_eq!(evidence.status.label(), expected, "{}", evidence.reason);
        assert_eq!(
            evidence.running_version.as_deref(),
            text(&record, "version", 128)
        );
    }
}
