use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::{AvailablePackageVersion, NpmTarget, PackageTarget, ProgsMetadata, installation};

const MAX_COMMAND_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Default)]
pub(crate) struct ApplicationLaunch {
    pub(crate) executable: Option<PathBuf>,
    pub(crate) chain: Vec<PathBuf>,
    pub(crate) identification: String,
    pub(crate) desktop_entry: Option<crate::models::AppInfo>,
    pub(crate) desktop_exists: Option<bool>,
    pub(crate) published_target_verified: bool,
}

impl ApplicationLaunch {
    pub(super) fn declare_window_entry(&mut self, entry: &crate::models::AppInfo) {
        if self.desktop_entry.is_none() {
            self.desktop_entry = Some(entry.clone());
            self.desktop_exists = desktop_exists(&entry.desktop_file_path);
            if !self.identification.is_empty() {
                self.identification.push_str("; ");
            }
            self.identification.push_str("Desktop entry declared by the window; launcher target not verified against this process");
        }
    }
}

fn desktop_exists(path: &Path) -> Option<bool> {
    match std::fs::metadata(path) {
        Ok(metadata) => Some(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// Reuse worker discovery while independently verifying the exact current route.
pub(super) fn associate_installed_desktop_from_matches(
    launch: &mut ApplicationLaunch,
    image: &File,
    matches: &[(PathBuf, program_metadata::desktop::Entry)],
) {
    if launch.desktop_entry.is_some() || matches.len() != 1 {
        return;
    }
    let path = &matches[0].0;
    // Reuse discovery, not stale fields: bind the current Exec to this image.
    let Ok(Some(entry)) = program_metadata::desktop::launch_match_pinned(
        path,
        image,
        std::time::Instant::now() + std::time::Duration::from_millis(200),
    ) else {
        return;
    };
    launch.desktop_entry = app_info_from_current_desktop(path.clone(), entry);
    if launch.desktop_entry.is_some() {
        launch.desktop_exists = Some(true);
    }
}

/// Desktop metadata is an association with an exact executable, not evidence
/// that its launcher is the current published installation. Retained running
/// builds can have a valid desktop entry even after the installation advances.
pub(super) fn associate_running_desktop(
    launch: &mut ApplicationLaunch,
    running: &File,
    candidate: Option<&crate::models::AppInfo>,
) {
    if launch.desktop_entry.is_some() {
        return;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    let matched = candidate.and_then(|candidate| {
        program_metadata::desktop::launch_match_pinned(
            &candidate.desktop_file_path,
            running,
            deadline,
        )
        .ok()
        .flatten()
        .map(|entry| (candidate.desktop_file_path.clone(), entry))
    });
    let matched = matched.or_else(|| {
        let mut matches =
            program_metadata::desktop::launch_matches_pinned(running, deadline).ok()?;
        (matches.len() == 1).then(|| matches.remove(0))
    });
    let Some((path, entry)) = matched else {
        return;
    };
    launch.desktop_entry = app_info_from_current_desktop(path, entry);
    if launch.desktop_entry.is_some() {
        launch.desktop_exists = Some(true);
        if !launch.identification.is_empty() {
            launch.identification.push_str("; ");
        }
        launch.identification.push_str(
            "Current desktop-entry route matches the pinned running image; this does not identify the current published installation",
        );
    }
}

fn app_info_from_current_desktop(
    desktop_file_path: PathBuf,
    entry: program_metadata::desktop::Entry,
) -> Option<crate::models::AppInfo> {
    let icon_path = entry.icon.and_then(|icon| {
        let path = PathBuf::from(&icon);
        if path.is_absolute() {
            return path.is_file().then_some(path);
        }
        crate::windows::lookup_theme_icon_exact("hicolor", &icon)
    });
    Some(crate::models::AppInfo {
        name: entry.name?,
        exec: entry.exec?,
        icon_path,
        comment: entry.comment,
        desktop_file_path,
        is_settings_module: false,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ApplicationInfo {
    pub(crate) launcher: Option<PathBuf>,
    pub(crate) launcher_resolved: Option<PathBuf>,
    pub(crate) launcher_exists: bool,
    pub(crate) desktop_exists: Option<bool>,
    pub(crate) chain: Vec<PathBuf>,
    pub(crate) resolution: String,
    pub(crate) executable: Option<PathBuf>,
    pub(crate) native: Option<bool>,
    pub(crate) installation: Option<String>,
    pub(crate) metadata: ProgsMetadata,
    pub(crate) version: Option<String>,
    pub(crate) version_reason: Option<String>,
    pub(crate) version_issue: Option<program_metadata::executable::MetadataIssue>,
    pub(crate) installed_metadata: Option<program_metadata::interpreted::InstalledVersion>,
    pub(crate) package: Option<String>,
    pub(crate) package_target: Option<PackageTarget>,
    pub(crate) latest_package_version: Option<AvailablePackageVersion>,
    pub(crate) npm_target: Option<NpmTarget>,
    pub(crate) latest_npm_version: Option<String>,
    pub(crate) build_id: Option<String>,
    pub(crate) build_id_reason: Option<String>,
    pub(crate) uncompiled_changes: Option<String>,
    pub(crate) launcher_installation: Option<String>,
    pub(crate) launcher_metadata: ProgsMetadata,
}

impl ApplicationInfo {
    pub(crate) fn query_package_version(&self) -> Option<AvailablePackageVersion> {
        self.package_target.as_ref().map(super::available::query)
    }

    pub(crate) fn query_npm_version(&self) -> Option<String> {
        self.npm_target.as_ref().map(super::npm::query)
    }
}

#[derive(Debug)]
pub(crate) enum ApplicationInfoUpdate {
    Details(ApplicationInfo),
    PackageVersion(AvailablePackageVersion),
    NpmVersion(String),
}

use program_metadata::launch::resolve;
#[cfg(test)]
use program_metadata::launch::wrapper_target;
#[cfg(test)]
use program_metadata::launch::{MAX_LAUNCH_STEPS, MAX_WRAPPER_BYTES};

pub(super) fn process_launch(
    executable: &Path,
    installed: Option<&installation::InstalledExecutable>,
    entry: Option<&crate::models::AppInfo>,
) -> ApplicationLaunch {
    let Some(installed) = installed else {
        return ApplicationLaunch {
            identification: "Unknown (installed executable aliases identify different builds)"
                .into(),
            ..Default::default()
        };
    };
    let expected = std::fs::metadata(&installed.path).ok();
    if let Some(entry) = entry
        && entry.exec.len() <= MAX_COMMAND_BYTES
    {
        let resolved = program_metadata::launch::resolve_exec(&entry.exec);
        let matches = resolved.executable.as_ref().is_some_and(|(_, file)| {
            file.metadata().is_ok_and(|metadata| {
                expected.as_ref().is_some_and(|expected| {
                    metadata.dev() == expected.dev() && metadata.ino() == expected.ino()
                })
            })
        });
        let current_entry = matches
            .then(|| {
                program_metadata::desktop::read(
                    &entry.desktop_file_path,
                    program_metadata::desktop::current_locale().as_deref(),
                    std::time::Instant::now() + std::time::Duration::from_millis(200),
                )
            })
            .and_then(Result::ok)
            .filter(|current| {
                !current.hidden
                    && current.entry_type.as_deref() == Some("Application")
                    && current.exec.as_deref() == Some(entry.exec.as_str())
            });
        // A candidate copied from a window or basename match is not an
        // association until both the current desktop record and the entire
        // static launch chain corroborate this exact image. Check stamps after
        // reading the desktop record as well as resolving its command.
        if let Some(entry) = current_entry.and_then(|current| {
            app_info_from_current_desktop(entry.desktop_file_path.clone(), current)
        }) && resolved.unchanged()
            && expected
                .as_ref()
                .is_some_and(|metadata| installed.unchanged(metadata))
        {
            return ApplicationLaunch {
                executable: resolved.executable.map(|(path, _)| path),
                chain: resolved.chain,
                identification:
                    "Current desktop-entry launch chain; original process launch route not recorded"
                        .into(),
                desktop_entry: Some(entry),
                desktop_exists: Some(true),
                published_target_verified: true,
            };
        }
    }
    let alias = (installed.launcher() == installed.path)
        .then(|| installation::launch_alias(&installed.path))
        .flatten();
    let launcher = alias.as_deref().unwrap_or_else(|| installed.launcher());
    let resolved = resolve(launcher);
    let unchanged = resolved.unchanged()
        && expected.as_ref().is_some_and(|metadata| {
            installed.unchanged(metadata)
                && resolved.executable.as_ref().is_some_and(|(_, file)| {
                    file.metadata().is_ok_and(|actual| {
                        super::executable_identity(&actual) == super::executable_identity(metadata)
                    })
                })
        });
    let path = resolved.executable.as_ref().map(|(path, _)| path.clone());
    let identification = if !unchanged {
        "Unknown (current installation changed or could not be resolved)".into()
    } else if installed.recovered_from_process() {
        "Current installation inferred from process launch name and publishing directory; original launch route not recorded".into()
    } else if !installed.is_published() {
        "Exact retained on-disk image; current published installation and original launch route are not proven".into()
    } else if launcher != executable {
        "Current published installation symlink; original process launch route not recorded".into()
    } else {
        "On-disk executable; original launcher/symlink route not recorded".into()
    };
    ApplicationLaunch {
        executable: unchanged.then_some(path).flatten(),
        chain: resolved.chain,
        identification,
        published_target_verified: unchanged && installed.is_published(),
        ..Default::default()
    }
}

pub(crate) fn inspect_application(exec: &str, desktop_file: &Path) -> ApplicationInfo {
    let mut result = ApplicationInfo {
        desktop_exists: desktop_exists(desktop_file),
        ..Default::default()
    };
    if exec.len() > MAX_COMMAND_BYTES {
        result.resolution = "Unknown (desktop command exceeds inspection limit)".into();
        result.version_reason = Some(result.resolution.clone());
        return result;
    }
    let Some(launcher) = crate::search::executable_path_from_exec(exec) else {
        result.resolution = "Unknown (desktop command cannot be resolved)".into();
        result.version_reason = Some(result.resolution.clone());
        return result;
    };
    result.launcher_exists = launcher.is_file();
    let (label, metadata) = installation::inspect(&launcher);
    result.launcher_installation = Some(label);
    result.launcher_metadata = metadata;
    result.launcher = Some(launcher.clone());
    result.npm_target = super::npm::target(&launcher, None);
    let resolution = program_metadata::launch::resolve_exec(exec);
    result.chain = resolution.chain.clone();
    result.launcher_resolved = resolution.launcher_resolved.clone();
    result.resolution = resolution.reason.clone();
    let Some((executable, file)) = &resolution.executable else {
        result.version_reason = Some(resolution.reason.clone());
        return result;
    };
    let version =
        super::probe_executable_context(file, executable, super::ExecutableContext::OnDisk);
    let package_image = installation::installed_package_image(executable, file);
    let package = package_image
        .as_ref()
        .map(|proof| proof.package.clone())
        .or_else(|| super::installed_package(executable));
    let (installation, metadata) = installation::inspect_with_owner(executable, package.as_deref());
    result.package_target = super::available::target(executable, package.as_deref());
    result.npm_target =
        super::npm::target(executable, version.version.as_deref()).or(result.npm_target);
    result.uncompiled_changes = Some(
        if package_image
            .as_ref()
            .is_some_and(|proof| proof.unchanged(file))
        {
            "Not applicable (package-managed installation)".into()
        } else if installation::uv_runtime(executable).is_some() {
            "Not applicable (prebuilt uv-managed runtime)".into()
        } else {
            super::freshness::uncompiled_changes(executable, file)
        },
    );
    result.executable = Some(executable.clone());
    result.native = Some(super::is_elf(file));
    match super::installed_application_metadata(executable) {
        Ok(metadata) => {
            result.installed_metadata = metadata;
            result.version = result
                .installed_metadata
                .as_ref()
                .map(|version| version.version.clone())
                .or(version.version);
            result.version_reason = result.version.is_none().then_some(version.reason).flatten();
            result.version_issue = result
                .version
                .is_none()
                .then_some(version.reason_kind)
                .flatten();
        }
        Err(reason) => result.version_reason = Some(reason),
    }
    result.build_id = version.build_id;
    result.build_id_reason = version.build_id_reason;
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
            version_reason: Some("launch chain changed during inspection; reopen Show Info".into()),
            version_issue: Some(program_metadata::executable::MetadataIssue::Changed),
            launcher_installation: result.launcher_installation,
            launcher_metadata: result.launcher_metadata,
            ..Default::default()
        };
    }
    if package_image
        .as_ref()
        .is_some_and(|proof| !proof.unchanged(file))
    {
        result.uncompiled_changes =
            Some("Unknown (package ownership or image changed during inspection)".into());
        result.package = None;
        result.package_target = None;
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

        fn elf(&self, name: &str) -> PathBuf {
            let mut bytes = vec![0u8; 64];
            bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
            bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
            bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
            bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
            bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
            bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
            bytes.extend_from_slice(name.as_bytes());
            self.file(name, bytes)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn shared_compact_input_counts_reach_installed_application_metadata() {
        use std::time::{Duration, UNIX_EPOCH};

        let fixture = Fixture::new();
        let executable = fixture.elf("app");
        let source = fixture.file("source.c", "fixture source; never compiled");
        let header = fixture.file("header.h", "fixture header; never compiled");
        let manifest = fixture.file(
            "build.ninja",
            "rule link\n  command = touch MUST_NOT_RUN\nbuild app: link source.c header.h\n",
        );
        let mtime = |path: &Path, seconds| {
            File::open(path)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
                .unwrap();
        };
        mtime(&executable, 200);
        mtime(&source, 300);
        mtime(&header, 100);
        mtime(&manifest, 100);
        let inspect = || {
            inspect_application(
                executable.to_str().unwrap(),
                &fixture.0.join("fixture.desktop"),
            )
            .uncompiled_changes
            .expect("installed application must retain the shared build observation")
        };
        assert_eq!(
            inspect(),
            "1 newer · 0 missing · added: unknown · 3 inputs checked"
        );
        mtime(&source, 100);
        assert_eq!(
            inspect(),
            "0 newer · 0 missing · added: unknown · 3 inputs checked · full build status unknown"
        );
        std::fs::remove_file(header).unwrap();
        assert_eq!(
            inspect(),
            "0 newer · 1 missing · added: unknown · 3 inputs checked"
        );
        assert!(!fixture.0.join("MUST_NOT_RUN").exists());
    }

    #[test]
    fn window_declared_entry_does_not_replace_the_verified_executable_or_chain() {
        let entry = crate::models::AppInfo {
            name: "GUI application".into(),
            exec: "/unsupported/launcher".into(),
            comment: None,
            icon_path: None,
            desktop_file_path: "/missing/tool.desktop".into(),
            is_settings_module: false,
        };
        let mut launch = ApplicationLaunch {
            executable: Some("/actual/program".into()),
            chain: vec!["/actual/program".into()],
            ..Default::default()
        };
        launch.declare_window_entry(&entry);
        assert_eq!(
            launch.executable.as_deref(),
            Some(Path::new("/actual/program"))
        );
        assert_eq!(launch.chain, vec![PathBuf::from("/actual/program")]);
        assert_eq!(launch.desktop_entry.as_ref(), Some(&entry));
        assert!(launch.identification.contains("declared by the window"));
        assert!(launch.identification.contains("not verified"));
    }

    #[test]
    fn current_desktop_candidates_refresh_fields_after_exact_route_verification() {
        let fixture = Fixture::new();
        let executable = fixture.elf("program");
        let icon = fixture.file("new-icon.svg", "<svg/>\n");
        let desktop = fixture.file(
            "tool.desktop",
            format!(
                "[Desktop Entry]\nType=Application\nName=Fresh Name\nComment=Fresh Comment\nIcon={}\nExec={}\n",
                icon.display(), executable.display()
            ),
        );
        let candidate = crate::models::AppInfo {
            name: "Stale Name".into(),
            exec: executable.display().to_string(),
            icon_path: Some(fixture.0.join("stale-icon.png")),
            comment: Some("Stale Comment".into()),
            desktop_file_path: desktop.clone(),
            is_settings_module: false,
        };
        let installed = installation::installed_executable(&executable).unwrap();
        let launch = process_launch(&executable, Some(&installed), Some(&candidate));
        let fresh = launch.desktop_entry.as_ref().expect("exact current route");
        assert_eq!(fresh.name, "Fresh Name");
        assert_eq!(fresh.comment.as_deref(), Some("Fresh Comment"));
        assert_eq!(fresh.icon_path, Some(icon));
        assert_eq!(fresh.desktop_file_path, desktop);
        assert_eq!(fresh.exec, candidate.exec);
        assert_eq!(launch.executable.as_ref(), Some(&executable));
        assert!(launch.published_target_verified);
    }

    #[test]
    fn pinned_running_desktop_preserves_latest_installation_and_refreshes_fields() {
        let fixture = Fixture::new();
        let retained = fixture.elf("retained");
        let latest = fixture.elf("latest");
        let icon = fixture.file("current-icon.svg", "<svg/>\n");
        let desktop = fixture.file(
            "different-desktop-name.desktop",
            format!(
                "[Desktop Entry]\nType=Application\nName=Current Name\nComment=Current Comment\nIcon={}\nExec={} %u\n",
                icon.display(), retained.display()
            ),
        );
        let candidate = crate::models::AppInfo {
            name: "Stale Name".into(),
            exec: format!("{} %u", retained.display()),
            icon_path: Some(fixture.0.join("stale-icon.png")),
            comment: Some("Stale Comment".into()),
            desktop_file_path: desktop.clone(),
            is_settings_module: false,
        };
        let installed = installation::installed_executable(&latest).unwrap();
        let previous = process_launch(&retained, Some(&installed), Some(&candidate));
        assert!(
            previous.desktop_entry.is_none(),
            "Latest-image validation alone cannot associate this retained running image"
        );
        let mut launch = ApplicationLaunch {
            executable: Some(latest.clone()),
            chain: vec![fixture.0.join("published"), latest.clone()],
            identification: "Current installation".into(),
            published_target_verified: true,
            ..Default::default()
        };
        let original_chain = launch.chain.clone();
        associate_running_desktop(
            &mut launch,
            &File::open(&retained).unwrap(),
            Some(&candidate),
        );
        let associated = launch
            .desktop_entry
            .as_ref()
            .expect("retained running image has an exact current desktop route");
        assert_eq!(associated.name, "Current Name");
        assert_eq!(associated.comment.as_deref(), Some("Current Comment"));
        assert_eq!(associated.icon_path.as_ref(), Some(&icon));
        assert_eq!(associated.desktop_file_path, desktop);
        assert_eq!(launch.desktop_exists, Some(true));
        assert_eq!(launch.executable.as_ref(), Some(&latest));
        assert_eq!(launch.chain, original_chain);
        assert!(launch.published_target_verified);
        assert!(
            launch
                .identification
                .contains("does not identify the current published installation")
        );
    }

    #[test]
    fn pinned_running_desktop_cannot_promote_a_retained_image_to_published() {
        let fixture = Fixture::new();
        let retained = fixture.elf("retained");
        let desktop = fixture.file(
            "program.desktop",
            format!(
                "[Desktop Entry]\nType=Application\nName=Retained\nExec={}\n",
                retained.display()
            ),
        );
        let candidate = crate::models::AppInfo {
            name: "Retained".into(),
            exec: retained.display().to_string(),
            icon_path: None,
            comment: None,
            desktop_file_path: desktop,
            is_settings_module: false,
        };
        let mut launch = ApplicationLaunch {
            executable: Some(retained.clone()),
            chain: vec![retained.clone()],
            published_target_verified: false,
            ..Default::default()
        };
        associate_running_desktop(
            &mut launch,
            &File::open(&retained).unwrap(),
            Some(&candidate),
        );
        assert!(launch.desktop_entry.is_some());
        assert!(!launch.published_target_verified);
        assert_eq!(launch.executable.as_ref(), Some(&retained));
        assert_eq!(launch.chain, vec![retained]);
    }

    #[test]
    #[ignore = "opt-in read-only retained/current application artifacts; never starts the program"]
    fn actual_retained_desktop_keeps_current_installation_identity_separate() {
        let retained = PathBuf::from(
            std::env::var("APPLICATIONLAUNCHER_TEST_RETAINED_DESKTOP_EXECUTABLE").unwrap(),
        );
        let latest = PathBuf::from(
            std::env::var("APPLICATIONLAUNCHER_TEST_RETAINED_DESKTOP_LATEST").unwrap(),
        );
        let desktop =
            PathBuf::from(std::env::var("APPLICATIONLAUNCHER_TEST_RETAINED_DESKTOP_FILE").unwrap());
        let pinned = File::open(&retained).unwrap();
        let original = pinned.metadata().unwrap();
        let latest_identity = std::fs::metadata(&latest).unwrap();
        assert_ne!(
            (original.dev(), original.ino()),
            (latest_identity.dev(), latest_identity.ino()),
            "This control requires separate retained and current images"
        );
        let expected = program_metadata::desktop::read(
            &desktop,
            program_metadata::desktop::current_locale().as_deref(),
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        )
        .unwrap();
        let candidate = crate::models::AppInfo {
            name: "Ignored cached name".into(),
            exec: "Ignored cached command".into(),
            icon_path: None,
            comment: None,
            desktop_file_path: desktop.clone(),
            is_settings_module: false,
        };
        let mut launch = ApplicationLaunch {
            executable: Some(latest.clone()),
            chain: vec![latest.clone()],
            published_target_verified: true,
            ..Default::default()
        };
        associate_running_desktop(&mut launch, &pinned, Some(&candidate));
        let association = launch
            .desktop_entry
            .as_ref()
            .expect("exact retained desktop route");
        assert_eq!(Some(&association.name), expected.name.as_ref());
        assert_eq!(Some(&association.exec), expected.exec.as_ref());
        assert_eq!(association.comment, expected.comment);
        assert_eq!(association.desktop_file_path, desktop);
        assert_eq!(launch.executable.as_ref(), Some(&latest));
        assert_eq!(launch.chain, vec![latest]);
        assert!(launch.published_target_verified);
        assert_eq!(
            super::super::executable_identity(&original),
            super::super::executable_identity(&pinned.metadata().unwrap())
        );
        applicationlauncher::diagnostic_log!(
            "retained desktop={} name={} latest installation remains independent; no executable started",
            association.desktop_file_path.display(),
            association.name
        );
    }

    #[test]
    fn child_desktop_candidates_require_a_current_exact_desktop_record() {
        let fixture = Fixture::new();
        let executable = fixture.elf("chrome");
        let wrapper = fixture.file(
            "google-chrome-fast",
            format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", executable.display()),
        );
        let desktop = fixture.0.join("google-chrome-fast.desktop");
        let entry = crate::models::AppInfo {
            name: "Google Chrome".into(),
            exec: wrapper.display().to_string(),
            comment: Some("Browser desktop metadata".into()),
            icon_path: Some(fixture.0.join("chrome.png")),
            desktop_file_path: desktop.clone(),
            is_settings_module: false,
        };
        let installed = installation::installed_executable(&executable).unwrap();
        for contents in [
            None,
            Some("[Desktop Entry]\nType=Application\nExec=/usr/bin/true\n".into()),
            Some(format!(
                "[Desktop Entry]\nType=Application\nHidden=true\nExec={}\n",
                wrapper.display()
            )),
            Some(format!(
                "[Desktop Entry]\nType=Link\nExec={}\n",
                wrapper.display()
            )),
        ] {
            if let Some(contents) = contents {
                std::fs::write(&desktop, contents).unwrap();
            }
            let launch = process_launch(&executable, Some(&installed), Some(&entry));
            assert!(launch.desktop_entry.is_none(), "{launch:?}");
            assert!(!launch.published_target_verified, "{launch:?}");
        }
        std::fs::write(
            &desktop,
            format!(
                "[Desktop Entry]\nType=Application\nName=Chrome\nExec={}\n",
                wrapper.display()
            ),
        )
        .unwrap();
        let launch = process_launch(&executable, Some(&installed), Some(&entry));
        let fresh = launch.desktop_entry.as_ref().unwrap();
        assert_eq!(fresh.desktop_file_path, entry.desktop_file_path);
        assert_eq!(fresh.exec, entry.exec);
        assert_eq!(fresh.name, "Chrome");
        assert_eq!(fresh.comment, None);
        assert_eq!(fresh.icon_path, None);
        assert!(launch.published_target_verified);
        // Being a sibling in the same product/package, or even having the
        // same bytes, is not evidence that the desktop command targets it.
        let helper = fixture.0.join("chrome_crashpad_handler");
        std::fs::copy(&executable, &helper).unwrap();
        let helper_installed = installation::installed_executable(&helper).unwrap();
        let helper_launch = process_launch(&helper, Some(&helper_installed), Some(&entry));
        assert!(helper_launch.desktop_entry.is_none(), "{helper_launch:?}");
        assert!(!helper_launch.published_target_verified);
    }

    #[test]
    fn process_launch_keeps_current_entry_metadata_separate_from_running_identity() {
        let fixture = Fixture::new();
        let executable = fixture.elf("program");
        let link = fixture.0.join("published");
        symlink(&executable, &link).unwrap();
        let desktop = fixture.file(
            "tool.desktop",
            format!(
                "[Desktop Entry]\nType=Application\nName=Tool\nComment=A tool\nExec={}\n",
                link.display()
            ),
        );
        let entry = crate::models::AppInfo {
            name: "Tool".into(),
            exec: link.display().to_string(),
            comment: Some("A tool".into()),
            icon_path: None,
            desktop_file_path: desktop.clone(),
            is_settings_module: false,
        };
        let installed = installation::installed_executable(&executable).unwrap();
        let result = process_launch(&executable, Some(&installed), Some(&entry));
        assert_eq!(result.executable.as_ref(), Some(&executable));
        assert_eq!(result.chain, vec![link.clone(), executable.clone()]);
        assert_eq!(result.desktop_entry.as_ref(), Some(&entry));
        assert_eq!(result.desktop_exists, Some(true));
        assert!(
            result
                .identification
                .contains("original process launch route not recorded")
        );
        std::fs::remove_file(desktop).unwrap();
        let missing = process_launch(&executable, Some(&installed), Some(&entry));
        assert!(missing.desktop_entry.is_none());
        assert!(!missing.published_target_verified);
        let other = fixture.elf("unrelated");
        std::fs::remove_file(&link).unwrap();
        symlink(&other, &link).unwrap();
        let rejected = process_launch(&executable, Some(&installed), Some(&entry));
        assert!(rejected.desktop_entry.is_none());
        assert_eq!(rejected.executable.as_ref(), Some(&executable));
        assert!(!rejected.chain.contains(&other));
    }

    #[test]
    fn unresolved_process_launch_never_claims_a_desktop_entry_or_installed_binary() {
        let fixture = Fixture::new();
        let missing = fixture.0.join("missing");
        let installed = installation::installed_executable(&missing).unwrap();
        let result = process_launch(&missing, Some(&installed), None);
        assert!(result.executable.is_none() && result.desktop_entry.is_none());
        assert_eq!(result.chain, vec![missing]);
        assert!(result.identification.starts_with("Unknown"));
    }

    #[test]
    fn browser_wrappers_verify_the_current_desktop_target_for_header_and_elf_colours() {
        use super::super::{ProcessLaunchContext, ProcessVersionTarget, probe_process_checked};
        let fixture = Fixture::new();
        let executable = std::env::current_exe().unwrap();
        assert!(
            !installation::installed_executable(&executable)
                .unwrap()
                .is_published()
        );
        for browser in ["firefox", "chrome"] {
            let wrapper = fixture.file(
                browser,
                format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", executable.display()),
            );
            let desktop = fixture.file(
                &format!("{browser}.desktop"),
                format!(
                    "[Desktop Entry]\nType=Application\nName={browser}\nExec={}\n",
                    wrapper.display()
                ),
            );
            let mut target = ProcessVersionTarget {
                pid: std::process::id() as i32,
                executable: executable.clone(),
                launch_context: ProcessLaunchContext::default(),
                program: Some(browser.into()),
                application_entry: None,
                window_declared_entry: false,
            };
            assert_eq!(
                probe_process_checked(&target, || true, false).process_build_is_current(),
                None
            );
            target.application_entry = Some(crate::models::AppInfo {
                name: browser.into(),
                exec: wrapper.display().to_string(),
                comment: None,
                icon_path: None,
                desktop_file_path: desktop.clone(),
                is_settings_module: false,
            });
            target.window_declared_entry = true;
            for details in [false, true] {
                let result = probe_process_checked(&target, || true, details);
                assert_eq!(
                    result.process_build_is_current(),
                    Some(true),
                    "{browser}: {result:?}"
                );
                if details {
                    assert!(result.build_id.is_some());
                    assert_eq!(result.build_id, result.latest_build_id);
                    assert!(result.launch.unwrap().published_target_verified);
                }
            }
            assert_eq!(
                probe_process_checked(&target, || false, false).process_build_is_current(),
                None
            );
            std::fs::write(
                &desktop,
                "[Desktop Entry]\nType=Application\nName=Changed\nExec=/usr/bin/true\n",
            )
            .unwrap();
            assert_eq!(
                probe_process_checked(&target, || true, false).process_build_is_current(),
                None,
                "a stale desktop command must not publish the retained image"
            );
            std::fs::remove_file(&desktop).unwrap();
            assert_eq!(
                probe_process_checked(&target, || true, false).process_build_is_current(),
                None,
                "a missing desktop entry must not publish the retained image"
            );
            std::fs::write(
                &desktop,
                format!(
                    "[Desktop Entry]\nType=Application\nName={browser}\nExec={}\n",
                    wrapper.display()
                ),
            )
            .unwrap();
            std::fs::write(&wrapper, "#!/bin/sh\nexec /usr/bin/true \"$@\"\n").unwrap();
            assert_eq!(
                probe_process_checked(&target, || true, false).process_build_is_current(),
                None,
                "an unrelated wrapper target must not validate this process"
            );
        }
    }

    #[test]
    fn desktop_file_existence_is_captured_even_when_the_launch_command_is_unavailable() {
        let fixture = Fixture::new();
        let desktop = fixture.file("tool.desktop", "[Desktop Entry]\nName=Tool\n");
        assert_eq!(inspect_application("", &desktop).desktop_exists, Some(true));
        std::fs::remove_file(&desktop).unwrap();
        assert_eq!(
            inspect_application("", &desktop).desktop_exists,
            Some(false)
        );
        let uninspectable = fixture.0.join("x".repeat(300));
        assert_eq!(inspect_application("", &uninspectable).desktop_exists, None);
        assert_eq!(ApplicationInfo::default().desktop_exists, None);
    }

    #[test]
    fn interpreted_launches_without_an_exact_payload_never_inherit_runtime_versions() {
        let fixture = Fixture::new();
        for (runtime, args) in [
            ("python3", "-m example"),
            ("node", "--eval 1"),
            ("java", "ExampleClass"),
        ] {
            let executable = fixture.elf(runtime);
            let result = inspect_application(
                &format!("{} {args}", executable.display()),
                &fixture.0.join("app.desktop"),
            );
            assert!(result.executable.is_none(), "{runtime}: {result:?}");
            assert!(
                result.version.is_none() && result.package.is_none(),
                "{runtime}: {result:?}"
            );
            assert!(result.version_reason.is_some(), "{runtime}: {result:?}");
        }
    }

    #[test]
    fn python_application_tiles_read_exact_installed_wheel_version_without_executing_it() {
        let fixture = Fixture::new();
        let runtime = fixture.elf("python3");
        let site = fixture.0.join("lib/python3.14/site-packages");
        let package = site.join("sample");
        let dist = site.join("sample-2.4.dist-info");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::create_dir_all(&dist).unwrap();
        let script = package.join("main.py");
        let sentinel = fixture.0.join("MUST_NOT_RUN");
        std::fs::write(&script, format!("open({:?}, 'w').write('bad')", sentinel)).unwrap();
        std::fs::write(dist.join("METADATA"), "Name: sample\nVersion: 2.4\n").unwrap();
        std::fs::write(dist.join("RECORD"), "sample/main.py,,\n").unwrap();
        let result = inspect_application(
            &format!("{} {}", runtime.display(), script.display()),
            &fixture.0.join("sample.desktop"),
        );
        assert_eq!(result.executable.as_deref(), Some(script.as_path()));
        assert_eq!(result.version.as_deref(), Some("2.4"));
        assert_eq!(result.native, Some(false));
        assert!(result.build_id.is_none() && result.version_reason.is_none());
        assert!(!sentinel.exists());
    }

    #[test]
    fn npm_application_tiles_use_manifest_identity_even_for_large_entry_points() {
        let fixture = Fixture::new();
        let entry = fixture.file("cli.js", vec![b'x'; MAX_WRAPPER_BYTES as usize + 1]);
        fixture.file(
            "package.json",
            br#"{"name":"@example/tool","version":"1.0.0","bin":{"tool":"cli.js"}}"#,
        );
        let launcher = fixture.0.join("tool");
        symlink(entry, &launcher).unwrap();
        let info = inspect_application(launcher.to_str().unwrap(), &fixture.0.join("tool.desktop"));
        assert_eq!(info.npm_target.as_ref().unwrap().name, "@example/tool");
        assert!(info.latest_npm_version.is_none());
        assert!(info.package_target.is_none());
        assert_eq!(info.version.as_deref(), Some("1.0.0"));
        assert!(info.version_reason.is_none());
    }

    #[test]
    fn chrome_launch_chain_follows_symlinks_env_shebangs_and_the_verified_here_template() {
        let root = Fixture::new();
        let binary = root.elf("chrome");
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
        let binary = root.elf("my browser");
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
        assert!(resolve(&dangling).executable.is_none());
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
        let first = root.elf("binary-first");
        let second = root.elf("binary-second");
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
        applicationlauncher::diagnostic_log!(
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
            applicationlauncher::diagnostic_log!(
                "available_package={:?}",
                info.query_package_version()
            );
        }
    }
}
