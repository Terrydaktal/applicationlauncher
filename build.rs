use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;
const FNV_PRIME: u64 = 1_099_511_628_211;

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) {
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

fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let inputs = vec![
        manifest_dir.join("Cargo.toml"),
        manifest_dir.join("Cargo.lock"),
        manifest_dir.join("build.rs"),
        manifest_dir.join(".cargo"),
        manifest_dir.join("rust-toolchain"),
        manifest_dir.join("rust-toolchain.toml"),
        manifest_dir.join("src"),
        manifest_dir.join("kwin"),
        manifest_dir.join("../fuzzy-rank/Cargo.toml"),
        manifest_dir.join("../fuzzy-rank/src"),
    ];
    for input in &inputs {
        println!("cargo:rerun-if-changed={}", input.display());
    }

    let mut files = Vec::new();
    for input in &inputs {
        collect_files(&input, &mut files);
    }
    files.sort();
    files.dedup();

    let mut hash = FNV_OFFSET;
    for file in files {
        let relative = file
            .strip_prefix(&manifest_dir)
            .unwrap_or(&file)
            .to_string_lossy();
        hash = hash_bytes(hash, relative.as_bytes());
        hash = hash_bytes(hash, &[0]);
        if let Ok(contents) = fs::read(&file) {
            hash = hash_bytes(hash, &contents);
        }
        hash = hash_bytes(hash, &[0xff]);
    }

    let package_version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let profile = env::var("PROFILE").unwrap_or_default();
    let target = env::var("TARGET").unwrap_or_default();
    let build_id = format!("{package_version}-{profile}-{target}-{hash:016x}");
    println!("cargo:rustc-env=APPLICATIONLAUNCHER_BUILD_ID={build_id}");
}
