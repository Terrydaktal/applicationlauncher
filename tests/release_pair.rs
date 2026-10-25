use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn release_pair_probes_share_identity_without_starting_runtime_services() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = PathBuf::from(format!(
        "/tmp/applicationlauncher-build-id-{}-{nonce}",
        std::process::id()
    ));
    let state = root.join("state");
    let runtime = root.join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    for executable in [
        env!("CARGO_BIN_EXE_applicationlauncher"),
        env!("CARGO_BIN_EXE_applicationlauncherd"),
    ] {
        let output = Command::new(executable)
            .arg("--build-id")
            .env("HOME", &root)
            .env("XDG_STATE_HOME", &state)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env_remove("APPLICATIONLAUNCHER_BUILD_ARCHIVE")
            .output()
            .unwrap();
        assert!(output.status.success(), "{executable}: {output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            applicationlauncher::BUILD_ID
        );
        assert!(!state.exists(), "build-ID probe wrote runtime state");
        assert_eq!(
            std::fs::read_dir(&runtime).unwrap().count(),
            0,
            "build-ID probe started a runtime endpoint"
        );
    }
    std::fs::remove_dir_all(&root).unwrap();
}
