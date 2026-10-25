use std::fs::File;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct ScriptApplication {
    pub(crate) name: String,
    pub(crate) target: String,
    pub(crate) kind: &'static str,
    pub(crate) entry_point: Option<PathBuf>,
    pub(crate) identification: &'static str,
}

pub(crate) fn runtime_name(name: &str) -> Option<&'static str> {
    match name {
        "node" | "nodejs" => Some("Node.js"),
        "java" => Some("Java"),
        "python" | "python3" | "pypy" | "pypy3" => Some("Python"),
        _ if name.strip_prefix("python3.").is_some_and(|version| {
            let version = version.strip_suffix('t').unwrap_or(version);
            !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
        }) =>
        {
            Some("Python")
        }
        _ => None,
    }
}

pub(super) fn probe(runtime: &str, pid: i32) -> Option<ScriptApplication> {
    const LIMIT: u64 = 16 * 1024;
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(format!("/proc/{pid}/cmdline"))
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > LIMIT {
        return None;
    }
    let args = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(std::str::from_utf8)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok();
    identify(runtime, &args, cwd.as_deref())
}

fn identify(runtime: &str, args: &[&str], cwd: Option<&Path>) -> Option<ScriptApplication> {
    let mut index = 1;
    while let Some(&arg) = args.get(index) {
        index += 1;
        match (runtime, arg) {
            ("Python", "-c" | "-" | "--help" | "--version") => return None,
            ("Python", "-m") | ("Java", "-m" | "--module") => {
                return named_target(
                    args.get(index)?,
                    if runtime == "Python" {
                        "Python module"
                    } else {
                        "Java module"
                    },
                );
            }
            ("Python", "-W" | "-X" | "--check-hash-based-pycs") => {
                args.get(index)?;
                index += 1;
            }
            ("Python", option) if option.starts_with("-m") && option.len() > 2 => {
                return named_target(&option[2..], "Python module");
            }
            ("Java", option) if option.starts_with("--module=") => {
                return named_target(option.trim_start_matches("--module="), "Java module");
            }
            ("Python", "--") => return file_target(args.get(index)?, "Python script", cwd),
            ("Python", option) if option.starts_with("-W") || option.starts_with("-X") => {}
            ("Python", option)
                if option.starts_with('-')
                    && option[1..]
                        .chars()
                        .all(|ch| "bBdEhiIOPqsSuvVx".contains(ch)) => {}
            ("Python", option) if option.starts_with('-') => return None,
            ("Python", script) => return file_target(script, "Python script", cwd),
            ("Java", "-jar") => return file_target(args.get(index)?, "Java archive", cwd),
            (
                "Java",
                "-cp"
                | "-classpath"
                | "--class-path"
                | "-p"
                | "--module-path"
                | "--upgrade-module-path"
                | "--add-modules"
                | "--limit-modules"
                | "--add-opens"
                | "--add-exports"
                | "--add-reads"
                | "--patch-module"
                | "--enable-native-access"
                | "--source",
            ) => {
                args.get(index)?;
                index += 1;
            }
            (
                "Java",
                "-ea" | "-da" | "-esa" | "-dsa" | "-server" | "-client" | "--enable-preview",
            ) => {}
            ("Java", option)
                if option.starts_with("-X")
                    || option.starts_with("-D")
                    || option.starts_with("--class-path=")
                    || option.starts_with("--module-path=")
                    || option.starts_with("--enable-native-access=")
                    || option.starts_with("-javaagent:")
                    || option.starts_with("-agentlib:")
                    || option.starts_with("-agentpath:")
                    || option.starts_with("--add-") && option.contains('=') => {}
            ("Java", option) if option.starts_with(['-', '@']) => return None,
            ("Java", source) if source.ends_with(".java") => {
                return file_target(source, "Java source", cwd);
            }
            ("Java", class) => return named_target(class, "Java main class"),
            _ => return None,
        }
    }
    None
}

fn named_target(target: &str, kind: &'static str) -> Option<ScriptApplication> {
    if target.is_empty()
        || target.starts_with('-')
        || target.len() > 512
        || !target
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "._$/-".contains(ch))
    {
        return None;
    }
    Some(ScriptApplication {
        name: target.into(),
        target: target.into(),
        kind,
        entry_point: None,
        identification: "Process arguments (module/class file not inferred)",
    })
}

fn file_target(target: &str, kind: &'static str, cwd: Option<&Path>) -> Option<ScriptApplication> {
    if target.is_empty() || target.len() > 4096 || target.chars().any(char::is_control) {
        return None;
    }
    let path = Path::new(target);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd?.join(path)
    };
    let entry_point = std::fs::canonicalize(path).ok()?;
    if !entry_point.is_file() {
        return None;
    }
    let name = entry_point.file_stem()?.to_str()?.to_owned();
    Some(ScriptApplication {
        name,
        target: target.into(),
        kind,
        entry_point: Some(entry_point),
        identification: "Process file argument (file inspected without executing it)",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_and_archives_are_identified_without_running_them() {
        let root = std::env::temp_dir().join(format!(
            "al-runtime-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        for file in ["whisper_service.py", "viewer.jar", "Viewer.java"] {
            std::fs::write(root.join(file), "THIS MUST NEVER EXECUTE").unwrap();
        }
        for (runtime, args, name, kind) in [
            (
                "Python",
                vec![
                    "python3",
                    "-I",
                    "-u",
                    "-X",
                    "dev",
                    "whisper_service.py",
                    "--input",
                    "audio",
                ],
                "whisper_service",
                "Python script",
            ),
            (
                "Java",
                vec![
                    "java",
                    "-server",
                    "--enable-native-access=ALL-UNNAMED",
                    "-Xmx2g",
                    "-Dmode=preview",
                    "-jar",
                    "viewer.jar",
                ],
                "viewer",
                "Java archive",
            ),
            (
                "Java",
                vec!["java", "--source", "21", "Viewer.java"],
                "Viewer",
                "Java source",
            ),
        ] {
            let app = identify(runtime, &args, Some(&root)).unwrap();
            assert_eq!(app.name, name);
            assert_eq!(app.kind, kind);
            assert!(app.entry_point.unwrap().starts_with(&root));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn modules_and_classes_do_not_guess_their_files_or_versions() {
        for (runtime, args, expected) in [
            ("Python", vec!["python3", "-mhttp.server"], "http.server"),
            (
                "Java",
                vec!["java", "--module=app/org.example.Main"],
                "app/org.example.Main",
            ),
            (
                "Python",
                vec!["python3", "-m", "http.server"],
                "http.server",
            ),
            (
                "Java",
                vec!["java", "-cp", "somewhere.jar", "org.example.Viewer"],
                "org.example.Viewer",
            ),
            (
                "Java",
                vec![
                    "java",
                    "--module-path",
                    "modules",
                    "-m",
                    "app/org.example.Main",
                ],
                "app/org.example.Main",
            ),
        ] {
            let app = identify(runtime, &args, None).unwrap();
            assert_eq!(app.name, expected);
            assert!(app.entry_point.is_none());
        }
        for (runtime, args) in [
            ("Python", vec!["python3", "-c", "print('secret')"]),
            ("Python", vec!["python3", "-X"]),
            ("Python", vec!["python3", "--unknown", "guessed.py"]),
            ("Java", vec!["java", "@args.txt"]),
            ("Java", vec!["java", "-jar"]),
            ("Java", vec!["java", "--unknown", "GuessedClass"]),
        ] {
            assert!(identify(runtime, &args, None).is_none());
        }
        assert_eq!(runtime_name("python3.14t"), Some("Python"));
        assert_eq!(runtime_name("python3.evil"), None);
    }
}
