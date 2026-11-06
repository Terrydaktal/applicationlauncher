use std::fs::File;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;

const INVENTORY_SCHEMA_VERSION: u32 = 4;
const MAX_INVENTORY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PROGRAMS: usize = 8192;
const MAX_BINARIES: usize = 32768;
const MAX_INSTALLED_ALIASES: usize = 64;

#[derive(Debug)]
pub(super) struct InstalledExecutable {
    pub(super) path: PathBuf,
    aliases: Vec<PathBuf>,
}

impl InstalledExecutable {
    pub(super) fn launcher(&self) -> &Path {
        self.aliases
            .first()
            .map_or(self.path.as_path(), PathBuf::as_path)
    }
    fn original(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            aliases: Vec::new(),
        }
    }

    pub(super) fn unchanged(&self, metadata: &std::fs::Metadata) -> bool {
        let identity = super::executable_identity(metadata);
        std::iter::once(&self.path)
            .chain(&self.aliases)
            .all(|path| {
                std::fs::metadata(path)
                    .is_ok_and(|current| super::executable_identity(&current) == identity)
            })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgsMetadata {
    pub(crate) capabilities: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) package_type: Option<String>,
    pub(crate) root_packages: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Capabilities {
    has_gui: bool,
    has_cli: bool,
    has_library: bool,
    has_service: bool,
    has_job: bool,
    has_plugin: bool,
    is_meta: bool,
}

impl Capabilities {
    fn summary(&self) -> String {
        let roles = [
            (self.is_meta, "META", "Meta-package"),
            (self.has_gui, "GUI", "GUI application"),
            (self.has_service, "SERVICE", "Service / daemon"),
            (self.has_job, "JOB", "One-shot service job"),
            (self.has_cli, "CLI", "Command-line tool"),
            (self.has_plugin, "PLUGIN", "Plugin / extension"),
            (self.has_library, "LIB", "Library"),
        ];
        let primary = roles
            .iter()
            .find(|(enabled, _, _)| *enabled)
            .map_or("Data / support package", |(_, _, role)| *role);
        let mut tags = [
            (self.is_meta, "META"),
            (self.has_gui, "GUI"),
            (self.has_cli, "CLI"),
            (self.has_library, "LIB"),
            (self.has_service, "SERVICE"),
            (self.has_job, "JOB"),
            (self.has_plugin, "PLUGIN"),
        ]
        .into_iter()
        .filter_map(|(enabled, tag)| enabled.then_some(tag))
        .collect::<Vec<_>>();
        if tags.is_empty() {
            tags.push("DATA");
        }
        format!(
            "{}  \u{2022}  Primary role: {primary}",
            tags.join(" \u{00b7} ")
        )
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ProgramState {
    dev: bool,
    fork: bool,
    cloned: bool,
    unclassified: bool,
    broken: bool,
    binary: bool,
    script: bool,
    opt: bool,
    orphan: bool,
}

#[derive(Deserialize)]
struct Binary {
    name: String,
    path: PathBuf,
    #[serde(default)]
    target: PathBuf,
}

impl Binary {
    fn paths(&self) -> impl Iterator<Item = PathBuf> + '_ {
        let target = (!self.target.as_os_str().is_empty()).then(|| {
            if self.target.is_absolute() {
                self.target.clone()
            } else {
                self.path
                    .parent()
                    .unwrap_or(Path::new("/"))
                    .join(&self.target)
            }
        });
        std::iter::once(self.path.clone())
            .chain(target)
            .filter_map(normalized_path)
    }
}

#[derive(Deserialize)]
struct Program {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "_owning_pkg")]
    owning_package: String,
    origin: String,
    install_role: String,
    #[serde(default)]
    state: ProgramState,
    #[serde(default)]
    desc: String,
    #[serde(default)]
    capabilities: Option<Capabilities>,
    binaries: Vec<Binary>,
}

impl Program {
    fn metadata(&self) -> ProgsMetadata {
        ProgsMetadata {
            capabilities: self.capabilities.as_ref().map(Capabilities::summary),
            description: (!self.desc.trim().is_empty()).then(|| self.desc.trim().to_owned()),
            ..Default::default()
        }
    }

    fn package_name(&self) -> &str {
        if self.owning_package.is_empty() {
            &self.name
        } else {
            &self.owning_package
        }
    }

    fn tool_label(&self) -> Option<String> {
        // Match Progs' provided_tool_identity, keeping local build classifications intact.
        let (badge, summary) = match (self.origin.as_str(), self.install_role.as_str()) {
            ("Pacman", "Explicit") => ("PET", "Pacman Explicit Tool"),
            ("Pacman", "Dependency") => ("PDT", "Pacman Dependency Tool"),
            ("Pacman", _) => ("PPT", "Pacman Provided Tool"),
            ("Aur", "Explicit") => ("AET", "AUR Explicit Tool"),
            ("Aur", "Dependency") => ("ADT", "AUR Dependency Tool"),
            ("Aur", _) => ("APT", "AUR Provided Tool"),
            ("Cargo", "Explicit") => ("CET", "Cargo Explicit Tool"),
            ("Cargo", _) => ("CUT", "Cargo Unknown Tool"),
            ("Npm", "Explicit") => ("NET", "npm Explicit Tool"),
            ("Npm", "Dependency") => ("NDT", "npm Dependency Tool"),
            ("Npm", _) => ("NUT", "npm Unknown Tool"),
            ("Uv", "Explicit") => ("UET", "uv Explicit Tool"),
            ("Uv", "Dependency") => ("UDT", "uv Dependency Tool"),
            ("Uv", _) => ("UUT", "uv Unknown Tool"),
            _ => return self.label(),
        };
        let management = if matches!(self.origin.as_str(), "Pacman" | "Aur") {
            "Package-managed"
        } else {
            "Toolchain-managed"
        };
        Some(format!("[{badge}] {summary} ({management})"))
    }

    fn label(&self) -> Option<String> {
        // These badges and summaries follow progs/src/models.rs, not package-name guesses.
        let package_managed = matches!(self.origin.as_str(), "Pacman" | "Aur");
        let (role, package_badge) = match (self.origin.as_str(), self.install_role.as_str()) {
            ("Pacman", "Explicit") => ("Explicit", "PEP"),
            ("Pacman", "Dependency") => ("Dependency", "PDP"),
            ("Pacman", "Standalone") => ("Standalone", "PSP"),
            ("Pacman", "ToolchainManaged") => ("Toolchain-managed", "PSP"),
            ("Aur", "Explicit") => ("Explicit", "AEP"),
            ("Aur", "Dependency") => ("Dependency", "ADP"),
            ("Aur", "Standalone") => ("Standalone", "ASP"),
            ("Aur", "ToolchainManaged") => ("Toolchain-managed", "ASP"),
            (_, _) if package_managed => return None,
            ("Uv", "Explicit") => ("Explicit", "UEP"),
            ("Uv", "Dependency") => ("Dependency", "UDP"),
            ("Uv", _) => ("Unknown install reason", "UUP"),
            ("Npm", "Explicit") => ("Explicit", "NEP"),
            ("Npm", "Dependency") => ("Dependency", "NDP"),
            ("Npm", _) => ("Unknown install reason", "NUP"),
            ("Cargo", "Explicit") => ("Explicit", "CEP"),
            ("Cargo", _) => ("Unknown install reason", "CUP"),
            ("Rustup", _) => ("", "RUT"),
            _ => ("", "UNC"),
        };
        let (source, management, local_badge) = if self.state.broken {
            ("BRK", "Broken", "BRK")
        } else if self.state.dev {
            ("DEV", "User-managed", "DEV")
        } else if self.state.fork {
            ("Fork", "User-managed", "FRK")
        } else if self.state.cloned {
            ("Git Clone", "External source", "CLO")
        } else if self.state.unclassified {
            ("UNC", "Unclassified", "UNC")
        } else if self.state.opt {
            ("opt", "External/manual", "OPT")
        } else if self.state.script {
            ("USC", "Unmanaged", "USC")
        } else if self.state.binary {
            ("UBI", "Unmanaged", "UBI")
        } else {
            ("UNC", "Unclassified", "UNC")
        };
        let badge = if package_managed && self.state.orphan {
            if self.origin == "Aur" { "AOP" } else { "POP" }
        } else if self.origin == "Aur" {
            package_badge
        } else if self.state.broken {
            "BRK"
        } else if matches!(self.origin.as_str(), "Uv" | "Npm" | "Cargo" | "Rustup") {
            package_badge
        } else if local_badge != "UNC" || self.state.unclassified {
            local_badge
        } else {
            package_badge
        };
        let summary = match self.origin.as_str() {
            "Pacman" => format!("Pacman {role} (Package-managed)"),
            "Aur" => format!("AUR {role} (Package-managed)"),
            "Uv" => format!("uv {role} (Toolchain-managed)"),
            "Npm" => format!("npm {role} (Toolchain-managed)"),
            "Cargo" => format!("Cargo {role} (Toolchain-managed)"),
            "Rustup" => "rustup (Toolchain-managed)".into(),
            "Local" => format!("{source} ({management})"),
            _ => return None,
        };
        Some(format!("[{badge}] {summary}"))
    }
}

#[derive(Deserialize)]
struct Scan {
    apps: Vec<Program>,
}

#[derive(Deserialize)]
struct Inventory {
    schema_version: u32,
    scan: Scan,
}

fn normalized_path(path: PathBuf) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            component => result.push(component.as_os_str()),
        }
    }
    Some(result)
}

fn checkout_root(executable: &Path) -> Option<&Path> {
    executable
        .ancestors()
        .skip(1)
        .take(16)
        .find(|directory| directory.join(".git").exists())
}

fn unique_label<'a>(
    programs: impl Iterator<Item = &'a Program>,
    classify: fn(&Program) -> Option<String>,
) -> Result<Option<String>, ()> {
    let mut label = None;
    for candidate in programs {
        let current = classify(candidate).ok_or(())?;
        if label.as_ref().is_some_and(|previous| previous != &current) {
            return Err(());
        }
        label = Some(current);
    }
    Ok(label)
}

impl Inventory {
    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() as u64 > MAX_INVENTORY_BYTES {
            return None;
        }
        let inventory: Self = serde_json::from_slice(bytes).ok()?;
        (inventory.schema_version == INVENTORY_SCHEMA_VERSION
            && inventory.scan.apps.len() <= MAX_PROGRAMS
            && inventory
                .scan
                .apps
                .iter()
                .map(|app| app.binaries.len())
                .sum::<usize>()
                <= MAX_BINARIES)
            .then_some(inventory)
    }

    fn metadata_for(&self, programs: &[&Program]) -> ProgsMetadata {
        let Some(first) = programs.first() else {
            return ProgsMetadata::default();
        };
        let mut metadata = first.metadata();
        if !programs
            .iter()
            .all(|program| program.metadata() == metadata)
        {
            return ProgsMetadata::default();
        }
        let name = first.package_name();
        if matches!(first.origin.as_str(), "Pacman" | "Aur")
            && super::valid_package_name(name)
            && programs.iter().all(|program| {
                matches!(program.origin.as_str(), "Pacman" | "Aur")
                    && program.package_name() == name
            })
        {
            metadata.package_type = unique_label(programs.iter().copied(), Program::label)
                .ok()
                .flatten();
        }
        metadata
    }

    fn matching_programs(&self, executable: &Path) -> Option<Vec<&Program>> {
        let executable = normalized_path(executable.to_owned())?;
        let exact = self
            .scan
            .apps
            .iter()
            .filter(|app| {
                app.binaries
                    .iter()
                    .any(|binary| binary.paths().any(|path| path == executable))
            })
            .collect::<Vec<_>>();
        if !exact.is_empty() {
            return Some(exact);
        }

        // Progs may index a fork's launch wrapper rather than its compiled ELF.
        // Require both the same checkout and executable name, never the name alone.
        let checkout = checkout_root(&executable)?;
        let name = executable.file_name()?.to_str()?;
        Some(
            self.scan
                .apps
                .iter()
                .filter(|app| {
                    app.origin == "Local"
                        && (app.state.dev || app.state.fork || app.state.cloned)
                        && app.binaries.iter().any(|binary| {
                            binary.name == name
                                && binary.paths().any(|path| path.starts_with(checkout))
                        })
                })
                .collect(),
        )
    }

    fn inspect(&self, executable: &Path) -> (Option<String>, ProgsMetadata) {
        let Some(programs) = self.matching_programs(executable) else {
            return (None, ProgsMetadata::default());
        };
        let label = unique_label(programs.iter().copied(), Program::tool_label)
            .ok()
            .flatten();
        let metadata = self.metadata_for(&programs);
        (label, metadata)
    }

    fn inspect_owner(&self, package: &str) -> (Option<String>, ProgsMetadata) {
        let programs = self
            .scan
            .apps
            .iter()
            .filter(|program| {
                matches!(program.origin.as_str(), "Pacman" | "Aur")
                    && if program.owning_package.is_empty() {
                        program.name == package
                    } else {
                        program.owning_package == package
                    }
            })
            .collect::<Vec<_>>();
        let label = unique_label(programs.iter().copied(), Program::tool_label)
            .ok()
            .flatten();
        let metadata = self.metadata_for(&programs);
        (label, metadata)
    }

    #[cfg(test)]
    fn lookup(&self, executable: &Path) -> Option<String> {
        self.inspect(executable).0
    }

    fn installed_executable(&self, executable: &Path) -> Option<InstalledExecutable> {
        let mut result = InstalledExecutable::original(executable);
        let Some(checkout) = checkout_root(executable) else {
            return Some(result);
        };
        let checkout = std::fs::canonicalize(checkout).ok()?;
        let name = executable.file_name()?.to_str()?;
        for program in self.matching_programs(executable).unwrap_or_default() {
            if program.origin != "Local"
                || program.state.broken
                || !(program.state.dev || program.state.fork || program.state.cloned)
            {
                continue;
            }
            for binary in program.binaries.iter().filter(|binary| binary.name == name) {
                let Some(alias) = normalized_path(binary.path.clone()) else {
                    continue;
                };
                let Ok(current) = std::fs::canonicalize(&alias) else {
                    continue;
                };
                if current == alias
                    || !current.starts_with(&checkout)
                    || current.file_name() != executable.file_name()
                {
                    continue;
                }
                // Follow published links, never choose a build by timestamp or basename alone.
                if !result.aliases.is_empty() && result.path != current {
                    return None;
                }
                result.path = current;
                if !result.aliases.contains(&alias) {
                    if result.aliases.len() >= MAX_INSTALLED_ALIASES {
                        return None;
                    }
                    result.aliases.push(alias);
                }
            }
        }
        Some(result)
    }

    fn launch_alias(&self, executable: &Path) -> Option<PathBuf> {
        let expected = std::fs::canonicalize(executable).ok()?;
        self.matching_programs(executable)?
            .into_iter()
            .flat_map(|program| &program.binaries)
            .take(MAX_INSTALLED_ALIASES)
            .filter_map(|binary| normalized_path(binary.path.clone()))
            .find(|alias| {
                alias != &expected && std::fs::canonicalize(alias).ok().as_ref() == Some(&expected)
            })
    }

    fn package_name(&self, executable: &Path) -> Option<(String, bool)> {
        let mut result = None;
        for program in self.matching_programs(executable)? {
            // Only forks/clones have an inferred upstream package; original local tools do not.
            let inferred = match program.origin.as_str() {
                "Pacman" | "Aur" => false,
                "Local"
                    if !program.state.dev
                        && !program.state.broken
                        && (program.state.fork || program.state.cloned) =>
                {
                    true
                }
                _ => return None,
            };
            let name = if program.owning_package.is_empty() {
                &program.name
            } else {
                &program.owning_package
            };
            if !super::valid_package_name(name) {
                return None;
            }
            let candidate = (name.clone(), inferred);
            if result
                .as_ref()
                .is_some_and(|previous| previous != &candidate)
            {
                return None;
            }
            result = Some(candidate);
        }
        result
    }
}

#[derive(Default)]
struct Cache {
    key: Option<(PathBuf, super::ExecutableIdentity)>,
    inventory: Option<Arc<Inventory>>,
}

fn load(path: &Path) -> Option<Arc<Inventory>> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_INVENTORY_BYTES {
        return None;
    }
    let key = (path.to_owned(), super::executable_identity(&metadata));
    let mut cache = CACHE.get_or_init(Default::default).lock().ok()?;
    if cache.key.as_ref() != Some(&key) {
        let mut bytes = Vec::new();
        file.take(MAX_INVENTORY_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        cache.inventory = Inventory::parse(&bytes).map(Arc::new);
        cache.key = Some(key);
    }
    cache.inventory.clone()
}

fn inventory() -> Option<Arc<Inventory>> {
    let path = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .map(|directory| directory.join("progs/scan-v1.json"));
    path.as_deref().and_then(load)
}

pub(super) fn package_name(executable: &Path) -> Option<(String, bool)> {
    inventory()?.package_name(executable)
}

pub(super) fn launch_alias(executable: &Path) -> Option<PathBuf> {
    inventory()?.launch_alias(executable)
}

pub(super) fn installed_executable(executable: &Path) -> Option<InstalledExecutable> {
    match inventory() {
        Some(inventory) => inventory.installed_executable(executable),
        None => Some(InstalledExecutable::original(executable)),
    }
}

pub(super) fn inspect(executable: &Path) -> (String, ProgsMetadata) {
    inspect_with_owner(executable, None)
}

pub(super) fn inspect_with_owner(
    executable: &Path,
    installed_package: Option<&str>,
) -> (String, ProgsMetadata) {
    let inventory = inventory();
    let owner_name = installed_package
        .and_then(|value| value.split_whitespace().next())
        .filter(|name| super::valid_package_name(name));
    let (label, metadata) = inventory
        .as_ref()
        .map(|inventory| {
            let (label, metadata) = inventory.inspect(executable);
            let owner = owner_name
                .map(|name| inventory.inspect_owner(name))
                .unwrap_or_default();
            (
                label.or(owner.0),
                ProgsMetadata {
                    capabilities: metadata.capabilities.or(owner.1.capabilities),
                    description: metadata.description.or(owner.1.description),
                    package_type: if installed_package.is_some() {
                        owner.1.package_type
                    } else {
                        metadata.package_type
                    },
                    root_packages: None,
                },
            )
        })
        .unwrap_or_default();
    let label = label.unwrap_or_else(|| {
        format!(
            "[UNC] UNC (Unclassified); Progs inventory {}",
            if inventory.is_some() {
                "has no unambiguous match"
            } else {
                "unavailable; refresh Progs"
            }
        )
    });
    (label, metadata)
}

#[cfg(test)]
fn describe(executable: &Path) -> String {
    inspect(executable).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn program(origin: &str, role: &str, state: serde_json::Value) -> serde_json::Value {
        json!({"name": "tool", "origin": origin, "install_role": role, "state": state,
            "binaries": [{"name": "tool", "path": "/bin/tool", "target": ""}]})
    }

    fn inventory(apps: Vec<serde_json::Value>) -> Vec<u8> {
        serde_json::to_vec(&json!({"schema_version": 4, "scan": {"apps": apps}})).unwrap()
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "launcher-progs-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn upstream_package_candidates_require_a_fork_clone_or_managed_package() {
        for (origin, state, expected) in [
            ("Local", json!({"dev": true}), false),
            ("Local", json!({"fork": true}), true),
            ("Local", json!({"cloned": true}), true),
            ("Local", json!({"dev": true, "broken": true}), false),
            ("Local", json!({"dev": true, "fork": true}), false),
            ("Local", json!({"fork": true, "broken": true}), false),
            ("Local", json!({"script": true}), false),
            ("Pacman", json!({}), true),
            ("Aur", json!({}), true),
            ("Npm", json!({"dev": true}), false),
        ] {
            let inventory =
                Inventory::parse(&inventory(vec![program(origin, "Explicit", state)])).unwrap();
            assert_eq!(
                inventory.package_name(Path::new("/bin/tool")),
                expected.then(|| ("tool".into(), origin == "Local")),
                "{origin}: {:?}",
                inventory.lookup(Path::new("/bin/tool"))
            );
        }
        let conflicting = Inventory::parse(&inventory(vec![
            program("Local", "Standalone", json!({"dev": true})),
            program("Local", "Standalone", json!({"fork": true})),
        ]))
        .unwrap();
        assert!(conflicting.package_name(Path::new("/bin/tool")).is_none());
        assert!(
            conflicting
                .package_name(Path::new("/unknown/tool"))
                .is_none()
        );
    }

    #[test]
    fn capabilities_follow_progs_tag_order_and_primary_role_precedence() {
        let mut flags = json!({
            "is_meta": true, "has_gui": true, "has_service": true,
            "has_job": true, "has_cli": true, "has_plugin": true, "has_library": true
        });
        let all: Capabilities = serde_json::from_value(flags.clone()).unwrap();
        assert_eq!(
            all.summary(),
            "META \u{00b7} GUI \u{00b7} CLI \u{00b7} LIB \u{00b7} SERVICE \u{00b7} JOB \u{00b7} PLUGIN  \u{2022}  Primary role: Meta-package"
        );
        for (flag, role) in [
            ("is_meta", "Meta-package"),
            ("has_gui", "GUI application"),
            ("has_service", "Service / daemon"),
            ("has_job", "One-shot service job"),
            ("has_cli", "Command-line tool"),
            ("has_plugin", "Plugin / extension"),
            ("has_library", "Library"),
        ] {
            let capabilities: Capabilities = serde_json::from_value(flags.clone()).unwrap();
            assert!(
                capabilities
                    .summary()
                    .ends_with(&format!("Primary role: {role}"))
            );
            flags[flag] = json!(false);
        }
        let empty: Capabilities = serde_json::from_value(flags).unwrap();
        assert_eq!(
            empty.summary(),
            "DATA  \u{2022}  Primary role: Data / support package"
        );
        let gui: Capabilities =
            serde_json::from_value(json!({"has_gui": true, "has_cli": true})).unwrap();
        assert_eq!(
            gui.summary(),
            "GUI \u{00b7} CLI  \u{2022}  Primary role: GUI application"
        );
    }

    #[test]
    fn progs_metadata_preserves_descriptions_and_distinguishes_missing_from_data_only() {
        let mut record = program("Pacman", "Explicit", json!({}));
        let missing: Program = serde_json::from_value(record.clone()).unwrap();
        assert_eq!(missing.metadata(), ProgsMetadata::default());
        record["capabilities"] = json!({"has_gui": true, "has_cli": true});
        record["desc"] = json!("A cross-platform, GPU-accelerated terminal emulator");
        let inventory = Inventory::parse(&inventory(vec![record.clone()])).unwrap();
        let (_, metadata) = inventory.inspect(Path::new("/bin/tool"));
        assert_eq!(
            metadata.capabilities.as_deref(),
            Some("GUI \u{00b7} CLI  \u{2022}  Primary role: GUI application")
        );
        assert_eq!(
            metadata.description.as_deref(),
            Some("A cross-platform, GPU-accelerated terminal emulator")
        );
        assert_eq!(
            inventory.inspect(Path::new("/unrelated/tool")).1,
            ProgsMetadata::default()
        );
        record["capabilities"] = json!({});
        record["desc"] = json!(" \t ");
        let data: Program = serde_json::from_value(record).unwrap();
        assert_eq!(
            data.metadata().capabilities.as_deref(),
            Some("DATA  \u{2022}  Primary role: Data / support package")
        );
        assert!(data.metadata().description.is_none());
    }

    #[test]
    fn executable_names_do_not_have_to_match_their_owning_package_name() {
        let bytes = inventory(vec![json!({
            "name": "bubblewrap", "_owning_pkg": "bubblewrap", "origin": "Pacman",
            "install_role": "Dependency", "desc": "Unprivileged sandboxing tool",
            "capabilities": {"has_cli": true},
            "binaries": [{"name": "bwrap", "path": "/usr/bin/bwrap", "target": ""}]
        })]);
        let inventory = Inventory::parse(&bytes).unwrap();
        let path = Path::new("/usr/bin/bwrap");
        let (label, metadata) = inventory.inspect(path);
        assert!(label.unwrap().contains("Pacman Dependency"));
        assert_eq!(
            metadata.description.as_deref(),
            Some("Unprivileged sandboxing tool")
        );
        assert!(metadata.capabilities.as_deref().unwrap().starts_with("CLI"));
        assert_eq!(
            inventory.package_name(path),
            Some(("bubblewrap".into(), false))
        );
    }

    #[test]
    fn progs_metadata_rejects_conflicting_matches_even_when_installation_labels_agree() {
        let mut first = program("Local", "Standalone", json!({"dev": true}));
        first["desc"] = json!("First application");
        first["capabilities"] = json!({"has_cli": true});
        let mut second = first.clone();
        let duplicate = Inventory::parse(&inventory(vec![first.clone(), second.clone()])).unwrap();
        assert_eq!(
            duplicate
                .inspect(Path::new("/bin/tool"))
                .1
                .description
                .as_deref(),
            Some("First application")
        );
        second["desc"] = json!("Different application");
        let conflicting =
            Inventory::parse(&inventory(vec![first.clone(), second.clone()])).unwrap();
        assert_eq!(
            conflicting.inspect(Path::new("/bin/tool")).1,
            ProgsMetadata::default()
        );
        second["desc"] = first["desc"].clone();
        second["capabilities"] = json!({"has_gui": true});
        let conflicting = Inventory::parse(&inventory(vec![first, second])).unwrap();
        assert_eq!(
            conflicting.inspect(Path::new("/bin/tool")).1,
            ProgsMetadata::default()
        );
    }

    #[test]
    fn installation_labels_follow_progs_badges_and_management_names() {
        for (origin, role, state, expected) in [
            (
                "Pacman",
                "Explicit",
                json!({}),
                "[PEP] Pacman Explicit (Package-managed)",
            ),
            (
                "Pacman",
                "Dependency",
                json!({}),
                "[PDP] Pacman Dependency (Package-managed)",
            ),
            (
                "Pacman",
                "Standalone",
                json!({}),
                "[PSP] Pacman Standalone (Package-managed)",
            ),
            (
                "Aur",
                "Explicit",
                json!({}),
                "[AEP] AUR Explicit (Package-managed)",
            ),
            (
                "Aur",
                "Dependency",
                json!({}),
                "[ADP] AUR Dependency (Package-managed)",
            ),
            (
                "Aur",
                "Standalone",
                json!({}),
                "[ASP] AUR Standalone (Package-managed)",
            ),
            (
                "Pacman",
                "Dependency",
                json!({"orphan": true}),
                "[POP] Pacman Dependency (Package-managed)",
            ),
            (
                "Uv",
                "Standalone",
                json!({}),
                "[UUP] uv Unknown install reason (Toolchain-managed)",
            ),
            (
                "Npm",
                "ToolchainManaged",
                json!({}),
                "[NUP] npm Unknown install reason (Toolchain-managed)",
            ),
            (
                "Cargo",
                "ToolchainManaged",
                json!({}),
                "[CUP] Cargo Unknown install reason (Toolchain-managed)",
            ),
            (
                "Local",
                "Standalone",
                json!({"dev": true}),
                "[DEV] DEV (User-managed)",
            ),
            (
                "Local",
                "Standalone",
                json!({"fork": true}),
                "[FRK] Fork (User-managed)",
            ),
            (
                "Local",
                "Standalone",
                json!({"cloned": true}),
                "[CLO] Git Clone (External source)",
            ),
            (
                "Local",
                "Standalone",
                json!({"broken": true}),
                "[BRK] BRK (Broken)",
            ),
            (
                "Local",
                "Standalone",
                json!({"opt": true}),
                "[OPT] opt (External/manual)",
            ),
            (
                "Local",
                "Standalone",
                json!({"script": true}),
                "[USC] USC (Unmanaged)",
            ),
            (
                "Local",
                "Standalone",
                json!({"binary": true}),
                "[UBI] UBI (Unmanaged)",
            ),
            ("Local", "Standalone", json!({}), "[UNC] UNC (Unclassified)"),
        ] {
            let record: Program = serde_json::from_value(program(origin, role, state)).unwrap();
            assert_eq!(record.label().as_deref(), Some(expected));
        }
        let unknown: Program =
            serde_json::from_value(program("FutureSource", "Standalone", json!({}))).unwrap();
        assert!(unknown.label().is_none());
    }

    #[test]
    fn installation_matches_exact_paths_and_relative_symlink_targets_not_just_names() {
        let mut record = program("Cargo", "ToolchainManaged", json!({}));
        record["binaries"] = json!([{"name":"tool", "path":"/home/test/.local/bin/tool", "target":"../../../Dev/tool/target/release/tool"}]);
        let inventory = Inventory::parse(&inventory(vec![record])).unwrap();
        for path in [
            "/home/test/.local/bin/tool",
            "/home/Dev/tool/target/release/tool",
        ] {
            assert_eq!(
                inventory.lookup(Path::new(path)).as_deref(),
                Some("[CUT] Cargo Unknown Tool (Toolchain-managed)")
            );
        }
        assert!(inventory.lookup(Path::new("/unrelated/tool")).is_none());
        assert!(inventory.lookup(Path::new("tool")).is_none());
    }

    #[test]
    fn available_package_mapping_preserves_ownership_and_does_not_reinterpret_npm_commands() {
        let npm = Inventory::parse(&inventory(vec![program(
            "Npm",
            "ToolchainManaged",
            json!({}),
        )]))
        .unwrap();
        assert!(npm.package_name(Path::new("/bin/tool")).is_none());
        let script = Inventory::parse(&inventory(vec![program(
            "Local",
            "Standalone",
            json!({"script": true}),
        )]))
        .unwrap();
        assert!(script.package_name(Path::new("/bin/tool")).is_none());
        let mut package = program("Pacman", "Explicit", json!({}));
        package["_owning_pkg"] = json!("actual-owning-package");
        let managed = Inventory::parse(&inventory(vec![package])).unwrap();
        assert_eq!(
            managed.package_name(Path::new("/bin/tool")),
            Some(("actual-owning-package".into(), false))
        );
        let local = Inventory::parse(&inventory(vec![program(
            "Local",
            "Standalone",
            json!({"fork": true}),
        )]))
        .unwrap();
        assert_eq!(
            local.package_name(Path::new("/bin/tool")),
            Some(("tool".into(), true))
        );
        assert!(local.package_name(Path::new("/unrelated/tool")).is_none());
    }

    #[test]
    fn installed_symlink_selects_the_current_build_not_the_retained_running_image() {
        let root = Fixture::new();
        let checkout = root.0.join("checkout");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        let old = checkout.join("build/release.old/tool");
        let new = checkout.join("build/release.new/src/tool");
        for path in [&old, &new] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "test build").unwrap();
        }
        let runtime = checkout.join("build/runtime");
        std::os::unix::fs::symlink("release.new/src", &runtime).unwrap();
        let alias = root.0.join("tool");
        std::os::unix::fs::symlink(runtime.join("tool"), &alias).unwrap();
        let mut record = program("Local", "Standalone", json!({"fork": true}));
        // A stale inventory target/version must not override the actual installed link.
        record["binaries"] = json!([{"name":"tool", "path":alias, "target":old, "version":"old"}]);
        let inventory = Inventory::parse(&inventory(vec![record])).unwrap();
        let installed = inventory.installed_executable(&old).unwrap();
        assert_eq!(installed.path, new);
        assert_eq!(installed.aliases, vec![alias.clone()]);
        let pinned = std::fs::metadata(&new).unwrap();
        assert!(installed.unchanged(&pinned));
        assert!(old.exists(), "the old running build must not be removed");

        std::fs::remove_file(&runtime).unwrap();
        std::os::unix::fs::symlink("release.old", &runtime).unwrap();
        assert!(
            !installed.unchanged(&pinned),
            "retargeting the installation during a probe must invalidate the result"
        );
    }

    #[test]
    fn installed_aliases_must_agree_on_one_build_and_stay_in_the_same_checkout() {
        let root = Fixture::new();
        let checkout = root.0.join("checkout");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        let old = checkout.join("old/tool");
        let new = checkout.join("new/tool");
        let foreign = root.0.join("another-checkout/tool");
        for path in [&old, &new, &foreign] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "test build").unwrap();
        }
        let first = root.0.join("first/tool");
        let second = root.0.join("second/tool");
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&new, &first).unwrap();
        std::os::unix::fs::symlink(&old, &second).unwrap();
        let mut record = program("Local", "Standalone", json!({"fork": true}));
        record["binaries"] = json!([
            {"name":"tool", "path":first, "target":old},
            {"name":"tool", "path":second, "target":old}
        ]);
        let ambiguous = Inventory::parse(&inventory(vec![record.clone()])).unwrap();
        assert!(ambiguous.installed_executable(&old).is_none());

        std::fs::remove_file(&second).unwrap();
        std::os::unix::fs::symlink(&new, &second).unwrap();
        let installed = ambiguous.installed_executable(&old).unwrap();
        assert_eq!(installed.path, new);
        assert_eq!(installed.aliases.len(), 2);
        record["binaries"] = json!([{"name":"tool", "path":first, "target":old}]);
        std::fs::remove_file(&first).unwrap();
        std::os::unix::fs::symlink(&foreign, &first).unwrap();
        let unrelated = Inventory::parse(&inventory(vec![record])).unwrap();
        let installed = unrelated.installed_executable(&old).unwrap();
        assert_eq!(installed.path, old);
        assert!(installed.aliases.is_empty());
    }

    #[test]
    fn latest_build_resolution_does_not_replace_package_or_runtime_identity() {
        let root = Fixture::new();
        let checkout = root.0.join("checkout");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        let old = checkout.join("old/tool");
        let new = checkout.join("new/tool");
        for path in [&old, &new] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "test build").unwrap();
        }
        let alias = root.0.join("tool");
        std::os::unix::fs::symlink(&new, &alias).unwrap();
        for origin in ["Pacman", "Aur", "Npm", "Uv", "Cargo"] {
            let mut record = program(origin, "Standalone", json!({"fork":true}));
            record["binaries"] = json!([{"name":"tool", "path":alias, "target":old}]);
            let inventory = Inventory::parse(&inventory(vec![record])).unwrap();
            assert_eq!(
                inventory.installed_executable(&old).unwrap().path,
                old,
                "{origin}"
            );
        }
        let mut record = program("Local", "Standalone", json!({"fork":true}));
        record["binaries"] = json!([{"name":"another-tool", "path":alias, "target":old}]);
        let different_name = Inventory::parse(&inventory(vec![record])).unwrap();
        assert_eq!(different_name.installed_executable(&old).unwrap().path, old);
        let empty = Inventory::parse(&inventory(vec![])).unwrap();
        assert_eq!(empty.installed_executable(&old).unwrap().path, old);
    }

    #[test]
    fn installation_maps_a_fork_wrapper_to_its_own_compiled_binary_only() {
        let root = Fixture::new();
        let checkout = root.0.join("checkout");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        let mut record = program("Local", "Standalone", json!({"fork": true}));
        record["name"] = json!("xfce4-terminal");
        record["desc"] = json!("Local terminal fork");
        record["capabilities"] = json!({"has_gui": true, "has_cli": true});
        record["binaries"] = json!([{"name":"xfce4-terminal", "path":root.0.join("bin/xfce4-terminal"), "target":checkout.join("run-patched-xfce4-terminal.sh")}]);
        let inventory = Inventory::parse(&inventory(vec![record])).unwrap();
        assert_eq!(
            inventory
                .lookup(&checkout.join("build/terminal/xfce4-terminal"))
                .as_deref(),
            Some("[FRK] Fork (User-managed)")
        );
        let metadata = inventory
            .inspect(&checkout.join("build/terminal/xfce4-terminal"))
            .1;
        assert_eq!(metadata.description.as_deref(), Some("Local terminal fork"));
        assert_eq!(
            metadata.capabilities.as_deref(),
            Some("GUI \u{00b7} CLI  \u{2022}  Primary role: GUI application")
        );
        assert_eq!(
            inventory.package_name(&checkout.join("build/terminal/xfce4-terminal")),
            Some(("xfce4-terminal".into(), true))
        );
        assert!(
            inventory
                .lookup(&root.0.join("unrelated/xfce4-terminal"))
                .is_none()
        );
        assert!(
            inventory
                .lookup(&checkout.join("build/unrelated-program"))
                .is_none()
        );
    }

    #[test]
    fn exact_package_ownership_identifies_inner_binaries_not_indexed_as_launch_commands() {
        let mut chrome = program("Aur", "Explicit", json!({}));
        chrome["name"] = json!("google-chrome");
        chrome["desc"] = json!("The popular web browser by Google (Stable Channel)");
        chrome["binaries"] =
            json!([{"name": "google-chrome-stable", "path": "/usr/bin/google-chrome-stable"}]);
        let mut wrapper = program("Local", "Standalone", json!({"dev": true}));
        wrapper["name"] = json!("google-chrome-fast");
        wrapper["desc"] = json!("Local custom wrapper");
        let inventory = Inventory::parse(&inventory(vec![chrome, wrapper])).unwrap();
        assert!(
            inventory
                .inspect(Path::new("/opt/google/chrome/chrome"))
                .0
                .is_none()
        );
        let (label, metadata) = inventory.inspect_owner("google-chrome");
        assert!(label.unwrap().contains("Package-managed"));
        assert_eq!(
            metadata.description.as_deref(),
            Some("The popular web browser by Google (Stable Channel)")
        );
        assert_eq!(
            inventory.inspect_owner("google-chrome-fast"),
            (None, ProgsMetadata::default())
        );
        assert_eq!(
            inventory.inspect_owner("unrelated"),
            (None, ProgsMetadata::default())
        );
    }

    #[test]
    fn conflicting_package_owner_descriptions_are_not_borrowed_or_guessed() {
        let mut first = program("Pacman", "Explicit", json!({}));
        first["name"] = json!("tool");
        first["desc"] = json!("First description");
        let mut second = first.clone();
        second["desc"] = json!("Conflicting description");
        let inventory = Inventory::parse(&inventory(vec![first, second])).unwrap();
        assert_eq!(inventory.inspect_owner("tool").1, ProgsMetadata::default());
    }

    #[test]
    fn installation_rejects_ambiguous_records_and_unsupported_or_invalid_inventories() {
        let records = vec![
            program("Cargo", "Standalone", json!({})),
            program("Local", "Standalone", json!({"dev": true})),
        ];
        assert!(
            Inventory::parse(&inventory(records))
                .unwrap()
                .lookup(Path::new("/bin/tool"))
                .is_none()
        );
        assert!(Inventory::parse(br#"{"schema_version":5,"scan":{"apps":[]}}"#).is_none());
        assert!(Inventory::parse(b"invalid JSON").is_none());
        let root = Fixture::new();
        let path = root.0.join("oversized.json");
        File::create(&path)
            .unwrap()
            .set_len(MAX_INVENTORY_BYTES + 1)
            .unwrap();
        assert!(load(&path).is_none());
        let fifo = root.0.join("fifo");
        let fifo_name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        assert!(load(&fifo).is_none());
    }

    #[test]
    fn installation_inventory_cache_reuses_parsing_and_observes_atomic_replacement() {
        let root = Fixture::new();
        let path = root.0.join("scan.json");
        std::fs::write(
            &path,
            inventory(vec![program("Local", "Standalone", json!({"fork": true}))]),
        )
        .unwrap();
        let first = load(&path).unwrap();
        assert!(Arc::ptr_eq(&first, &load(&path).unwrap()));
        let replacement = root.0.join("replacement.json");
        std::fs::write(
            &replacement,
            inventory(vec![program("Local", "Standalone", json!({"dev": true}))]),
        )
        .unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let updated = load(&path).unwrap();
        assert!(!Arc::ptr_eq(&first, &updated));
        assert_eq!(
            updated.lookup(Path::new("/bin/tool")).as_deref(),
            Some("[DEV] DEV (User-managed)")
        );
        assert_eq!(
            first.lookup(Path::new("/bin/tool")).as_deref(),
            Some("[FRK] Fork (User-managed)")
        );
    }

    #[test]
    #[ignore = "read-only probe; requires APPLICATIONLAUNCHER_TEST_INSTALLATION_PATH and APPLICATIONLAUNCHER_TEST_INSTALLATION_LABEL"]
    fn live_progs_installation_matches_the_expected_classification() {
        let path = std::env::var("APPLICATIONLAUNCHER_TEST_INSTALLATION_PATH").unwrap();
        let expected = std::env::var("APPLICATIONLAUNCHER_TEST_INSTALLATION_LABEL").unwrap();
        let label = describe(Path::new(&path));
        let metadata = inspect(Path::new(&path)).1;
        eprintln!("{path}: {label}; {metadata:?}");
        assert_eq!(label, expected);
        for (key, actual) in [
            (
                "APPLICATIONLAUNCHER_TEST_CAPABILITIES",
                metadata.capabilities,
            ),
            ("APPLICATIONLAUNCHER_TEST_DESCRIPTION", metadata.description),
        ] {
            if let Ok(expected) = std::env::var(key) {
                assert_eq!(actual.as_deref(), Some(expected.as_str()));
            }
        }
    }
    #[test]
    fn schema_four_preserves_progs_development_classification_and_metadata() {
        let bytes = serde_json::to_vec(&json!({
            "schema_version": 4,
            "uv_dependency_revision": 1,
            "scan": {
                "apps": [{
                    "name": "progs",
                    "origin": "Local",
                    "install_role": "Standalone",
                    "state": {"dev": true},
                    "desc": "System programs manager",
                    "capabilities": {"has_gui": true, "has_cli": true},
                    "binaries": [{
                        "name": "progs",
                        "path": "/home/example/.local/bin/progs",
                        "target": "/home/example/Dev/progs/target/release/progs"
                    }]
                }],
                "provides_map": {},
                "_stats": [1, 0, 0, 0, 0]
            }
        }))
        .unwrap();
        let inventory = Inventory::parse(&bytes).expect("current Progs inventory must be accepted");
        for executable in [
            "/home/example/.local/bin/progs",
            "/home/example/Dev/progs/target/release/progs",
        ] {
            let (label, metadata) = inventory.inspect(Path::new(executable));
            assert_eq!(label.as_deref(), Some("[DEV] DEV (User-managed)"));
            assert_eq!(
                metadata.description.as_deref(),
                Some("System programs manager")
            );
            assert_eq!(
                metadata.capabilities.as_deref(),
                Some("GUI \u{00b7} CLI  \u{2022}  Primary role: GUI application")
            );
        }
    }

    #[test]
    fn current_progs_badges_preserve_package_and_tool_install_roles() {
        for (origin, role, package_badge, tool_badge) in [
            ("Pacman", "Explicit", "PEP", "PET"),
            ("Pacman", "Dependency", "PDP", "PDT"),
            ("Pacman", "Standalone", "PSP", "PPT"),
            ("Pacman", "ToolchainManaged", "PSP", "PPT"),
            ("Aur", "Explicit", "AEP", "AET"),
            ("Aur", "Dependency", "ADP", "ADT"),
            ("Aur", "Standalone", "ASP", "APT"),
            ("Aur", "ToolchainManaged", "ASP", "APT"),
            ("Uv", "Explicit", "UEP", "UET"),
            ("Uv", "Dependency", "UDP", "UDT"),
            ("Uv", "Standalone", "UUP", "UUT"),
            ("Uv", "ToolchainManaged", "UUP", "UUT"),
            ("Npm", "Explicit", "NEP", "NET"),
            ("Npm", "Dependency", "NDP", "NDT"),
            ("Npm", "Standalone", "NUP", "NUT"),
            ("Npm", "ToolchainManaged", "NUP", "NUT"),
            ("Cargo", "Explicit", "CEP", "CET"),
            ("Cargo", "Dependency", "CUP", "CUT"),
            ("Cargo", "Standalone", "CUP", "CUT"),
            ("Cargo", "ToolchainManaged", "CUP", "CUT"),
            ("Rustup", "ToolchainManaged", "RUT", "RUT"),
        ] {
            let record: Program = serde_json::from_value(program(origin, role, json!({}))).unwrap();
            assert!(
                record
                    .label()
                    .unwrap()
                    .starts_with(&format!("[{package_badge}]")),
                "{origin}/{role}: package"
            );
            assert!(
                record
                    .tool_label()
                    .unwrap()
                    .starts_with(&format!("[{tool_badge}]")),
                "{origin}/{role}: tool"
            );
        }
    }

    #[test]
    fn current_progs_special_state_badges_and_precedence_are_preserved() {
        for (origin, role, state, package_badge, tool_badge) in [
            ("Local", "Standalone", json!({"fork": true}), "FRK", "FRK"),
            (
                "Local",
                "Standalone",
                json!({"dev": true, "fork": true}),
                "DEV",
                "DEV",
            ),
            ("Local", "Standalone", json!({"cloned": true}), "CLO", "CLO"),
            ("Local", "Standalone", json!({"opt": true}), "OPT", "OPT"),
            ("Local", "Standalone", json!({"script": true}), "USC", "USC"),
            ("Local", "Standalone", json!({"binary": true}), "UBI", "UBI"),
            (
                "Local",
                "Standalone",
                json!({"broken": true, "fork": true}),
                "BRK",
                "BRK",
            ),
            ("Npm", "Explicit", json!({"fork": true}), "NEP", "NET"),
            ("Npm", "Explicit", json!({"broken": true}), "BRK", "NET"),
            (
                "Pacman",
                "Dependency",
                json!({"orphan": true}),
                "POP",
                "PDT",
            ),
            ("Aur", "Dependency", json!({"orphan": true}), "AOP", "ADT"),
            ("Aur", "Dependency", json!({"broken": true}), "ADP", "ADT"),
        ] {
            let record: Program = serde_json::from_value(program(origin, role, state)).unwrap();
            assert!(
                record
                    .label()
                    .unwrap()
                    .starts_with(&format!("[{package_badge}]")),
                "{origin}/{role}: package"
            );
            assert!(
                record
                    .tool_label()
                    .unwrap()
                    .starts_with(&format!("[{tool_badge}]")),
                "{origin}/{role}: tool"
            );
        }
    }
    #[test]
    fn launch_aliases_for_toolchain_applications_must_still_point_to_the_exact_entry() {
        let root = Fixture::new();
        let entry = root.0.join("cli.js");
        let other = root.0.join("other.js");
        std::fs::write(&entry, "not executed").unwrap();
        std::fs::write(&other, "not executed").unwrap();
        let alias = root.0.join("tool");
        std::os::unix::fs::symlink(&entry, &alias).unwrap();
        let mut record = program("Npm", "ToolchainManaged", json!({}));
        record["binaries"] = json!([{"name":"tool", "path":alias, "target":entry}]);
        let inventory = Inventory::parse(&inventory(vec![record])).unwrap();
        assert_eq!(inventory.launch_alias(&entry).as_ref(), Some(&alias));
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&other, &alias).unwrap();
        assert!(inventory.launch_alias(&entry).is_none());
    }
}
