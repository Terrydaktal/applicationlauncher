use super::tests::node;
use super::*;
use std::sync::mpsc;

#[test]
fn visible_metadata_overtakes_collapsed_headers_without_reordering_visible_requests() {
    let (started, requests) = mpsc::channel();
    let (finish, release) = mpsc::channel();
    let cache = ProcessVersionCache::start(
        egui::Context::default(),
        egui::ViewportId::ROOT,
        move |request| {
            started.send(request.identity.pid).unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
            ProcessVersion::default()
        },
    );
    cache.request_for_header(&node(1));
    assert_eq!(requests.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
    cache.request_for_header(&node(2));
    cache.request(&node(3));
    cache.request_for_header(&node(4));
    cache.request(&node(5));
    for expected in [3, 5, 2, 4] {
        finish.send(()).unwrap();
        assert_eq!(
            requests.recv_timeout(Duration::from_secs(5)).unwrap(),
            expected
        );
    }
    drop(cache);
    finish.send(()).unwrap();
}

#[test]
fn expanding_a_queued_header_promotes_it_ahead_of_collapsed_headers() {
    let (started, requests) = mpsc::channel();
    let (finish, release) = mpsc::channel();
    let cache = ProcessVersionCache::start(
        egui::Context::default(),
        egui::ViewportId::ROOT,
        move |request| {
            started
                .send((request.identity.pid, request.include_packages))
                .unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
            ProcessVersion::default()
        },
    );
    cache.request_for_header(&node(1));
    requests.recv_timeout(Duration::from_secs(5)).unwrap();
    cache.request_for_header(&node(2));
    cache.request_for_header(&node(3));
    cache.request(&node(3));
    cache.request(&node(3));
    finish.send(()).unwrap();
    assert_eq!(
        requests.recv_timeout(Duration::from_secs(5)).unwrap(),
        (3, true)
    );
    drop(cache);
    finish.send(()).unwrap();
}
