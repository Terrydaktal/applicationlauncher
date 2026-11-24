use super::*;
use program_metadata::executable::VersionSource;

fn result(source: VersionSource) -> ExecutableVersion {
    ExecutableVersion {
        version: Some("trusted fixture 1.0".into()),
        source: Some(source),
        build_id: Some("same-exact-image".into()),
        ..Default::default()
    }
}

#[test]
fn identical_main_image_reuses_one_main_image_observation_within_the_lookup() {
    for source in [
        VersionSource::ElfProductMetadata,
        VersionSource::ElfProductBanner,
        VersionSource::TrustedLocalProbe,
        VersionSource::TrustedSystemProbe,
        VersionSource::VerifiedInstallationProbe,
    ] {
        let running = result(source);
        let disk = disk_executable_version(&running, true, || panic!("duplicate version query"));
        assert_eq!(disk.version, running.version);
        assert_eq!(disk.source, running.source);
        assert_eq!(disk.build_id, running.build_id);
    }
}

#[test]
fn changed_identity_or_path_keeps_the_independent_disk_probe() {
    let running = result(VersionSource::TrustedLocalProbe);
    let disk = disk_executable_version(&running, false, || ExecutableVersion {
        version: Some("different on-disk image 2.0".into()),
        ..Default::default()
    });
    assert_eq!(disk.version.as_deref(), Some("different on-disk image 2.0"));
}

#[test]
fn process_apis_components_and_incomplete_results_never_become_disk_evidence() {
    let mut cases = vec![
        result(VersionSource::RunningApplicationApi),
        result(VersionSource::ElfConstantFunction),
        result(VersionSource::ElfVersionObject),
    ];
    let mut required = result(VersionSource::TrustedLocalProbe);
    required.components_required = true;
    cases.push(required);
    let mut component = result(VersionSource::TrustedLocalProbe);
    component
        .components
        .push(program_metadata::executable::ExecutableComponent {
            path: "/fixture/private-library.so".into(),
            identity: program_metadata::build_status::artifact::ArtifactStamp::from_metadata(
                &std::fs::metadata("/proc/self/exe").unwrap(),
            ),
            build_id: Some("separate-component".into()),
        });
    cases.push(component);
    let mut failed = result(VersionSource::TrustedLocalProbe);
    failed.reason_kind = Some(program_metadata::executable::MetadataIssue::Changed);
    cases.push(failed);
    let mut missing = result(VersionSource::TrustedLocalProbe);
    missing.version = None;
    cases.push(missing);
    for running in cases {
        let disk = disk_executable_version(&running, true, || ExecutableVersion {
            version: Some("independent disk result".into()),
            ..Default::default()
        });
        assert_eq!(disk.version.as_deref(), Some("independent disk result"));
    }
}
