use super::{tests::node, *};
use program_metadata::{build_status::artifact::ArtifactStamp, development::Metadata};
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn development_receipt_refresh_discards_changed_entries_without_rescanning_sources() {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Entry(PathBuf);
    impl Drop for Entry {
        fn drop(&mut self) {
            std::fs::remove_file(&self.0).unwrap();
        }
    }
    for source_only in [false, true] {
        for change in ["unchanged", "same-path edit", "entry route"] {
            let entry = Entry(std::env::temp_dir().join(format!(
                "launcher-pi-development-{}-{}.js",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            )));
            let mut file = std::fs::File::create_new(&entry.0).unwrap();
            std::io::Write::write_all(&mut file, b"owned source entry").unwrap();
            let stamp = ArtifactStamp::from_metadata(&file.metadata().unwrap());
            let path = entry.0.clone();
            let probes = Arc::new(AtomicUsize::new(0));
            let probe_count = probes.clone();
            let cache = ProcessVersionCache::start_with_receipts(
                egui::Context::default(),
                egui::ViewportId::ROOT,
                move |_| {
                    probe_count.fetch_add(1, Ordering::SeqCst);
                    ProcessVersion {
                        application: Some(super::super::NodeApplication {
                            package: super::super::pi::PACKAGE.into(),
                            installed_version: "1".into(),
                            entry_point: path.clone(),
                            manifest: path.with_extension("json"),
                            identification: "Owned fixture",
                        }),
                        application_development: Some(Metadata {
                            source_languages: vec!["TypeScript".into()],
                            build_system: vec!["tsc".into()],
                            source_only,
                            artifact_stamp: (!source_only).then_some(stamp),
                            ..Default::default()
                        }),
                        version: Some("Independent Node version".into()),
                        ..Default::default()
                    }
                },
                |_| panic!("development refresh must not query packages"),
                move |request| {
                    let mut application = request.application.clone();
                    match change {
                        "same-path edit" => {
                            std::fs::write(&application.entry_point, b"changed owned entry source")
                                .unwrap();
                        }
                        "entry route" => {
                            application.entry_point.set_extension("different.js");
                        }
                        _ => {}
                    }
                    (
                        application,
                        super::super::PiBuildEvidence {
                            reason: "Receipt refreshed".into(),
                            ..Default::default()
                        },
                    )
                },
            );
            let target = node(42);
            let deadline = Instant::now() + Duration::from_secs(5);
            while cache.request(&target).is_none() {
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            cache
                .shared
                .0
                .lock()
                .unwrap()
                .receipt_checked
                .insert(target.identity, Instant::now() - PI_RECEIPT_REFRESH);
            loop {
                let result = cache.request(&target).unwrap();
                if result.application_build.is_some() {
                    assert_eq!(
                        result.application_development.is_some(),
                        change == "unchanged" || (source_only && change == "same-path edit"),
                        "{change}"
                    );
                    assert_eq!(result.version.as_deref(), Some("Independent Node version"));
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            assert_eq!(probes.load(Ordering::SeqCst), 1, "{change}");
        }
    }
}
