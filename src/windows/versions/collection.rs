//! Collection policy for synchronous callers and progressive process-info lookups.

#[cfg(test)]
mod benchmarks;

pub(super) struct DiskDetails {
    pub package: Option<String>,
    pub installation: String,
    pub progs_metadata: super::ProgsMetadata,
    pub package_target: Option<super::PackageTarget>,
    pub npm_target: Option<super::NpmTarget>,
    pub launch: super::ApplicationLaunch,
    pub launch_target_verified: bool,
}

pub(super) fn with_source_check<T>(
    deferred: bool,
    source: impl FnOnce() -> String + Send,
    details: impl FnOnce() -> T,
) -> (String, T) {
    if deferred {
        // Do not start expensive work here: the lookup's bounded source worker
        // receives the pinned installation after final metadata validation.
        return (super::freshness::PENDING.into(), details());
    }
    std::thread::scope(|scope| {
        let source = std::thread::Builder::new()
            .name("process-info-source-check".into())
            .spawn_scoped(scope, source);
        let details = details();
        let source = match source {
            Ok(worker) => worker
                .join()
                .unwrap_or_else(|_| "Unknown (source check worker failed)".into()),
            Err(_) => "Unknown (source check worker unavailable)".into(),
        };
        (source, details)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn source_check_overlaps_details_and_is_joined_before_publishing() {
        let (started, waiting) = mpsc::channel();
        let (finish, release) = mpsc::channel();
        let (source, details) = with_source_check(
            false,
            move || {
                started.send(()).unwrap();
                release.recv_timeout(Duration::from_secs(5)).unwrap();
                "source inputs verified".to_owned()
            },
            || {
                waiting.recv_timeout(Duration::from_secs(5)).unwrap();
                finish.send(()).unwrap();
                "independent metadata"
            },
        );
        assert_eq!(source, "source inputs verified");
        assert_eq!(details, "independent metadata");
    }

    #[test]
    fn failed_source_worker_preserves_other_metadata_without_claiming_clean_sources() {
        let (source, details) = with_source_check(false, || panic!("source fixture failed"), || 42);
        assert_eq!(source, "Unknown (source check worker failed)");
        assert_eq!(details, 42);
    }

    #[test]
    fn deferred_collection_never_starts_the_expensive_source_check() {
        let (source, details) = with_source_check(true, || panic!("must be deferred"), || 42);
        assert_eq!(source, super::super::freshness::PENDING);
        assert_eq!(details, 42);
    }
}
