//! Shared package comparison policy, transport, parsing and freshness adapter.
#[cfg(test)]
use super::valid_package_name;
pub(crate) use program_metadata::package::AvailablePackageVersion;
pub(crate) use program_metadata::package::PackageTarget;
#[cfg(test)]
pub(crate) use program_metadata::package::PackageVersionSource;
use std::path::Path;

pub(super) fn target(executable: &Path, installed: Option<&str>) -> Option<PackageTarget> {
    program_metadata::package::availability_target(executable, installed)
}

pub(super) fn query(target: &PackageTarget) -> AvailablePackageVersion {
    let mut result = program_metadata::package::available_package_version(
        &target.name,
        !target.inferred && program_metadata::installation::allows_aur_package(&target.name),
    );
    if target.inferred {
        result
            .text
            .push_str(" (upstream comparison; not owning package)");
    }
    result
}

#[cfg(test)]
pub(crate) fn fixture(source: PackageVersionSource, text: &str) -> AvailablePackageVersion {
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    AvailablePackageVersion {
        source,
        text: text.into(),
        synced_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|now| now.as_secs() as i64 - 2 * 86400),
        checked_at: Some(Instant::now() - Duration::from_secs(120)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_owner_targets_are_preserved_without_authorizing_inferred_aur_names() {
        assert_eq!(
            target(Path::new("/usr/bin/ssh"), Some("openssh 10.4p1-3")),
            Some(PackageTarget {
                name: "openssh".into(),
                inferred: false
            })
        );
        for name in ["", "-Syu", "name with spaces", "../escape"] {
            assert!(!valid_package_name(name));
        }
    }
    #[test]
    fn shared_package_versions_keep_freshness_and_source_separate() {
        let pacman = fixture(PackageVersionSource::Pacman, "core/bash 5.3");
        assert_eq!(pacman.field_label(), "latest version (in last Pacman sync)");
        assert!(pacman.display().contains("synced 2 days ago"));
        let aur = fixture(PackageVersionSource::Aur, "aur/tool 1.0");
        assert_eq!(aur.field_label(), "latest version (AUR)");
        assert!(aur.display().contains("checked 2 minutes ago"));
        let unknown: AvailablePackageVersion = "Unavailable".into();
        assert_eq!(unknown.display(), "Unavailable");
    }
}
