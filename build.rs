use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;
const FNV_PRIME: u64 = 1_099_511_628_211;

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) {
    if path.file_name().is_some_and(|name| name == "__pycache__")
        || path
            .extension()
            .is_some_and(|extension| extension == "pyc" || extension == "pyo")
    {
        return;
    }
    if path.is_file() {
        files.push(path.to_path_buf());
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let entry_path = entry.path();
        if entry_path.file_name().is_some_and(|name| name == ".git") {
            continue;
        }
        collect_files(&entry_path, files);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn python_inspection_caches_are_not_application_build_inputs() {
        let root =
            std::env::temp_dir().join(format!("launcher-build-inputs-{}", std::process::id()));
        std::fs::create_dir_all(root.join("__pycache__")).unwrap();
        std::fs::write(root.join("worker.py"), "source").unwrap();
        std::fs::write(root.join("__pycache__/worker.pyc"), "cache").unwrap();
        std::fs::write(root.join("worker.pyo"), "cache").unwrap();
        let mut files = Vec::new();
        super::collect_files(&root, &mut files);
        assert_eq!(files, [root.join("worker.py")]);
        std::fs::remove_dir_all(root).unwrap();
    }
}

fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source_dir = if let Ok(metadata) = fs::read(manifest_dir.join("source-root.json")) {
        let value: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
        PathBuf::from(value["root"].as_str().unwrap())
    } else {
        manifest_dir.clone()
    };
    app_version::emit(source_dir.to_str().unwrap(), "rust:.");
    if env::var_os("CARGO_FEATURE_RELEASE_MINIMAL").is_some() {
        assert!(
            manifest_dir.join("source-root.json").is_file(),
            "Use scripts/build-release-matrix: release-minimal requires the verified dependency-erasure workspace"
        );
    }
    println!("cargo:rerun-if-changed=build-support/lifecycle_sources.py");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("lifecycle");
    let mode = if env::var_os("CARGO_FEATURE_UPDATE_TIMINGS").is_some() {
        "observable"
    } else {
        "erased"
    };
    let mut features: Vec<_> = env::vars()
        .filter_map(|(name, _)| {
            name.strip_prefix("CARGO_FEATURE_")
                .map(|name| name.to_ascii_lowercase().replace('_', "-"))
        })
        .collect();
    features.sort();
    let generated = std::process::Command::new("/usr/bin/python3")
        .arg("-I")
        .arg(manifest_dir.join("build-support/lifecycle_sources.py"))
        .arg(&source_dir)
        .arg(&output)
        .arg(mode)
        .arg(format!("{features:?}"))
        .status()
        .expect("Python 3 is required to generate the lifecycle release postimages");
    assert!(generated.success(), "Lifecycle postimage generation failed");
    let inputs = vec![
        source_dir.join("debuggability.toml"),
        source_dir.join("Cargo.toml"),
        source_dir.join("Cargo.lock"),
        source_dir.join("APP-VERSION"),
        source_dir.join("build.rs"),
        source_dir.join("build-support"),
        source_dir.join(".cargo"),
        source_dir.join("rust-toolchain"),
        source_dir.join("rust-toolchain.toml"),
        source_dir.join("src"),
        source_dir.join("kwin"),
        source_dir.join("../fuzzy-rank/Cargo.toml"),
        source_dir.join("../fuzzy-rank/APP-VERSION"),
        source_dir.join("../fuzzy-rank/build.rs"),
        source_dir.join("../fuzzy-rank/build-support"),
        source_dir.join("../fuzzy-rank/src"),
        source_dir.join("../program-metadata/Cargo.toml"),
        source_dir.join("../program-metadata/src"),
    ];
    for input in &inputs {
        println!("cargo:rerun-if-changed={}", input.display());
    }

    let mut files = Vec::new();
    for input in &inputs {
        collect_files(input, &mut files);
    }
    files.sort();
    files.dedup();

    let mut source_inputs = Vec::new();
    let mut hash = hash_bytes(FNV_OFFSET, mode.as_bytes());
    hash = hash_bytes(hash, format!("{features:?}").as_bytes());
    for file in files {
        let relative = file
            .strip_prefix(&source_dir)
            .unwrap_or(&file)
            .to_string_lossy();
        hash = hash_bytes(hash, relative.as_bytes());
        hash = hash_bytes(hash, &[0]);
        if let Ok(contents) = fs::read(&file) {
            hash = hash_bytes(hash, &contents);
            use sha2::Digest;
            source_inputs.push(serde_json::json!({"path":relative,"sha256":format!("{:x}",sha2::Sha256::digest(&contents))}));
        }
        hash = hash_bytes(hash, &[0xff]);
    }

    let package_version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let profile = env::var("PROFILE").unwrap_or_default();
    let target = env::var("TARGET").unwrap_or_default();
    let build_id = format!("{package_version}-{profile}-{target}-{hash:016x}");
    println!("cargo:rustc-env=APPLICATIONLAUNCHER_BUILD_ID={build_id}");
    artifact_manifest::emit(&source_dir, &output, &features, &source_inputs);
}

#[path = "build-support/app_version.rs"]
mod app_version;

#[path = "build-support/artifact_manifest.rs"]
mod artifact_manifest;
