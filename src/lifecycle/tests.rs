use super::*;

#[test]
fn lifecycle_protocol_requires_bounded_operation_ids_and_explicit_native_scopes() {
    for id in ["job-123", "application-update-123-1"] {
        assert!(valid_id(id));
    }
    for id in ["", "../escape", "x/y", "x.service;anything"] {
        assert!(!valid_id(id));
    }
    assert!(valid_uuid("12345678-1234-5678-9abc-123456789abc"));
    assert!(!valid_uuid("12345678/1234-5678-9abc-123456789abc"));
    let value: Value = serde_json::from_str(&Service.get_capabilities()).unwrap();
    assert_eq!(value["protocol"], 1);
    assert_eq!(value["terminal_replacement"], 1);
    assert_eq!(value["application_adapters"], 1);
    assert_eq!(value["external_terminal_replacement"], 1);
    for id in ["", "../escape", "not-a-uuid"] {
        assert!(execute_external_window(id, &json!({})).is_err());
    }
}

#[test]
fn private_bus_lifecycle_rejects_malformed_requests_and_callers_without_helper_ownership() {
    const PRIVATE: &str = "APPLICATIONLAUNCHER_LIFECYCLE_TEST_PRIVATE";
    if std::env::var(PRIVATE).as_deref() != Ok("1") {
        let output = Command::new("dbus-run-session").arg("--")
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "lifecycle::tests::private_bus_lifecycle_rejects_malformed_requests_and_callers_without_helper_ownership", "--nocapture"])
            .env(PRIVATE, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let id = "12345678-1234-5678-9abc-123456789abc";
    let helper = format!("org.xfce.Terminal5.Update.i{}", id.replace('-', "_"));
    let fixture = zbus::blocking::connection::Builder::session()
        .unwrap()
        .name(crate::tracker::SERVICE_NAME)
        .unwrap()
        .name(helper.as_str())
        .unwrap()
        .serve_at(PATH, Service)
        .unwrap()
        .build()
        .unwrap();
    let caller = zbus::blocking::Connection::session().unwrap();
    assert_ne!(fixture.unique_name(), caller.unique_name());
    let proxy =
        zbus::blocking::Proxy::new(&caller, crate::tracker::SERVICE_NAME, PATH, INTERFACE).unwrap();
    let capabilities: String = proxy.call("GetCapabilities", &()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&capabilities).unwrap()["protocol"],
        1
    );
    let valid_but_not_owned =
        json!({"protocol":1,"job_id":id,"window_uuid":id,"token":"a".repeat(64),"service":helper})
            .to_string();
    for request in [
        "{}".to_owned(),
        "invalid-json".to_owned(),
        "x".repeat(65537),
        valid_but_not_owned,
    ] {
        assert!(
            proxy
                .call::<_, _, String>("BeginWindowUpdate", &(request,))
                .is_err()
        );
    }
    assert!(
        proxy
            .call::<_, _, String>("GetWindowUpdateReport", &("../escape",))
            .is_err()
    );
}

#[test]
fn durable_records_are_private_bounded_and_never_follow_symlinks() {
    let path = std::env::temp_dir().join(format!(
        "lifecycle-storage-{}-{}",
        std::process::id(),
        crate::tracker::now_ms()
    ));
    private_directory(&path).unwrap();
    let record = path.join("record.json");
    write_private(&record, &json!({"phase":"ready"})).unwrap();
    assert_eq!(read_private(&record).unwrap()["phase"], "ready");
    assert!(write_private(&record, &json!({"phase":"changed"})).is_err());
    let alias = path.join("alias.json");
    std::os::unix::fs::symlink(&record, &alias).unwrap();
    assert!(read_private(&alias).is_err());
    fs::set_permissions(&record, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_private(&record).is_err());
    fs::remove_dir_all(path).unwrap();
}

#[test]
fn managed_worker_rejects_unknown_modes_without_executing_anything() {
    assert!(worker_command("anything", Path::new("/not-used")).is_err());
    assert!(spawn_job("../escape", "window", Path::new("/not-used")).is_err());
    assert!(spawn_job("job", "anything", Path::new("/not-used")).is_err());
}

#[test]
fn missing_worker_receipts_expire_without_blocking_future_operations_forever() {
    let directory = std::env::temp_dir().join(format!(
        "lifecycle-pending-{}-{}",
        std::process::id(),
        crate::tracker::now_ms()
    ));
    private_directory(&directory).unwrap();
    let request = directory.join("request.json");
    write_private(&request, &json!({})).unwrap();
    assert!(worker_pending_or_alive(&directory, &request));
    let old = std::time::SystemTime::now() - Duration::from_secs(11);
    File::options()
        .write(true)
        .open(&request)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(old))
        .unwrap();
    assert!(!worker_pending_or_alive(&directory, &request));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
#[ignore = "requires the user systemd manager; creates only a private no-op lifecycle service"]
fn real_user_service_executes_the_release_worker_and_retains_its_result() {
    let directory = std::env::temp_dir().join(format!(
        "lifecycle-service-test-{}-{}",
        std::process::id(),
        crate::tracker::now_ms()
    ));
    private_directory(&directory).unwrap();
    let request = directory.join("request.json");
    write_private(&request, &json!({"deliberately_invalid":true})).unwrap();
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("applicationlauncherd");
    assert!(
        binary.is_file(),
        "Build the release daemon before running this integration test"
    );
    let mut command = Command::new("systemd-run");
    command
        .args([
            "--user",
            "--quiet",
            "--wait",
            "--collect",
            "--service-type=exec",
            "--property=KillMode=process",
        ])
        .arg(format!(
            "--unit=applicationlauncher-lifecycle-test-{}",
            directory.file_name().unwrap().to_string_lossy()
        ))
        .arg(format!("--setenv=XDG_STATE_HOME={}", directory.display()))
        .arg("--setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=/not-a-real-lifecycle-test-bus")
        .arg("--")
        .arg(binary)
        .arg("--lifecycle-worker")
        .arg("program")
        .arg(&request);
    let output = crate::process::output_with_timeout(command, Duration::from_secs(15)).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        read_private(&directory.join("result.json")).unwrap()["ok"],
        false
    );
    assert!(!worker_alive(&directory));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn native_restore_locks_share_the_lifecycle_worker_exclusion_boundary() {
    let directory = std::env::temp_dir().join(format!(
        "lifecycle-lock-{}-{}",
        std::process::id(),
        crate::tracker::now_ms()
    ));
    let held = OperationLock::acquire_in(&directory).unwrap();
    assert!(OperationLock::busy_in(&directory).unwrap());
    assert!(OperationLock::acquire_in(&directory).is_err());
    drop(held);
    assert!(!OperationLock::busy_in(&directory).unwrap());
    let held = OperationLock::acquire_in(&directory).unwrap();
    drop(held);
    fs::set_permissions(
        directory.join("operation.lock"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(OperationLock::busy_in(&directory).is_err());
    assert!(OperationLock::acquire_in(&directory).is_err());
    fs::remove_file(directory.join("operation.lock")).unwrap();
    std::os::unix::fs::symlink(directory.join("missing"), directory.join("operation.lock"))
        .unwrap();
    assert!(OperationLock::busy_in(&directory).is_err());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn embedded_worker_publishes_a_minimal_identity_and_a_durable_failure_without_a_session_bus() {
    let directory = std::env::temp_dir().join(format!(
        "lifecycle-runner-{}-{}",
        std::process::id(),
        crate::tracker::now_ms()
    ));
    private_directory(&directory).unwrap();
    let request = directory.join("request.json");
    write_private(&request, &json!({"input":null})).unwrap();
    let output = worker_command("program", &request)
        .unwrap()
        .env("XDG_STATE_HOME", &directory)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/not-a-real-lifecycle-test-bus",
        )
        .output()
        .unwrap();
    assert!(output.status.success());
    let identity = read_private(&directory.join("worker.json")).unwrap();
    assert!(identity["pid"].as_u64().unwrap() > 1);
    assert!(identity["start"].as_u64().unwrap() > 0);
    assert!(identity.get("arguments").is_none());
    assert_eq!(
        read_private(&directory.join("result.json")).unwrap()["ok"],
        false
    );
    fs::remove_dir_all(directory).unwrap();
}
