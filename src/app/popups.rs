use super::*;
use crate::models::{InfoMetadataSection, InfoProgram, InfoUpdateTarget};
use std::sync::atomic::AtomicBool;

mod tmux;

const MAX_INFO_POPUPS: usize = 32;

fn terminal_update_window(
    window: &WindowInfo,
    node: &ProcessInfoNode,
    binary_is_current: Option<bool>,
) -> Option<String> {
    (binary_is_current == Some(false)
        && window.pid == Some(node.identity.pid)
        && window.class.to_ascii_lowercase().contains("xfce4-terminal")
        && node.executable.as_ref().is_some_and(|path| {
            path.file_name()
                .is_some_and(|name| name == "xfce4-terminal" || name == "xfce4-terminal (deleted)")
        }))
    .then(|| window.id.clone())
}

fn process_update_target(
    window: &WindowInfo,
    node: &ProcessInfoNode,
    binary_is_current: Option<bool>,
) -> Option<InfoUpdateTarget> {
    if let Some(window) = terminal_update_window(window, node, binary_is_current) {
        return Some(InfoUpdateTarget::TerminalWindow(window));
    }
    if window.pid == Some(node.identity.pid)
        && node.identity.pid > 1
        && node.identity.start_ticks > 0
        && node.executable.as_ref().is_some_and(|path| {
            path.file_name()
                .is_some_and(|name| name == "copyq" || name == "copyq (deleted)")
        })
    {
        return Some(InfoUpdateTarget::CopyQ(node.identity));
    }
    if binary_is_current == Some(false)
        && !window.id.is_empty()
        && window.pid.is_some_and(|pid| pid > 1)
        && matches!(
            window.class.to_ascii_lowercase().as_str(),
            "xfce4-terminal" | "org.xfce.terminal"
        )
        && node.identity.pid > 1
        && node.identity.start_ticks > 0
    {
        let application = node.executable.as_ref().and_then(|path| {
            match path.file_name()?.to_str()?.trim_end_matches(" (deleted)") {
                "fish" => Some(InfoProgram::Fish),
                "codex" | "codex-code-mode" => Some(InfoProgram::Codex),
                _ => None,
            }
        });
        if let Some(application) = application {
            return Some(InfoUpdateTarget::Program {
                window: window.id.clone(),
                process: node.identity,
                application,
            });
        }
    }
    let pane = window.tmux_pane.as_ref()?;
    (binary_is_current == Some(false)
        && window.class.to_ascii_lowercase().contains("xfce4-terminal")
        && node.identity.pid == pane.session.server_pid
        && node.identity.pid != pane.client_pid
        && node.identity.start_ticks > 0
        && Path::new(&pane.session.socket_path).is_absolute()
        && node.executable.as_ref().is_some_and(|path| {
            path.file_name()
                .is_some_and(|name| name == "tmux" || name == "tmux (deleted)")
        }))
    .then(|| InfoUpdateTarget::TmuxServer {
        window: window.id.clone(),
        server: applicationlauncher::tracker::TmuxServerIdentity {
            pid: node.identity.pid,
            start_ticks: node.identity.start_ticks,
            socket_path: pane.session.socket_path.clone(),
        },
    })
}

#[derive(Default)]
pub(super) struct InfoPopups {
    items: Vec<InfoPopup>,
    next_id: u64,
}

struct InfoPopup {
    id: egui::ViewportId,
    focus_requested: Arc<AtomicBool>,
    target: InfoTarget,
    initial_size: Option<[f32; 2]>,
    initial_size_ready: bool,
}

impl InfoPopup {
    fn window_initial_size(
        &mut self,
        ctx: &egui::Context,
        data: &InfoPopupData,
        details_ready: bool,
    ) -> [f32; 2] {
        // Fit at most twice: the placeholder, then the loaded details. Later updates
        // and user-expanded sections must not keep resizing the window.
        if self.initial_size.is_none() || (details_ready && !self.initial_size_ready) {
            self.initial_size = Some(crate::popups::info_popup_initial_size(
                ctx,
                self.id,
                data,
                [1200.0, 800.0],
                [520.0, 360.0],
            ));
            self.initial_size_ready = details_ready;
        }
        self.initial_size.unwrap()
    }
}

enum InfoTarget {
    Window(WindowInfoPopup),
    App(AppInfoPopup),
}

struct AppInfoPopup {
    app: AppInfo,
    metadata: Option<crate::windows::versions::ApplicationInfo>,
    receiver: Option<std::sync::mpsc::Receiver<crate::windows::versions::ApplicationInfoUpdate>>,
    cancelled: Arc<AtomicBool>,
}

impl AppInfoPopup {
    fn new(app: AppInfo) -> Self {
        Self {
            app,
            metadata: None,
            receiver: None,
            cancelled: Arc::default(),
        }
    }

    fn poll_metadata(&mut self, ctx: &egui::Context, viewport_id: egui::ViewportId) {
        if self.metadata.is_some() && self.receiver.is_none() {
            return;
        }
        let receiver = self.receiver.get_or_insert_with(|| {
            let exec = self.app.exec.clone();
            let desktop_file = self.app.desktop_file_path.clone();
            let cancelled = self.cancelled.clone();
            let ctx = ctx.clone();
            let (tx, rx) = std::sync::mpsc::sync_channel(3);
            // At most three updates; local details never wait for a package registry.
            applicationlauncher::spawn_worker!("app-info", move |worker| {
                use crate::windows::versions::ApplicationInfoUpdate;
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                applicationlauncher::observe! { worker.set_state("resolving-installed-application"); }
                let metadata = crate::windows::versions::inspect_application(&exec, &desktop_file);
                if cancelled.load(Ordering::Acquire)
                    || tx
                        .try_send(ApplicationInfoUpdate::Details(metadata.clone()))
                        .is_err()
                {
                    return;
                }
                ctx.request_repaint_of(egui::ViewportId::ROOT);
                ctx.request_repaint_of(viewport_id);
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                applicationlauncher::observe! { worker.set_state("checking-available-package-version"); }
                if let Some(version) = metadata.query_package_version()
                    && !cancelled.load(Ordering::Acquire)
                {
                    let _ = tx.try_send(ApplicationInfoUpdate::PackageVersion(version));
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    ctx.request_repaint_of(viewport_id);
                }
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                applicationlauncher::observe! { worker.set_state("checking-available-npm-version"); }
                if let Some(version) = metadata.query_npm_version()
                    && !cancelled.load(Ordering::Acquire)
                {
                    let _ = tx.try_send(ApplicationInfoUpdate::NpmVersion(version));
                }
                // Publish completion before waking the UI, including when no package applies.
                drop(tx);
                ctx.request_repaint_of(egui::ViewportId::ROOT);
                ctx.request_repaint_of(viewport_id);
            });
            rx
        });
        let mut finished = false;
        for _ in 0..4 {
            match receiver.try_recv() {
                Ok(crate::windows::versions::ApplicationInfoUpdate::Details(metadata)) => {
                    self.metadata = Some(metadata);
                }
                Ok(crate::windows::versions::ApplicationInfoUpdate::PackageVersion(version)) => {
                    if let Some(metadata) = self.metadata.as_mut() {
                        metadata.latest_package_version = Some(version);
                    }
                }
                Ok(crate::windows::versions::ApplicationInfoUpdate::NpmVersion(version)) => {
                    if let Some(metadata) = self.metadata.as_mut() {
                        metadata.latest_npm_version = Some(version);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.metadata.get_or_insert_with(Default::default);
                    finished = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
            }
        }
        if finished {
            if let Some(metadata) = self.metadata.as_mut()
                && metadata.package_target.is_some()
                && metadata.latest_package_version.is_none()
            {
                metadata.latest_package_version =
                    Some("Unavailable (package lookup ended without a result)".into());
            }
            if let Some(metadata) = self.metadata.as_mut()
                && metadata.npm_target.is_some()
                && metadata.latest_npm_version.is_none()
            {
                metadata.latest_npm_version =
                    Some("Unavailable (npm lookup ended without a result)".into());
            }
            self.receiver = None;
        }
    }
}

impl Drop for AppInfoPopup {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

#[derive(Default)]
struct InstalledApplicationFields<'a> {
    executable: Option<&'a Path>,
    chain: Option<&'a [PathBuf]>,
    version: Option<&'a str>,
    version_reason: Option<&'a str>,
    installed_metadata: Option<&'a program_metadata::interpreted::InstalledVersion>,
    build_id: Option<&'a str>,
    build_id_reason: Option<&'a str>,
    native: Option<bool>,
    package_target: bool,
    latest_package_version: Option<&'a crate::windows::versions::AvailablePackageVersion>,
    npm_target: Option<&'a crate::windows::versions::NpmTarget>,
    latest_npm_version: Option<&'a str>,
    uncompiled_changes: Option<&'a str>,
    installation: Option<&'a str>,
    package: Option<&'a str>,
    metadata: Option<&'a crate::windows::versions::ProgsMetadata>,
}

pub(super) fn info_metadata_label(role: &str, label: &str) -> String {
    if !matches!(role, "Application" | "Runtime") {
        return format!("{role} {label}");
    }
    let mut label = if label == "installation" {
        "Type"
    } else {
        label
    }
    .to_owned();
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    label
}

fn installation_type_value(value: &str) -> String {
    crate::popups::installation_badge(value)
        .unwrap_or(value)
        .to_owned()
}

fn metadata_issue_value(reason: &str) -> String {
    if ["Not embedded", "Not applicable", "Unavailable", "Unknown"]
        .iter()
        .any(|prefix| reason.starts_with(prefix))
    {
        reason.to_owned()
    } else {
        format!("Unavailable ({reason})")
    }
}

const VERSION_FIELD_LABEL: &str = "Version (ELF build ID)";
const LATEST_VERSION_FIELD_LABEL: &str = "Latest version on disk (ELF build ID)";

fn combine_version_build_id_rows(rows: &mut Vec<InfoPopupRow>, role: &str, latest: bool) {
    let (version_label, build_label, combined_label) = if latest {
        (
            "latest version (on disk)",
            "latest ELF build ID (on disk)",
            LATEST_VERSION_FIELD_LABEL,
        )
    } else {
        ("version", "ELF build ID", VERSION_FIELD_LABEL)
    };
    let version_label = info_metadata_label(role, version_label);
    let build_label = info_metadata_label(role, build_label);
    if !rows.iter().any(|row| row.label == version_label) {
        return;
    }
    let Some(index) = rows.iter().position(|row| row.label == build_label) else {
        return;
    };
    let build = rows.remove(index);
    let version = rows
        .iter_mut()
        .find(|row| row.label == version_label)
        .unwrap();
    version.label = info_metadata_label(role, combined_label);
    let build_value = if build.value.trim().is_empty() {
        "Unavailable"
    } else {
        &build.value
    };
    let same_failure = version.value == build_value
        && (version.value == "Loading..." || version.value.starts_with("Unavailable"));
    if !same_failure {
        version.value = format!("{} ({build_value})", version.value);
    }
    version.binary_is_current = build.binary_is_current;
}

fn installed_application_rows(
    role: &str,
    fields: InstalledApplicationFields<'_>,
    pending: bool,
) -> Vec<InfoPopupRow> {
    let chain = fields.chain.filter(|chain| !chain.is_empty()).map(|chain| {
        let text = chain
            .iter()
            .map(|path| path.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" -> ");
        if fields.executable.is_some() {
            text
        } else {
            format!("{text} (unresolved)")
        }
    });
    let package =
        latest_package_version_field(fields.latest_package_version, fields.package_target);
    let npm = latest_npm_version_field(fields.latest_npm_version, fields.npm_target);
    let owning_package = fields.package.map(|package| {
        match fields
            .metadata
            .and_then(|metadata| metadata.package_type.as_deref())
            .and_then(crate::popups::installation_badge)
        {
            Some(badge) => format!("{package} [{badge}]"),
            None => package.to_owned(),
        }
    });
    let root_package = fields.package.map(|_| {
        (
            "root package",
            Some(
                fields
                    .metadata
                    .and_then(|metadata| metadata.root_packages.clone())
                    .unwrap_or_else(|| {
                        "Unknown (package dependency/history metadata unavailable)".into()
                    }),
            ),
        )
    });
    let mut rows = [
        (
            "latest version (on disk)",
            fields
                .version
                .map(|version| match fields.installed_metadata {
                    Some(metadata) => format!(
                        "{version} ({}; installed metadata, not a running-version receipt)",
                        metadata.kind
                    ),
                    None => version.to_owned(),
                })
                .or_else(|| {
                    fields
                        .version_reason
                        .map(|reason| format!("Unavailable ({reason})"))
                }),
        ),
        (
            "latest ELF build ID (on disk)",
            if fields.native == Some(false) {
                Some("Not applicable (interpreted application entry point)".into())
            } else {
                fields
                    .build_id
                    .map(str::to_owned)
                    .or_else(|| fields.build_id_reason.map(metadata_issue_value))
            },
        ),
        (
            "uncompiled changes",
            fields
                .uncompiled_changes
                .map(program_metadata::build_status::display_uncompiled_changes)
                .map(str::to_owned),
        ),
        (
            "installation",
            fields.installation.map(installation_type_value),
        ),
        ("package", owning_package),
    ]
    .into_iter()
    .chain(package.map(|(label, value)| (label, Some(value))))
    .chain(npm.map(|(label, value)| (label, Some(value))))
    .chain(root_package)
    .chain(
        fields
            .metadata
            .and_then(|metadata| metadata.source_checkout.as_ref())
            .map(|root| ("source checkout", Some(root.display().to_string()))),
    )
    .chain([
        ("Launch chain", chain),
        (
            "description",
            fields
                .metadata
                .and_then(|metadata| metadata.description.clone()),
        ),
        (
            "capabilities",
            fields
                .metadata
                .and_then(|metadata| metadata.capabilities.clone()),
        ),
    ])
    .map(|(label, value)| InfoPopupRow {
        label: info_metadata_label(role, label),
        scope: InfoFieldScope::Installation,
        value: value.unwrap_or_else(|| {
            if pending {
                "Loading..."
            } else if label == "package" {
                "-"
            } else {
                "Unavailable"
            }
            .into()
        }),
        searched: false,
        separator_before: false,
        binary_is_current: None,
    })
    .collect();
    combine_version_build_id_rows(&mut rows, role, true);
    rows
}

fn desktop_file_value(path: Option<&Path>, exists: Option<bool>, pending: bool) -> String {
    let Some(path) = path.filter(|path| !path.as_os_str().is_empty()) else {
        return "None".into();
    };
    let path = path.to_string_lossy();
    match exists {
        Some(true) => path.into_owned(),
        Some(false) => format!("{path} (missing)"),
        None if pending => format!("{path} (checking...)"),
        None => format!("{path} (existence unknown)"),
    }
}

fn desktop_entry_rows(
    entry: Option<&AppInfo>,
    exists: Option<bool>,
    pending: bool,
    fallback_icon: Option<&Path>,
) -> Vec<InfoPopupRow> {
    let pending_entry = entry.is_none() && pending;
    let entry = entry.filter(|entry| !entry.desktop_file_path.as_os_str().is_empty());
    let fallback_icon = fallback_icon.filter(|_| entry.is_some() || pending_entry);
    [
        (
            "Desktop file",
            desktop_file_value(
                entry.map(|entry| entry.desktop_file_path.as_path()),
                exists,
                pending,
            ),
            true,
        ),
        (
            "Icon path",
            entry
                .and_then(|entry| entry.icon_path.as_deref())
                .or(fallback_icon)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".into()),
            false,
        ),
        (
            "Name",
            entry
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| "-".into()),
            true,
        ),
        (
            "Comment",
            entry
                .and_then(|entry| entry.comment.clone())
                .unwrap_or_else(|| "-".into()),
            true,
        ),
    ]
    .into_iter()
    .map(|(label, value, searched)| InfoPopupRow {
        label: label.into(),
        scope: InfoFieldScope::Installation,
        value: if pending_entry && label != "Icon path" {
            "Loading...".into()
        } else {
            value
        },
        searched,
        separator_before: false,
        binary_is_current: None,
    })
    .collect()
}

fn append_desktop_entry_rows(rows: &mut Vec<InfoPopupRow>, desktop_rows: Vec<InfoPopupRow>) {
    let index = rows
        .iter()
        .position(|row| matches!(row.label.as_str(), "Description" | "Capabilities"))
        .unwrap_or(rows.len());
    rows.splice(index..index, desktop_rows);
}

fn application_info_rows(state: &AppInfoPopup) -> Vec<InfoPopupRow> {
    let details = state.metadata.as_ref();
    let mut rows = installed_application_rows(
        "Application",
        InstalledApplicationFields {
            executable: details.and_then(|info| info.executable.as_deref()),
            chain: details.map(|info| info.chain.as_slice()),
            version: details.and_then(|info| info.version.as_deref()),
            version_reason: details.and_then(|info| info.version_reason.as_deref()),
            installed_metadata: details.and_then(|info| info.installed_metadata.as_ref()),
            build_id: details.and_then(|info| info.build_id.as_deref()),
            build_id_reason: details.and_then(|info| info.build_id_reason.as_deref()),
            native: details.and_then(|info| info.native),
            package_target: details.is_some_and(|info| info.package_target.is_some()),
            latest_package_version: details.and_then(|info| info.latest_package_version.as_ref()),
            npm_target: details.and_then(|info| info.npm_target.as_ref()),
            latest_npm_version: details.and_then(|info| info.latest_npm_version.as_deref()),
            uncompiled_changes: details.and_then(|info| info.uncompiled_changes.as_deref()),
            installation: details.and_then(|info| info.installation.as_deref()),
            package: details.and_then(|info| info.package.as_deref()),
            metadata: details.map(|info| &info.metadata),
        },
        details.is_none(),
    );
    append_desktop_entry_rows(
        &mut rows,
        desktop_entry_rows(
            Some(&state.app),
            details.and_then(|info| info.desktop_exists),
            details.is_none(),
            None,
        ),
    );
    rows
}

struct WindowInfoPopup {
    window: WindowInfo,
    lookup: Option<WindowProcessTreeLookup>,
    versions: Option<ProcessVersionCache>,
    tmux: Option<crate::windows::tmux_info::Lookup>,
}

impl InfoTarget {
    fn same_target(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Window(left), Self::Window(right)) => left.window.id == right.window.id,
            (Self::App(left), Self::App(right)) => {
                left.app.desktop_file_path == right.app.desktop_file_path
            }
            _ => false,
        }
    }
}

impl InfoPopups {
    pub(super) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(super) fn request_repaint(&self, ctx: &egui::Context) {
        for popup in &self.items {
            ctx.request_repaint_of(popup.id);
        }
    }

    pub(super) fn cancel_focus_requests(&self) {
        for popup in &self.items {
            popup.focus_requested.store(false, Ordering::Relaxed);
        }
    }

    fn open(&mut self, target: InfoTarget) -> Result<egui::ViewportId, &'static str> {
        if let Some(index) = self
            .items
            .iter()
            .position(|popup| popup.target.same_target(&target))
        {
            let popup = self.items.remove(index);
            popup.focus_requested.store(true, Ordering::Relaxed);
            let id = popup.id;
            self.items.push(popup);
            return Ok(id);
        }
        if self.items.len() >= MAX_INFO_POPUPS {
            return Err("Close an information window before opening another (limit: 32).");
        }
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("Information window IDs exhausted.")?;
        let id = egui::ViewportId::from_hash_of(("launcher-info-popup", self.next_id));
        self.items.push(InfoPopup {
            id,
            focus_requested: Arc::new(AtomicBool::new(true)),
            target,
            initial_size: None,
            initial_size_ready: false,
        });
        Ok(id)
    }

    fn close(&mut self, id: egui::ViewportId) -> bool {
        let before = self.items.len();
        self.items.retain(|popup| popup.id != id);
        self.items.len() != before
    }

    pub(super) fn close_last(&mut self) {
        self.items.pop();
    }
}

fn latest_npm_version_field(
    latest_version: Option<&str>,
    target: Option<&crate::windows::versions::NpmTarget>,
) -> Option<(&'static str, String)> {
    target.map(|target| {
        (
            "latest package version (npm)",
            latest_version
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Checking npm registry for {}...", target.name)),
        )
    })
}

fn latest_package_version_field(
    latest_package_version: Option<&crate::windows::versions::AvailablePackageVersion>,
    has_package_target: bool,
) -> Option<(&'static str, String)> {
    has_package_target.then(|| {
        (
            latest_package_version.map_or("latest version (Pacman/AUR)", |version| {
                version.field_label()
            }),
            latest_package_version
                .map(|version| version.display())
                .unwrap_or_else(|| "Checking available package version...".into()),
        )
    })
}

pub(super) fn process_version_rows(
    role: &str,
    version: Option<&crate::windows::versions::ProcessVersion>,
    pending: bool,
) -> Vec<InfoPopupRow> {
    let mut rows: Vec<_> = [
        (
            "version",
            version.and_then(|version| version.version.as_deref()),
        ),
        (
            "ELF build ID",
            version.and_then(|version| version.build_id.as_deref()),
        ),
    ]
    .into_iter()
    .map(|(label, value)| InfoPopupRow {
        label: info_metadata_label(role, label),
        scope: InfoFieldScope::RunningInstance,
        value: value
            .map(|value| {
                if label == "version"
                    && version
                        .and_then(|version| version.application_installed_metadata.as_ref())
                        .is_some_and(|metadata| metadata.kind == "Electron package")
                {
                    format!("{value} (running native engine; loaded application version unknown)")
                } else {
                    value.to_owned()
                }
            })
            .unwrap_or_else(|| {
                if let Some(reason) = version.and_then(|version| version.running_unavailable) {
                    format!("Unavailable ({reason})")
                } else if label == "version"
                    && let Some(reason) =
                        version.and_then(|version| version.version_reason.as_deref())
                {
                    format!("Unavailable ({reason})")
                } else if label == "ELF build ID"
                    && let Some(reason) =
                        version.and_then(|version| version.build_id_reason.as_deref())
                {
                    metadata_issue_value(reason)
                } else {
                    if pending { "Loading..." } else { "Unavailable" }.into()
                }
            }),
        searched: false,
        separator_before: false,
        binary_is_current: if label == "ELF build ID"
            && value.is_some_and(|value| !value.trim().is_empty())
        {
            version
                .filter(|version| version.running_unavailable.is_none())
                .and_then(|version| version.running_latest_binary)
        } else {
            None
        },
    })
    .collect();
    combine_version_build_id_rows(&mut rows, role, false);
    let launch = version.and_then(|version| version.launch.as_ref());
    if let Some(candidate) = version.and_then(|version| version.on_disk_candidate.as_ref()) {
        rows.push(InfoPopupRow {
            label: info_metadata_label(role, "on-disk candidate"),
            scope: InfoFieldScope::Installation,
            value: format!(
                "{} — {}",
                candidate.display(),
                version
                    .and_then(|version| version.on_disk_candidate_reason.as_deref())
                    .unwrap_or("absolute argv[0]; running executable unverified")
            ),
            searched: false,
            separator_before: false,
            binary_is_current: None,
        });
    }
    rows.extend(installed_application_rows(
        role,
        InstalledApplicationFields {
            executable: launch.and_then(|launch| launch.executable.as_deref()),
            chain: launch.map(|launch| launch.chain.as_slice()),
            version: version.and_then(|version| version.latest_version.as_deref()),
            version_reason: version.and_then(|version| version.latest_version_reason.as_deref()),
            installed_metadata: version
                .and_then(|version| version.application_installed_metadata.as_ref()),
            build_id: version.and_then(|version| version.latest_build_id.as_deref()),
            build_id_reason: version.and_then(|version| version.latest_build_id_reason.as_deref()),
            package_target: version.is_some_and(|version| version.package_target.is_some()),
            latest_package_version: version
                .and_then(|version| version.latest_package_version.as_ref()),
            npm_target: version.and_then(|version| version.npm_target.as_ref()),
            latest_npm_version: version.and_then(|version| version.latest_npm_version.as_deref()),
            uncompiled_changes: version.and_then(|version| version.uncompiled_changes.as_deref()),
            installation: version.and_then(|version| version.installation.as_deref()),
            package: version.and_then(|version| version.package.as_deref()),
            metadata: version.map(|version| &version.progs_metadata),
            ..Default::default()
        },
        pending,
    ));
    rows
}

pub(super) fn application_version_rows(
    role: &str,
    version: Option<&crate::windows::versions::ProcessVersion>,
    pending: bool,
) -> Vec<InfoPopupRow> {
    let Some(_) = version.and_then(|version| version.runtime_name) else {
        return process_version_rows(role, version, pending);
    };
    let mut rows = Vec::new();
    let mut installed_rows = Vec::new();
    let mut package_rows = Vec::new();
    let mut launch_rows = Vec::new();
    let row = |label: &str, value: String| InfoPopupRow {
        label: info_metadata_label(role, label),
        scope: InfoFieldScope::RunningInstance,
        value,
        searched: false,
        separator_before: false,
        binary_is_current: None,
    };
    if let Some(application) = version.and_then(|version| version.application.as_ref()) {
        let build = version.and_then(|version| version.application_build.as_ref());
        let package_field = latest_package_version_field(
            version.and_then(|version| version.application_latest_package_version.as_ref()),
            version.is_some_and(|version| version.application_package_target.is_some()),
        );
        rows.push(row(
            "running version",
            build
                .and_then(|build| build.running_version.clone())
                .unwrap_or_else(|| {
                    "Unknown (installed metadata does not prove the loaded version)".to_owned()
                }),
        ));
        if application.package == "@earendil-works/pi-coding-agent" {
            rows.push(row(
                "running entry point",
                build
                    .and_then(|build| build.running_entry.as_ref())
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "Unknown".into()),
            ));
        }
        launch_rows.push(row(
            "Launch chain",
            version
                .and_then(|version| version.application_launch.as_ref())
                .map(|launch| {
                    launch
                        .chain
                        .iter()
                        .map(|path| path.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" -> ")
                })
                .filter(|chain| !chain.is_empty())
                .unwrap_or_else(|| "Unavailable".into()),
        ));
        installed_rows.push(row(
            "installed version",
            application.installed_version.clone(),
        ));
        package_rows.push(row("package", application.package.clone()));
        if let Some((label, value)) = package_field {
            package_rows.push(row(label, value));
        }
        if let Some((label, value)) = latest_npm_version_field(
            version.and_then(|version| version.application_latest_npm_version.as_deref()),
            version.and_then(|version| version.application_npm_target.as_ref()),
        ) {
            package_rows.push(row(label, value));
        }
        if application.package == "@earendil-works/pi-coding-agent" {
            append_pi_build_rows(role, build, &mut rows, &mut installed_rows);
        }
        launch_rows.push(row(
            "installed package manifest",
            application.manifest.display().to_string(),
        ));
    }
    if let Some(application) = version.and_then(|version| version.script_application.as_ref()) {
        let installed = version.and_then(|version| version.application_installed_metadata.as_ref());
        installed_rows.push(row(
            "installed version",
            installed
                .map(|metadata| metadata.version.clone())
                .unwrap_or_else(|| {
                    "Unknown (no exact installed application version metadata)".into()
                }),
        ));
        if let Some(installed) = installed {
            launch_rows.push(row(
                "installed metadata",
                format!(
                    "{} — {}",
                    installed.metadata_path.display(),
                    installed.identification
                ),
            ));
        }
        let package_field = latest_package_version_field(
            version.and_then(|version| version.application_latest_package_version.as_ref()),
            version.is_some_and(|version| version.application_package_target.is_some()),
        );
        for (label, value) in [
            ("kind", application.kind.to_owned()),
            ("launch target", application.target.clone()),
            (
                "running version",
                "Unknown (no application build identity published)".into(),
            ),
            (
                if application.kind.starts_with("Python") {
                    "source changes since start"
                } else {
                    "build status"
                },
                if application.kind.starts_with("Python") {
                    "Unknown (no startup source snapshot published)".into()
                } else {
                    "Unknown (no application build identity published)".into()
                },
            ),
        ] {
            rows.push(row(label, value));
        }
        launch_rows.push(row(
            "Launch chain",
            version
                .and_then(|version| version.application_launch.as_ref())
                .map(|launch| {
                    launch
                        .chain
                        .iter()
                        .map(|path| path.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" -> ")
                })
                .filter(|chain| !chain.is_empty())
                .unwrap_or_else(|| "Unavailable".into()),
        ));
        package_rows.push(row(
            "package",
            version
                .and_then(|version| version.application_package_target.as_ref())
                .map(|package| package.name.clone())
                .unwrap_or_else(|| "-".into()),
        ));
        if let Some((label, value)) = package_field {
            package_rows.push(row(label, value));
        }
    }
    if let Some(installation) =
        version.and_then(|version| version.application_installation.as_ref())
    {
        installed_rows.push(row("installation", installation_type_value(installation)));
    }
    if let Some(root) =
        version.and_then(|version| version.application_progs_metadata.source_checkout.as_ref())
    {
        installed_rows.push(row("source directory", root.display().to_string()));
    }
    installed_rows.extend(package_rows);
    installed_rows.extend(launch_rows);
    installed_rows.push(row(
        "description",
        version
            .and_then(|version| version.application_progs_metadata.description.clone())
            .unwrap_or_else(|| "Unavailable".into()),
    ));
    installed_rows.push(row(
        "capabilities",
        version
            .and_then(|version| version.application_progs_metadata.capabilities.clone())
            .unwrap_or_else(|| "Unavailable".into()),
    ));
    rows.extend(installed_rows.into_iter().map(|mut row| {
        row.scope = InfoFieldScope::Installation;
        row
    }));
    rows
}

pub(super) fn hosted_metadata_sections(
    ctx: &egui::Context,
    id: egui::Id,
    node: &ProcessInfoNode,
    version: Option<&crate::windows::versions::ProcessVersion>,
) -> Vec<InfoMetadataSection> {
    let runtime = version
        .and_then(|version| version.runtime_name)
        .or_else(|| {
            let name = node
                .executable
                .as_ref()?
                .file_name()?
                .to_str()?
                .trim_end_matches(" (deleted)");
            crate::windows::versions::runtime_name(name)
        });
    let Some(runtime) = runtime else {
        return Vec::new();
    };
    let name = version
        .and_then(|version| version.application.as_ref())
        .map(|app| {
            if app.package == "@earendil-works/pi-coding-agent" {
                "Pi"
            } else {
                app.package.rsplit('/').next().unwrap_or(&app.package)
            }
        })
        .or_else(|| {
            version
                .and_then(|version| version.script_application.as_ref())
                .map(|app| app.name.as_str())
        })
        .unwrap_or(if version.is_none() {
            "Loading..."
        } else {
            "Unknown"
        });
    let app_id = id.with("hosted-application");
    let runtime_id = id.with("runtime");
    let application_rows = if process_info_section_is_open(ctx, app_id, true) && version.is_some() {
        application_version_rows("Application", version, false)
    } else {
        Vec::new()
    };
    let runtime_rows = if process_info_section_is_open(ctx, runtime_id, true) {
        let mut rows = vec![InfoPopupRow {
            label: "Executable path".into(),
            scope: InfoFieldScope::RunningInstance,
            value: node
                .executable
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "Unavailable".into()),
            searched: false,
            separator_before: false,
            binary_is_current: None,
        }];
        rows.extend(process_version_rows("Runtime", version, version.is_none()));
        rows
    } else {
        Vec::new()
    };
    vec![
        InfoMetadataSection {
            id: app_id,
            header: format!("Application: {name}").into(),
            binary_is_current: version.and_then(|version| version.process_build_is_current()),
            default_open: true,
            rows: application_rows,
        },
        InfoMetadataSection {
            id: runtime_id,
            header: format!("Runtime: {runtime}").into(),
            binary_is_current: version
                .filter(|version| version.running_unavailable.is_none())
                .and_then(|version| version.running_latest_binary),
            default_open: true,
            rows: runtime_rows,
        },
    ]
}

fn append_pi_build_rows(
    role: &str,
    build: Option<&crate::windows::versions::PiBuildEvidence>,
    running_rows: &mut Vec<InfoPopupRow>,
    installed_rows: &mut Vec<InfoPopupRow>,
) {
    let empty = crate::windows::versions::PiBuildEvidence::default();
    let build = build.unwrap_or(&empty);
    let compare = |running: Option<&str>, latest: Option<&str>| {
        if build.status == crate::windows::versions::PiBuildStatus::Unknown {
            return None;
        }
        let valid =
            |hash: &&str| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit());
        let running = running.filter(valid)?;
        let latest = latest.filter(valid)?;
        Some(running.eq_ignore_ascii_case(latest))
    };
    let row = |scope, (label, value): (&str, Option<String>)| {
        let binary_is_current = match label {
            "running code fingerprint" => {
                compare(build.running_sdk.as_deref(), build.latest_sdk.as_deref())
            }
            "running extensions fingerprint" => compare(
                build.running_extensions.as_deref(),
                build.latest_extensions.as_deref(),
            ),
            _ => None,
        };
        InfoPopupRow {
            label: info_metadata_label(role, label),
            scope,
            value: value.unwrap_or_else(|| "Unknown".into()),
            searched: false,
            separator_before: false,
            binary_is_current,
        }
    };
    running_rows.extend(
        [
            ("running code fingerprint", build.running_sdk.clone()),
            (
                "running extensions fingerprint",
                build.running_extensions.clone(),
            ),
            (
                "loaded extension roots",
                build.extension_count.map(|count| count.to_string()),
            ),
            (
                "startup build receipt",
                build
                    .record_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
            ),
        ]
        .into_iter()
        .map(|field| row(InfoFieldScope::RunningInstance, field)),
    );
    installed_rows.extend(
        [
            (
                "latest installed code fingerprint",
                build.latest_sdk.clone(),
            ),
            (
                "latest extensions fingerprint",
                build.latest_extensions.clone(),
            ),
            (
                "installed build manifest",
                build
                    .manifest_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
            ),
        ]
        .into_iter()
        .map(|field| row(InfoFieldScope::Installation, field)),
    );
}

pub(super) fn window_info_rows(
    window: &WindowInfo,
    window_version_rows: Vec<InfoPopupRow>,
) -> Vec<InfoPopupRow> {
    let app_key = window_application_key(window);
    let class_is_searched = !window.class.eq_ignore_ascii_case(&app_key);
    let row = |label: &str, value: String, searched: bool| InfoPopupRow {
        label: label.to_string(),
        scope: InfoFieldScope::Window,
        value,
        searched,
        separator_before: false,
        binary_is_current: None,
    };

    let mut rows = vec![
        row("Applicationlauncher title", window.title.clone(), true),
        row("Raw window title", window.raw_title.clone(), false),
        row("Application key", app_key, true),
        row("Class", window.class.clone(), class_is_searched),
        row("Window ID", window.id.clone(), false),
        row(
            "Geometry",
            window
                .geometry
                .map(|(x, y, width, height)| {
                    format!("x={x}, y={y}, width={width}, height={height}")
                })
                .unwrap_or_else(|| "Unavailable".to_string()),
            false,
        ),
        row(
            "Minimized",
            window
                .minimized
                .map(|value| value.to_string())
                .unwrap_or_else(|| "Unavailable".to_string()),
            false,
        ),
        row(
            "Last activated",
            format_activation_time(window.last_activated_at_ms),
            false,
        ),
        row(
            "Activation sequence",
            window.activation_sequence.to_string(),
            false,
        ),
    ];
    rows.extend(window_version_rows);
    rows
}

pub(super) fn process_info_rows(
    window: &WindowInfo,
    node: &ProcessInfoNode,
    version_rows: Vec<InfoPopupRow>,
) -> Vec<InfoPopupRow> {
    let active = window.active_process.is_some()
        && window
            .process_chain
            .first()
            .is_some_and(|entry| entry.pid == node.identity.pid);
    let row = |label: &str, value: String, searched: bool| InfoPopupRow {
        label: label.to_string(),
        scope: InfoFieldScope::RunningInstance,
        value,
        searched,
        separator_before: false,
        binary_is_current: None,
    };
    let mut rows = vec![
        row(
            "Process name",
            node.name.clone(),
            active && window.active_process.as_ref() == Some(&node.name),
        ),
        row("PID", node.identity.pid.to_string(), false),
        row("Parent PID", node.parent_pid.to_string(), false),
        row(
            "Process start (ticks since boot)",
            node.identity.start_ticks.to_string(),
            false,
        ),
        row(
            "Working directory",
            node.cwd.as_deref().map(display_path).unwrap_or_else(|| {
                node.cwd_error
                    .map(metadata_issue_value)
                    .unwrap_or_else(|| "Unavailable".into())
            }),
            active && node.cwd == window.cwd_path,
        ),
        row(
            "Command line",
            node.command_line
                .clone()
                .unwrap_or_else(|| "Unavailable".into()),
            active && node.command_line == window.command_line,
        ),
        row(
            "Executable path",
            node.executable
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| {
                    node.executable_error
                        .map(metadata_issue_value)
                        .unwrap_or_else(|| "Unavailable".into())
                }),
            false,
        ),
    ];
    if let Some(script) = &node.script_name {
        rows.insert(1, row("Script name", script.clone(), false));
    }
    rows.extend(tmux::process_rows(window, node));
    rows.extend(version_rows);
    rows
}

fn unique_desktop_candidate<'a>(
    mut entries: impl Iterator<Item = &'a AppInfo>,
) -> Option<&'a AppInfo> {
    let entry = entries.next()?;
    entries.next().is_none().then_some(entry)
}

fn window_desktop_candidate<'a>(
    apps: &'a [AppInfo],
    window: &WindowInfo,
) -> Option<(&'a AppInfo, bool)> {
    if let Some(name) = window.desktop_file_name.as_deref() {
        let mut entries = apps.iter().filter(|entry| {
            entry.desktop_file_path.to_str() == Some(name)
                || entry
                    .desktop_file_path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    == Some(name.trim_end_matches(".desktop"))
        });
        if let Some(entry) = entries.next() {
            // A declared basename can match multiple XDG entries. Do not
            // silently select whichever happened to be enumerated first.
            return entries.next().is_none().then_some((entry, true));
        }
    }
    let mut keys = Vec::new();
    let class = window.class.trim();
    if !class.is_empty() {
        keys.push(normalize_app_match_key(class));
        if let Some(segment) = class.rsplit('.').next() {
            keys.push(normalize_app_match_key(segment));
        }
    }
    if let Some(name) = window
        .exe_path
        .as_ref()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
    {
        keys.push(normalize_app_match_key(name));
    }
    if let Some(name) = &window.active_process {
        keys.push(normalize_app_match_key(name));
    }
    keys.retain(|key| !key.is_empty());
    let mut best = None;
    let mut ambiguous = false;
    for entry in apps {
        let Some(score) =
            best_app_match_score(&keys, entry).map(|score| (score, entry.is_settings_module))
        else {
            continue;
        };
        match best {
            None => best = Some((entry, score)),
            Some((_, previous)) if score < previous => {
                best = Some((entry, score));
                ambiguous = false;
            }
            Some((_, previous)) if score == previous => ambiguous = true,
            _ => {}
        }
    }
    best.filter(|_| !ambiguous).map(|(entry, _)| (entry, false))
}

fn process_desktop_candidate<'a>(
    apps: &'a [AppInfo],
    window: &WindowInfo,
    node: &ProcessInfoNode,
) -> Option<(&'a AppInfo, bool)> {
    let owner_entry = window_desktop_candidate(apps, window);
    if window.pid == Some(node.identity.pid) {
        return owner_entry;
    }
    // Only a candidate crosses from the window owner to a child. The worker
    // must resolve it to that child's independently pinned executable, and the
    // window-declared qualification must never cross the process boundary.
    owner_entry.map(|(entry, _)| (entry, false)).or_else(|| {
        let key = normalize_app_match_key(&node.name);
        unique_desktop_candidate(apps.iter().filter(|entry| {
            command_basename(&entry.exec).is_some_and(|name| normalize_app_match_key(&name) == key)
        }))
        .map(|entry| (entry, false))
    })
}

impl App {
    fn process_desktop_rows(
        launch: Option<&crate::windows::versions::ApplicationLaunch>,
        pending: bool,
    ) -> Vec<InfoPopupRow> {
        let entry = launch.and_then(|launch| launch.desktop_entry.as_ref());
        desktop_entry_rows(
            entry,
            launch.and_then(|launch| launch.desktop_exists),
            pending,
            None,
        )
    }

    fn open_info_popup(&mut self, ctx: &egui::Context, target: InfoTarget) {
        let reopening = self
            .info_popups
            .items
            .iter()
            .any(|popup| popup.target.same_target(&target));
        match self.info_popups.open(target) {
            Ok(id) => {
                // Wake an existing minimized viewport; a new viewport also focuses on its first frame.
                if !ctx.embed_viewports() {
                    ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
                    if reopening {
                        crate::popups::foreground::request(id);
                    }
                    ctx.request_repaint_of(id);
                }
                ctx.request_repaint();
            }
            Err(message) => self.error_message = Some(message.to_owned()),
        }
    }

    pub(super) fn open_window_info_popup(&mut self, ctx: &egui::Context, window: WindowInfo) {
        self.open_info_popup(
            ctx,
            InfoTarget::Window(WindowInfoPopup {
                window,
                lookup: None,
                versions: None,
                tmux: None,
            }),
        );
    }

    pub(super) fn open_app_info_popup(&mut self, ctx: &egui::Context, app: AppInfo) {
        self.open_info_popup(ctx, InfoTarget::App(AppInfoPopup::new(app)));
    }

    pub(super) fn process_popup_events(&mut self) {
        let mut restore_launcher_focus = false;
        while let Ok(event) = self.popup_event_receiver.try_recv() {
            match event {
                PopupEvent::CloseSettings => {
                    self.close_settings_menu();
                    restore_launcher_focus = true;
                }
                PopupEvent::CloseInfo(id) => {
                    restore_launcher_focus |= self.info_popups.close(id);
                }
                PopupEvent::CloseHistory => {
                    self.close_history_popup();
                    restore_launcher_focus = true;
                }
                PopupEvent::UpdateTerminalWindow { window, info } => {
                    let error = super::terminal_maintenance::open_direct(
                        &self.terminal_maintenance,
                        window.clone(),
                        info,
                    )
                    .err();
                    crate::popups::bind_terminal_update_status(
                        &self.repaint_ctx,
                        info,
                        window,
                        self.terminal_maintenance.clone(),
                        error,
                    );
                    self.repaint_ctx.request_repaint_of(info);
                }
                PopupEvent::UpdateTerminalWindows { window } => {
                    super::terminal_maintenance::open(&self.terminal_maintenance, window);
                }
            }
        }
        if restore_launcher_focus && self.info_popups.is_empty() {
            self.start_time = Instant::now();
            self.repaint_ctx
                .send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
        }
    }

    pub(super) fn show_settings_native_viewport(&mut self, ctx: &egui::Context) {
        let Some(shared_state) = self.settings_popup_state.clone() else {
            self.settings_popup_state =
                Some(Arc::new(std::sync::Mutex::new(SettingsWindowState::new(
                    self.launcher_settings_snapshot(),
                    self.idle_codex_action.clone(),
                    self.terminal_maintenance.clone(),
                ))));
            ctx.request_repaint();
            return;
        };

        let state_snapshot = shared_state.lock().ok().map(|mut state| {
            state.flush_pending_save();
            (
                state.settings,
                state.revision,
                (state.scale_anchor / state.settings.ui_scale).clamp(0.2, 5.0),
                state.save_deadline,
            )
        });
        let Some((settings, revision, viewport_scale_factor, save_deadline)) = state_snapshot
        else {
            return;
        };
        if let Some(deadline) = save_deadline {
            ctx.request_repaint_after(
                deadline.saturating_duration_since(std::time::Instant::now()),
            );
        }
        if revision != self.settings_popup_applied_revision {
            self.apply_launcher_settings_snapshot(settings, ctx);
            self.settings_popup_applied_revision = revision;
        }

        let builder = egui::ViewportBuilder::default()
            .with_title("Launcher Settings")
            .with_inner_size([
                (SETTINGS_VIEWPORT_SIZE[0] + 20.0) * viewport_scale_factor,
                (SETTINGS_VIEWPORT_SIZE[1] + 80.0) * viewport_scale_factor,
            ])
            .with_min_inner_size([
                SETTINGS_VIEWPORT_MIN_SIZE[0] * viewport_scale_factor,
                SETTINGS_VIEWPORT_MIN_SIZE[1] * viewport_scale_factor,
            ])
            .with_resizable(true)
            .with_always_on_top();
        let event_sender = self.popup_event_sender.clone();
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("launcher_settings_popup"),
            builder,
            move |ctx, _class| {
                let modal_open = ctx.memory(|memory| memory.top_modal_layer().is_some());
                let close_requested = ctx.input(|input| {
                    input.viewport().close_requested()
                        || (!modal_open && input.key_pressed(egui::Key::Escape))
                        || input.key_pressed(egui::Key::F10)
                });
                if close_requested {
                    let _ = event_sender.send(PopupEvent::CloseSettings);
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }

                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 248))
                            .inner_margin(egui::Margin::same(12)),
                    )
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let Ok(mut state) = shared_state.lock() else {
                                    return;
                                };
                                let previous_revision = state.revision;
                                let close_requested =
                                    render_deferred_settings_panel(ui, &mut state);
                                if close_requested {
                                    state.flush_pending_save_now();
                                    let _ = event_sender.send(PopupEvent::CloseSettings);
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                }
                                if state.revision != previous_revision {
                                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                                }
                                if let Some(deadline) = state.save_deadline {
                                    ctx.request_repaint_after(
                                        deadline
                                            .saturating_duration_since(std::time::Instant::now()),
                                    );
                                }
                            });
                    });
            },
        );
    }

    fn window_info_popup_data(
        &self,
        ctx: &egui::Context,
        viewport_id: egui::ViewportId,
        popup: &WindowInfoPopup,
    ) -> (InfoPopupData, bool) {
        let window_info = &popup.window;
        let desktop_file_path = window_info
            .desktop_file_name
            .as_ref()
            .filter(|path| Path::new(path).is_absolute())
            .cloned()
            .unwrap_or_else(|| "Unavailable".into());
        let snapshot = popup
            .lookup
            .as_ref()
            .filter(|lookup| lookup.target == WindowProcessTreeTarget::for_window(window_info))
            .and_then(|lookup| lookup.result.as_ref());
        let mut details_ready = snapshot.is_some();
        let mut sections = Vec::new();
        for node in snapshot
            .into_iter()
            .flat_map(|snapshot| snapshot.nodes.iter())
        {
            let owner = window_info.pid == Some(node.identity.pid);
            let id = process_info_section_id((viewport_id, &window_info.id), node.identity);
            let open = process_info_section_is_open(ctx, id, owner);
            let version = popup.versions.as_ref().and_then(|cache| {
                if open {
                    cache.request_for_application(node, true, || {
                        process_desktop_candidate(&self.apps, window_info, node)
                    })
                } else {
                    cache.request_header_for_application(node, || {
                        process_desktop_candidate(&self.apps, window_info, node)
                    })
                }
            });
            if version
                .as_ref()
                .is_some_and(|version| version.application_build.is_some())
            {
                // Visible Pi headers refresh local receipts, never ELF or package probes.
                ctx.request_repaint_after(crate::windows::versions::PI_RECEIPT_REFRESH);
            }
            let binary_is_current = version
                .as_deref()
                .and_then(|version| version.process_build_is_current());
            let mut metadata_sections = Vec::new();
            let rows = if open {
                details_ready &= version.is_some();
                metadata_sections = hosted_metadata_sections(ctx, id, node, version.as_deref());
                let launch = version.as_ref().and_then(|version| {
                    if version.runtime_name.is_some() {
                        version.application_launch.as_ref()
                    } else {
                        version.launch.as_ref()
                    }
                });
                let mut versions = if metadata_sections.is_empty() {
                    let mut rows = application_version_rows(
                        "Application",
                        version.as_deref(),
                        version.is_none(),
                    );
                    append_desktop_entry_rows(
                        &mut rows,
                        Self::process_desktop_rows(launch, version.is_none()),
                    );
                    rows
                } else {
                    if let Some(application) = metadata_sections.first_mut()
                        && !application.rows.is_empty()
                    {
                        append_desktop_entry_rows(
                            &mut application.rows,
                            Self::process_desktop_rows(launch, version.is_none()),
                        );
                    }
                    Vec::new()
                };
                let mut process = process_info_rows(window_info, node, Vec::new());
                tmux::merge_process_rows(
                    &mut process,
                    &mut versions,
                    window_info,
                    node,
                    popup.tmux.as_ref(),
                    snapshot
                        .map(|snapshot| snapshot.nodes.as_ref())
                        .unwrap_or_default(),
                );
                process.extend(versions);
                if owner {
                    let mut rows = window_info_rows(window_info, Vec::new());
                    rows.extend(process);
                    rows
                } else {
                    process
                }
            } else {
                Vec::new()
            };
            if open {
                metadata_sections.extend(tmux::sections(
                    ctx,
                    id,
                    window_info,
                    node,
                    popup.tmux.as_ref(),
                    snapshot
                        .map(|snapshot| snapshot.nodes.as_ref())
                        .unwrap_or_default(),
                ));
            }
            sections.push(InfoProcessSection {
                id,
                depth: node.depth,
                header: node.header.clone(),
                binary_is_current,
                update_target: process_update_target(window_info, node, binary_is_current),
                default_open: owner,
                rows,
                metadata_sections,
            });
        }
        if !snapshot.is_some_and(|snapshot| {
            snapshot
                .nodes
                .iter()
                .any(|node| window_info.pid == Some(node.identity.pid))
        }) {
            let identity = ProcessIdentity {
                pid: window_info.pid.unwrap_or(0),
                start_ticks: 0,
            };
            sections.push(InfoProcessSection {
                id: process_info_section_id((viewport_id, &window_info.id), identity),
                depth: 0,
                binary_is_current: None,
                update_target: None,
                header: format!(
                    "{} (window owner; process metadata {})",
                    window_application_key(window_info),
                    if snapshot.is_some() {
                        "unavailable"
                    } else {
                        "loading"
                    }
                )
                .into(),
                default_open: true,
                metadata_sections: Vec::new(),
                rows: window_info_rows(window_info, {
                    let mut rows = process_version_rows("Application", None, snapshot.is_none());
                    append_desktop_entry_rows(
                        &mut rows,
                        Self::process_desktop_rows(None, snapshot.is_none()),
                    );
                    if let Some(row) = rows.iter_mut().find(|row| row.label == "Desktop file")
                        && Path::new(&desktop_file_path).is_absolute()
                    {
                        row.value = desktop_file_value(
                            Some(Path::new(&desktop_file_path)),
                            None,
                            snapshot.is_none(),
                        );
                    }
                    rows
                }),
            });
        }

        (
            InfoPopupData {
                title: format!("Window Info: {}", window_info.title),
                heading: window_info.title.clone(),
                subtitle: "Expand a process to inspect its own metadata. Window details belong to the window owner.".to_string(),
                scroll_sensitivity: self.info_scroll_sensitivity,
                rows: Vec::new(),
                process_tree: Some(InfoProcessTree {
                    sections,
                    notice: snapshot
                        .map(|snapshot| snapshot.notice.clone())
                        .unwrap_or_else(|| "Loading process tree...".into()),
                }),
            },
            details_ready,
        )
    }

    fn app_info_popup_data(&self, state: &AppInfoPopup) -> InfoPopupData {
        let app_info = &state.app;

        InfoPopupData {
            title: format!("Info: {}", app_info.name),
            heading: app_info.name.clone(),
            subtitle: "Installed application, launch chain and desktop-entry metadata".to_string(),
            scroll_sensitivity: self.info_scroll_sensitivity,
            rows: application_info_rows(state),
            process_tree: None,
        }
    }

    pub(super) fn show_settings_popup(&mut self, ctx: &egui::Context) {
        if !ctx.embed_viewports() {
            self.show_settings_native_viewport(ctx);
            return;
        }
        let viewport_scale_factor =
            (self.settings_menu_scale_anchor / self.ui_scale).clamp(0.2, 5.0);
        let mut is_open = true;
        let mut should_close = false;
        egui::Window::new("Launcher Settings")
            .id(egui::Id::new("launcher_settings_popup"))
            .default_size([
                (SETTINGS_VIEWPORT_SIZE[0] + 20.0) * viewport_scale_factor,
                (SETTINGS_VIEWPORT_SIZE[1] + 80.0) * viewport_scale_factor,
            ])
            .min_size([
                SETTINGS_VIEWPORT_MIN_SIZE[0] * viewport_scale_factor,
                SETTINGS_VIEWPORT_MIN_SIZE[1] * viewport_scale_factor,
            ])
            .resizable(true)
            .collapsible(false)
            .order(egui::Order::Foreground)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 240))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                    ))
                    .corner_radius(egui::CornerRadius::same(12)),
            )
            .open(&mut is_open)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.render_settings_panel(ui) {
                            should_close = true;
                        }
                    });
            });

        if should_close || !is_open {
            self.close_settings_menu();
        }
    }

    pub(super) fn show_info_popups(&mut self, ctx: &egui::Context) {
        let mut popups = std::mem::take(&mut self.info_popups.items);
        popups.retain_mut(|popup| {
            let (data, size, min_size) = match &mut popup.target {
                InfoTarget::Window(state) => {
                    if let Some(current) = self
                        .windows
                        .iter()
                        .find(|window| window.id == state.window.id)
                    {
                        state.window = current.clone();
                    }
                    if let Some(lookup) = state.lookup.as_mut() {
                        lookup.poll();
                    }
                    state
                        .versions
                        .get_or_insert_with(|| ProcessVersionCache::new(ctx.clone(), popup.id));
                    let target = WindowProcessTreeTarget::for_window(&state.window);
                    if state
                        .lookup
                        .as_ref()
                        .is_none_or(|lookup| lookup.target != target && lookup.result.is_some())
                    {
                        if let Some(cache) = &state.versions {
                            cache.reset();
                        }
                        state.lookup = Some(WindowProcessTreeLookup::start(
                            target,
                            ctx.clone(),
                            popup.id,
                        ));
                    }
                    state.poll_tmux(ctx, popup.id);
                    let (data, details_ready) = self.window_info_popup_data(ctx, popup.id, state);
                    let size = popup.window_initial_size(ctx, &data, details_ready);
                    let min_size = [520.0_f32.min(size[0]), 360.0_f32.min(size[1])];
                    (data, size, min_size)
                }
                InfoTarget::App(state) => {
                    state.poll_metadata(ctx, popup.id);
                    let details_ready = state.metadata.is_some();
                    let data = self.app_info_popup_data(state);
                    let size = popup.window_initial_size(ctx, &data, details_ready);
                    let min_size = [520.0_f32.min(size[0]), 360.0_f32.min(size[1])];
                    (data, size, min_size)
                }
            };
            if !ctx.embed_viewports() {
                show_deferred_info_popup(
                    ctx,
                    popup.id,
                    data,
                    size,
                    min_size,
                    popup.focus_requested.clone(),
                    self.popup_event_sender.clone(),
                );
                return true;
            }

            let mut is_open = true;
            let response = egui::Window::new(&data.title)
                .id(egui::Id::new(popup.id))
                .default_size(size)
                .min_size(min_size)
                .resizable(true)
                .collapsible(false)
                .vscroll(false)
                .order(egui::Order::Foreground)
                .frame(
                    egui::Frame::window(&ctx.style())
                        .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 240))
                        .stroke(egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                        ))
                        .corner_radius(egui::CornerRadius::same(12)),
                )
                .open(&mut is_open)
                .show(ctx, |ui| {
                    crate::popups::render_info_popup_contents(
                        ui,
                        &data,
                        popup.id,
                        &self.popup_event_sender,
                    );
                });
            if popup.focus_requested.swap(false, Ordering::Relaxed)
                && let Some(response) = response
            {
                ctx.move_to_top(response.response.layer_id);
            }
            is_open
        });
        self.info_popups.items = popups;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windows::versions::PackageVersionSource;

    fn app(path: &str) -> InfoTarget {
        InfoTarget::App(AppInfoPopup::new(AppInfo {
            name: "Same display name".into(),
            exec: "test-app".into(),
            desktop_file_path: path.into(),
            icon_path: None,
            comment: None,
            is_settings_module: false,
        }))
    }

    fn desktop_process(pid: i32, name: &str) -> ProcessInfoNode {
        ProcessInfoNode {
            identity: ProcessIdentity {
                pid,
                start_ticks: 42,
            },
            parent_pid: 1,
            depth: 1,
            name: name.into(),
            script_name: None,
            header: name.into(),
            executable: Some(format!("/not-accessed/{name}").into()),
            executable_identity: Some((1, 2)),
            executable_error: None,
            cwd: None,
            cwd_error: None,
            raw_argv0: None,
            command_line: None,
        }
    }

    #[test]
    fn chrome_children_receive_the_owner_desktop_only_as_an_unverified_candidate() {
        let InfoTarget::Window(mut popup) = window("chrome-window") else {
            unreachable!()
        };
        popup.window.pid = Some(10);
        popup.window.desktop_file_name = Some("google-chrome-fast".into());
        popup.window.title = "A tab title, not an application name".into();
        popup.window.icon_path = Some("/window-only.png".into());
        let apps = [AppInfo {
            name: "Google Chrome".into(),
            exec: "/not-accessed/google-chrome-fast %U".into(),
            desktop_file_path: "/not-accessed/google-chrome-fast.desktop".into(),
            icon_path: Some("/desktop/chrome.png".into()),
            comment: Some("Web browser".into()),
            is_settings_module: false,
        }];
        let owner = desktop_process(10, "chrome");
        let child = desktop_process(11, "chrome");
        let helper = desktop_process(12, "chrome_crashpad_handler");
        assert_eq!(
            process_desktop_candidate(&apps, &popup.window, &owner),
            Some((&apps[0], true))
        );
        for node in [&child, &helper] {
            assert_eq!(
                process_desktop_candidate(&apps, &popup.window, node),
                Some((&apps[0], false))
            );
            let rows = process_info_rows(&popup.window, node, Vec::new());
            assert!(
                !rows
                    .iter()
                    .any(|row| matches!(row.label.as_str(), "Raw window title" | "Geometry"))
            );
        }
        let launch = crate::windows::versions::ApplicationLaunch {
            desktop_entry: Some(apps[0].clone()),
            desktop_exists: Some(true),
            published_target_verified: true,
            ..Default::default()
        };
        let rows = App::process_desktop_rows(Some(&launch), false);
        for (label, value) in [
            ("Name", "Google Chrome"),
            ("Comment", "Web browser"),
            ("Icon path", "/desktop/chrome.png"),
        ] {
            assert!(
                rows.iter()
                    .any(|row| row.label == label && row.value == value)
            );
        }
        let mut no_icon = launch;
        no_icon.desktop_entry.as_mut().unwrap().icon_path = None;
        assert!(
            App::process_desktop_rows(Some(&no_icon), false)
                .iter()
                .any(|row| row.label == "Icon path" && row.value == "-")
        );
    }

    #[test]
    fn ambiguous_desktop_names_or_commands_never_select_the_first_candidate() {
        let InfoTarget::Window(mut popup) = window("ambiguous-window") else {
            unreachable!()
        };
        popup.window.pid = Some(10);
        popup.window.desktop_file_name = Some("browser".into());
        let apps = ["/one/browser.desktop", "/two/browser.desktop"].map(|path| AppInfo {
            name: "Browser".into(),
            exec: "/not-accessed/google-chrome-fast".into(),
            desktop_file_path: path.into(),
            icon_path: None,
            comment: None,
            is_settings_module: false,
        });
        for pid in [10, 11] {
            assert!(
                process_desktop_candidate(&apps, &popup.window, &desktop_process(pid, "chrome"))
                    .is_none()
            );
        }
        popup.window.desktop_file_name = None;
        assert!(
            process_desktop_candidate(
                &apps,
                &popup.window,
                &desktop_process(11, "google-chrome-fast")
            )
            .is_none()
        );
        popup.window.class = "Browser".into();
        assert!(window_desktop_candidate(&apps, &popup.window).is_none());
    }

    #[test]
    fn app_progs_lookup_publishes_once_without_restarting_on_later_frames() {
        let InfoTarget::App(mut popup) = app("/test/progs.desktop") else {
            unreachable!();
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        popup.receiver = Some(rx);
        let ctx = egui::Context::default();
        popup.poll_metadata(&ctx, egui::ViewportId::ROOT);
        assert!(popup.metadata.is_none() && popup.receiver.is_some());
        let metadata = crate::windows::versions::ApplicationInfo {
            metadata: crate::windows::versions::ProgsMetadata {
                capabilities: Some(
                    "GUI \u{00b7} CLI  \u{2022}  Primary role: GUI application".into(),
                ),
                description: Some("Terminal emulator".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        tx.send(crate::windows::versions::ApplicationInfoUpdate::Details(
            metadata.clone(),
        ))
        .unwrap();
        drop(tx);
        popup.poll_metadata(&ctx, egui::ViewportId::ROOT);
        assert_eq!(popup.metadata.as_ref(), Some(&metadata));
        assert!(popup.receiver.is_none());
        popup.poll_metadata(&ctx, egui::ViewportId::ROOT);
        assert_eq!(popup.metadata.as_ref(), Some(&metadata));
        assert!(popup.receiver.is_none());
    }

    #[test]
    fn disconnected_app_progs_lookup_stops_loading_instead_of_retrying() {
        let InfoTarget::App(mut popup) = app("/test/progs.desktop") else {
            unreachable!();
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        popup.receiver = Some(rx);
        drop(tx);
        let ctx = egui::Context::default();
        for _ in 0..3 {
            popup.poll_metadata(&ctx, egui::ViewportId::ROOT);
            assert_eq!(popup.metadata, Some(Default::default()));
            assert!(popup.receiver.is_none());
        }
    }

    #[test]
    fn combined_version_fields_use_the_requested_format_and_preserve_scope_and_freshness() {
        use crate::windows::versions::ProcessVersion;
        let name = "Xfce Terminal 1.2.0-fork0.4.9-46";
        let installed = "4fcbbefb36addaaee2f2948a22e2b808698149fa";
        for role in ["Application", "Runtime"] {
            for status in [Some(true), Some(false), None] {
                let running = if status == Some(false) {
                    "7f0240905d72a3925acf2afe975ab383042ebd2a"
                } else {
                    installed
                };
                let version = ProcessVersion {
                    version: Some(name.into()),
                    build_id: Some(running.into()),
                    latest_version: Some(name.into()),
                    latest_build_id: Some(installed.into()),
                    running_latest_binary: status,
                    ..Default::default()
                };
                let rows = process_version_rows(role, Some(&version), false);
                assert_eq!(rows[0].label, VERSION_FIELD_LABEL);
                assert_eq!(rows[0].value, format!("{name} ({running})"));
                assert_eq!(rows[0].scope, InfoFieldScope::RunningInstance);
                assert_eq!(rows[0].binary_is_current, status);
                assert_eq!(rows[1].label, LATEST_VERSION_FIELD_LABEL);
                assert_eq!(
                    rows[1].value,
                    "Xfce Terminal 1.2.0-fork0.4.9-46 (4fcbbefb36addaaee2f2948a22e2b808698149fa)"
                );
                assert_eq!(rows[1].scope, InfoFieldScope::Installation);
                assert_eq!(rows[1].binary_is_current, None);
                assert!(!rows.iter().any(|row| matches!(
                    row.label.as_str(),
                    "Version"
                        | "ELF build ID"
                        | "Latest version (on disk)"
                        | "Latest ELF build ID (on disk)"
                )));
            }
        }
    }

    #[test]
    fn combined_version_fields_keep_loading_and_missing_builds_neutral_without_duplicates() {
        for pending in [true, false] {
            let rows = process_version_rows("Application", None, pending);
            for row in &rows[..2] {
                assert_eq!(
                    row.value,
                    if pending { "Loading..." } else { "Unavailable" }
                );
                assert!(row.binary_is_current.is_none());
            }
        }
        let InfoTarget::App(mut popup) = app("/test/terminal.desktop") else {
            unreachable!()
        };
        popup.metadata = Some(crate::windows::versions::ApplicationInfo {
            version: Some("Xfce Terminal 1.2.0".into()),
            build_id: Some("installed-id".into()),
            ..Default::default()
        });
        let rows = application_info_rows(&popup);
        assert_eq!(rows[0].label, LATEST_VERSION_FIELD_LABEL);
        assert_eq!(rows[0].value, "Xfce Terminal 1.2.0 (installed-id)");
        assert_eq!(rows[0].scope, InfoFieldScope::Installation);
        assert!(rows[0].binary_is_current.is_none());
    }

    #[test]
    fn npm_version_rows_keep_upstream_and_node_registry_versions_out_of_running_identity() {
        use crate::windows::versions::{NodeApplication, NpmTarget, ProcessVersion};
        let mut native = ProcessVersion {
            version: Some("codex-cli 0.159.1".into()),
            latest_version: Some("codex-cli 0.159.1".into()),
            npm_target: Some(NpmTarget {
                name: "@openai/codex".into(),
                upstream: true,
            }),
            ..Default::default()
        };
        let rows = process_version_rows("Application", Some(&native), false);
        assert!(
            rows.iter()
                .any(|row| row.label == "Latest package version (npm)"
                    && row.value.contains("Checking npm registry"))
        );
        native.latest_npm_version =
            Some("@openai/codex 0.160.0 (npm latest; upstream reference)".into());
        let rows = process_version_rows("Application", Some(&native), false);
        assert!(rows.iter().any(|row| row.label == VERSION_FIELD_LABEL
            && row.value == "codex-cli 0.159.1 (Unavailable)"));
        let npm_row = rows
            .iter()
            .find(|row| row.label == "Latest package version (npm)")
            .unwrap();
        assert!(npm_row.value.contains("upstream reference"));
        assert!(npm_row.binary_is_current.is_none());

        let hosted = ProcessVersion {
            runtime_name: Some("Node.js"),
            version: Some("v26.4.0".into()),
            application: Some(NodeApplication {
                package: "@example/tool".into(),
                installed_version: "1.0.0".into(),
                entry_point: "/tool/cli.js".into(),
                manifest: "/tool/package.json".into(),
                identification: "test metadata",
            }),
            application_npm_target: NpmTarget::package("@example/tool"),
            application_latest_npm_version: Some("@example/tool 2.0.0 (npm latest)".into()),
            ..Default::default()
        };
        let rows = application_version_rows("Application", Some(&hosted), false);
        assert!(
            rows.iter()
                .any(|row| row.label == "Latest package version (npm)"
                    && row.value == "@example/tool 2.0.0 (npm latest)")
        );
        assert!(
            rows.iter()
                .any(|row| row.label == "Installed version" && row.value == "1.0.0")
        );
        assert!(!rows.iter().any(|row| row.value == "v26.4.0"));
        let runtime = process_version_rows("Runtime", Some(&hosted), false);
        assert!(!runtime.iter().any(|row| row.label.ends_with("(npm)")));
        assert!(
            runtime
                .iter()
                .any(|row| row.label == VERSION_FIELD_LABEL && row.value == "v26.4.0 (Unavailable)")
        );
    }

    #[test]
    fn application_npm_updates_and_worker_failure_do_not_leave_checking_forever() {
        use crate::windows::versions::{ApplicationInfo, ApplicationInfoUpdate, NpmTarget};
        for success in [false, true] {
            let InfoTarget::App(mut popup) = app("/test/tool.desktop") else {
                unreachable!()
            };
            let (tx, rx) = std::sync::mpsc::sync_channel(3);
            popup.receiver = Some(rx);
            tx.send(ApplicationInfoUpdate::Details(ApplicationInfo {
                version: Some("local version".into()),
                npm_target: NpmTarget::package("@example/tool"),
                ..Default::default()
            }))
            .unwrap();
            let ctx = egui::Context::default();
            popup.poll_metadata(&ctx, egui::ViewportId::ROOT);
            assert!(
                application_info_rows(&popup)
                    .iter()
                    .any(|row| row.label == "Latest package version (npm)"
                        && row.value.contains("Checking npm registry"))
            );
            if success {
                tx.send(ApplicationInfoUpdate::NpmVersion(
                    "@example/tool 2.0.0 (npm latest)".into(),
                ))
                .unwrap();
            }
            drop(tx);
            popup.poll_metadata(&ctx, egui::ViewportId::ROOT);
            assert!(popup.receiver.is_none());
            let rows = application_info_rows(&popup);
            let row = rows
                .iter()
                .find(|row| row.label == "Latest package version (npm)")
                .unwrap();
            assert_eq!(
                row.value,
                if success {
                    "@example/tool 2.0.0 (npm latest)"
                } else {
                    "Unavailable (npm lookup ended without a result)"
                }
            );
            assert_eq!(
                popup.metadata.as_ref().unwrap().version.as_deref(),
                Some("local version")
            );
        }
    }

    #[test]
    fn application_details_are_published_before_the_available_package_query_finishes() {
        use crate::windows::versions::{ApplicationInfo, ApplicationInfoUpdate, PackageTarget};
        let InfoTarget::App(mut popup) = app("/test/browser.desktop") else {
            unreachable!();
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        popup.receiver = Some(rx);
        tx.send(ApplicationInfoUpdate::Details(ApplicationInfo {
            version: Some("Browser 1.0".into()),
            package_target: Some(PackageTarget {
                name: "browser".into(),
                inferred: false,
            }),
            ..Default::default()
        }))
        .unwrap();
        let ctx = egui::Context::default();
        popup.poll_metadata(&ctx, egui::ViewportId::ROOT);
        assert_eq!(
            popup.metadata.as_ref().unwrap().version.as_deref(),
            Some("Browser 1.0")
        );
        assert!(popup.receiver.is_some());
        let rows = application_info_rows(&popup);
        assert!(
            rows.iter()
                .any(|row| row.label == "Latest version (Pacman/AUR)"
                    && row.value == "Checking available package version...")
        );
        tx.send(ApplicationInfoUpdate::PackageVersion(
            crate::windows::versions::available_package_fixture(
                PackageVersionSource::Pacman,
                "extra/browser 2.0",
            ),
        ))
        .unwrap();
        drop(tx);
        popup.poll_metadata(&ctx, egui::ViewportId::ROOT);
        assert!(popup.receiver.is_none());
        let rows = application_info_rows(&popup);
        assert!(
            rows.iter()
                .any(|row| row.label == LATEST_VERSION_FIELD_LABEL
                    && row.value == "Browser 1.0 (Unavailable)")
        );
        assert!(
            rows.iter()
                .any(|row| row.label == "Latest version (in last Pacman sync)"
                    && row.value == "extra/browser 2.0 (synced 2 days ago)")
        );
    }

    #[test]
    fn application_package_rows_show_their_actual_source_and_its_own_age() {
        use crate::windows::versions::{ApplicationInfo, PackageTarget};
        for (source, text, label, value) in [
            (
                PackageVersionSource::Pacman,
                "extra/example 1.2.3-1",
                "Latest version (in last Pacman sync)",
                "extra/example 1.2.3-1 (synced 2 days ago)",
            ),
            (
                PackageVersionSource::Aur,
                "aur/example 1.2.3-1",
                "Latest version (AUR)",
                "aur/example 1.2.3-1 (checked 2 minutes ago)",
            ),
        ] {
            let InfoTarget::App(mut popup) = app("/fixture/no-live-query.desktop") else {
                unreachable!()
            };
            popup.metadata = Some(ApplicationInfo {
                package_target: Some(PackageTarget {
                    name: "example".into(),
                    inferred: false,
                }),
                latest_package_version: Some(crate::windows::versions::available_package_fixture(
                    source, text,
                )),
                ..Default::default()
            });
            let rows = application_info_rows(&popup);
            let row = rows.iter().find(|row| row.label == label).unwrap();
            assert_eq!(row.value, value);
            assert_eq!(row.scope, InfoFieldScope::Installation);
            assert!(row.binary_is_current.is_none());
            assert!(!row.searched);
        }
    }

    #[test]
    fn classifications_show_only_initials_in_application_process_and_runtime_info() {
        use crate::windows::versions::{ApplicationInfo, ProcessVersion};
        for (classification, expected) in [
            ("[PDT] Pacman Dependency Tool (Package-managed)", "PDT"),
            ("[PET] Pacman Explicit Tool (Package-managed)", "PET"),
            ("[ADT] AUR Dependency Tool (Package-managed)", "ADT"),
            ("[AET] AUR Explicit Tool (Package-managed)", "AET"),
            ("[NET] npm Explicit Tool (Toolchain-managed)", "NET"),
            ("[CET] Cargo Explicit Tool (Toolchain-managed)", "CET"),
            ("[UET] uv Explicit Tool (Toolchain-managed)", "UET"),
            ("[FRK] Fork (User-managed)", "FRK"),
            ("[DEV] DEV (User-managed)", "DEV"),
            ("[CLO] Git Clone (External source)", "CLO"),
            ("[FKM] Fork (User-managed)", "FRK"),
            ("[DVM] DEV (User-managed)", "DEV"),
            ("[CLX] Git Clone (External source)", "CLO"),
            ("[NPT] npm (Toolchain-managed)", "NUT"),
            ("[CAT] Cargo (Toolchain-managed)", "CUT"),
            ("[UVT] uv (Toolchain-managed)", "UUT"),
            (
                "[UNC] UNC (Unclassified); Progs inventory unavailable; refresh Progs",
                "UNC",
            ),
            ("PDT", "PDT"),
            ("Unknown", "Unknown"),
        ] {
            let version = ProcessVersion {
                installation: Some(classification.into()),
                application_installation: Some(classification.into()),
                ..Default::default()
            };
            let InfoTarget::App(mut state) = app("/apps/example.desktop") else {
                unreachable!();
            };
            state.metadata = Some(ApplicationInfo {
                installation: version.installation.clone(),
                ..Default::default()
            });
            for rows in [
                application_info_rows(&state),
                process_version_rows("Application", Some(&version), false),
                process_version_rows("Runtime", Some(&version), false),
                application_version_rows("Application", Some(&version), false),
            ] {
                assert_eq!(
                    rows.iter().find(|row| row.label == "Type").unwrap().value,
                    expected
                );
            }
            assert_eq!(version.installation.as_deref(), Some(classification));
        }
    }

    #[test]
    fn application_info_fields_are_installation_metadata_even_while_loading() {
        let InfoTarget::App(mut state) = app("/apps/example.desktop") else {
            unreachable!();
        };
        for ready in [false, true] {
            if ready {
                state.metadata = Some(Default::default());
            }
            let rows = application_info_rows(&state);
            assert!(!rows.is_empty());
            assert!(
                rows.iter()
                    .all(|row| row.scope == InfoFieldScope::Installation)
            );
        }
    }

    #[test]
    fn python_build_id_absence_is_independent_of_version_and_process_access() {
        use crate::windows::versions::ProcessVersion;
        let mut version = ProcessVersion {
            runtime_name: Some("Python"),
            version: Some("Python 3.12.12".into()),
            latest_version: Some("Python 3.12.12".into()),
            build_id_reason: Some("Not embedded (no GNU build ID)".into()),
            latest_build_id_reason: Some("Not embedded (no GNU build ID)".into()),
            ..Default::default()
        };
        let rows = process_version_rows("Runtime", Some(&version), false);
        for label in [VERSION_FIELD_LABEL, LATEST_VERSION_FIELD_LABEL] {
            let row = rows.iter().find(|row| row.label == label).unwrap();
            assert_eq!(row.value, "Python 3.12.12 (Not embedded (no GNU build ID))");
            assert!(row.binary_is_current.is_none());
        }
        version.running_unavailable = Some("permission denied reading process executable");
        version.version = None;
        version.on_disk_candidate = Some("/usr/lib/systemd/systemd".into());
        version.on_disk_candidate_reason =
            Some("absolute argv[0]; running executable unverified".into());
        let rows = process_version_rows("Runtime", Some(&version), false);
        for label in [VERSION_FIELD_LABEL] {
            assert!(
                rows.iter()
                    .find(|row| row.label == label)
                    .unwrap()
                    .value
                    .contains("permission denied")
            );
        }
        let candidate = rows
            .iter()
            .find(|row| row.label == "On-disk candidate")
            .unwrap();
        assert_eq!(candidate.scope, InfoFieldScope::Installation);
        assert!(candidate.value.contains("running executable unverified"));
        assert_eq!(
            rows.iter()
                .find(|row| row.label == LATEST_VERSION_FIELD_LABEL)
                .unwrap()
                .value,
            "Python 3.12.12 (Not embedded (no GNU build ID))"
        );
    }

    #[test]
    fn python_source_checkout_does_not_invent_a_loaded_version_or_compilation_status() {
        use crate::windows::versions::{ProcessVersion, ProgsMetadata, ScriptApplication};
        let version = ProcessVersion {
            runtime_name: Some("Python"),
            script_application: Some(ScriptApplication {
                name: "whisper_service".into(),
                target: "whisper_service.py".into(),
                kind: "Python script",
                entry_point: Some("/project/whisper_service.py".into()),
                identification: "Process arguments",
            }),
            application_installation: Some("[DEV] Development Checkout (User-managed)".into()),
            application_progs_metadata: ProgsMetadata {
                source_checkout: Some("/project".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let rows = application_version_rows("Application", Some(&version), false);
        assert_eq!(
            rows.iter().find(|row| row.label == "Type").unwrap().value,
            "DEV"
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.label == "Source directory")
                .unwrap()
                .value,
            "/project"
        );
        let changes = rows
            .iter()
            .find(|row| row.label == "Source changes since start")
            .unwrap();
        assert!(changes.value.contains("no startup source snapshot"));
        assert!(
            !rows
                .iter()
                .any(|row| row.label == "Build status" || row.value == "0.1.0")
        );
        assert!(
            rows.iter()
                .find(|row| row.label == "Running version")
                .unwrap()
                .value
                .starts_with("Unknown")
        );
        assert!(
            rows.iter()
                .find(|row| row.label == "Installed version")
                .unwrap()
                .value
                .starts_with("Unknown")
        );
    }

    #[test]
    fn inaccessible_process_details_keep_permission_errors_separate_from_candidate_paths() {
        let InfoTarget::Window(state) = window("systemd") else {
            unreachable!()
        };
        let node = ProcessInfoNode {
            identity: crate::models::ProcessIdentity {
                pid: 3140,
                start_ticks: 30364,
            },
            parent_pid: 1,
            depth: 0,
            name: "systemd".into(),
            script_name: None,
            header: "systemd (pid 3140)".into(),
            executable: None,
            executable_identity: None,
            executable_error: Some("permission denied reading process executable"),
            cwd: None,
            cwd_error: Some("permission denied reading process working directory"),
            raw_argv0: Some("/usr/lib/systemd/systemd".into()),
            command_line: Some("/usr/lib/systemd/systemd --user".into()),
        };
        let rows = process_info_rows(&state.window, &node, Vec::new());
        for label in ["Executable path", "Working directory"] {
            let row = rows.iter().find(|row| row.label == label).unwrap();
            assert!(row.value.contains("permission denied"));
            assert!(!row.value.contains("/usr/lib/systemd"));
        }
    }

    #[test]
    fn native_process_info_matches_the_requested_order_with_package_updates_and_roots() {
        use crate::windows::versions::{
            ApplicationLaunch, NpmTarget, PackageTarget, ProcessVersion, ProgsMetadata,
        };
        let InfoTarget::Window(state) = window("bash") else {
            unreachable!()
        };
        let node = ProcessInfoNode {
            identity: crate::models::ProcessIdentity {
                pid: 42,
                start_ticks: 100,
            },
            parent_pid: 41,
            depth: 1,
            name: "bash".into(),
            script_name: None,
            header: "bash (pid 42) - /usr/bin/bash".into(),
            executable_error: None,
            cwd_error: None,
            raw_argv0: None,
            executable: Some("/usr/bin/bash".into()),
            executable_identity: None,
            cwd: Some("/work/cmake".into()),
            command_line: Some("bash".into()),
        };
        let mut version = ProcessVersion {
            version: Some("GNU bash, version 5.3.15(1)-release".into()),
            latest_version: Some("GNU bash, version 5.3.15(1)-release".into()),
            build_id: Some("running-build-id".into()),
            latest_build_id: Some("installed-build-id".into()),
            running_latest_binary: Some(false),
            uncompiled_changes: Some("Unknown (no supported local build metadata)".into()),
            installation: Some("[PDT] Pacman Dependency Tool (Package-managed)".into()),
            package: Some("bash 5.3.15-2".into()),
            package_target: Some(PackageTarget {
                name: "bash".into(),
                inferred: false,
            }),
            latest_package_version: Some(crate::windows::versions::available_package_fixture(
                PackageVersionSource::Pacman,
                "extra/bash 5.3.20-2",
            )),
            progs_metadata: ProgsMetadata {
                description: Some("The GNU Bourne Again shell".into()),
                capabilities: Some("CLI".into()),
                package_type: Some("[PDP] Pacman Dependency (Package-managed)".into()),
                root_packages: Some("base 3-3 [PEP]".into()),
                source_checkout: None,
            },
            launch: Some(ApplicationLaunch {
                executable: Some("/usr/bin/bash".into()),
                chain: vec!["/usr/bin/rbash".into(), "/usr/bin/bash".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        for role in ["Application", "Runtime"] {
            let mut versions = process_version_rows(role, Some(&version), false);
            append_desktop_entry_rows(
                &mut versions,
                desktop_entry_rows(None, Some(false), false, None),
            );
            let rows = process_info_rows(&state.window, &node, versions);
            assert_eq!(
                rows.iter()
                    .map(|row| row.label.as_str())
                    .collect::<Vec<_>>(),
                [
                    "Process name",
                    "PID",
                    "Parent PID",
                    "Process start (ticks since boot)",
                    "Working directory",
                    "Command line",
                    "Executable path",
                    VERSION_FIELD_LABEL,
                    LATEST_VERSION_FIELD_LABEL,
                    "Uncompiled changes",
                    "Type",
                    "Package",
                    "Latest version (in last Pacman sync)",
                    "Root package",
                    "Launch chain",
                    "Desktop file",
                    "Icon path",
                    "Name",
                    "Comment",
                    "Description",
                    "Capabilities",
                ]
            );
            assert_eq!(rows[7].binary_is_current, Some(false));
            assert_eq!(rows[8].binary_is_current, None);
            assert_eq!(rows[10].value, "PDT");
            assert_eq!(rows[11].value, "bash 5.3.15-2 [PDP]");
            assert_eq!(rows[13].value, "base 3-3 [PEP]");
            assert!(
                rows[..8]
                    .iter()
                    .all(|row| row.scope == InfoFieldScope::RunningInstance)
            );
            assert!(
                rows[8..]
                    .iter()
                    .all(|row| row.scope == InfoFieldScope::Installation)
            );
        }
        version.npm_target = Some(NpmTarget {
            name: "@example/tool".into(),
            upstream: true,
        });
        let rows = process_version_rows("Application", Some(&version), false);
        let position = |label: &str| rows.iter().position(|row| row.label == label).unwrap();
        assert_eq!(
            position("Latest package version (npm)"),
            position("Latest version (in last Pacman sync)") + 1
        );
        assert_eq!(
            position("Root package"),
            position("Latest package version (npm)") + 1
        );
        assert!(
            rows[position("Latest package version (npm)")]
                .value
                .starts_with("Checking")
        );
    }

    #[test]
    fn unsupported_version_reasons_reach_info_without_becoming_version_values() {
        use crate::windows::versions::ProcessVersion;
        let result = ProcessVersion {
            version_reason: Some("Pinned ELF has no recognized product banner".into()),
            latest_version_reason: Some("Selected payload metadata is unsupported".into()),
            ..Default::default()
        };
        let rows = process_version_rows("Application", Some(&result), false);
        assert_eq!(
            rows.iter()
                .find(|row| row.label == VERSION_FIELD_LABEL)
                .unwrap()
                .value,
            "Unavailable (Pinned ELF has no recognized product banner) (Unavailable)"
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.label == LATEST_VERSION_FIELD_LABEL)
                .unwrap()
                .value,
            "Unavailable (Selected payload metadata is unsupported) (Unavailable)"
        );
        assert!(result.version.is_none() && result.latest_version.is_none());
    }

    #[test]
    fn electron_info_distinguishes_running_engine_version_from_installed_product_version() {
        use crate::windows::versions::ProcessVersion;
        let result = ProcessVersion {
            version: Some("Electron 42.0.0".into()),
            latest_version: Some("2.4.1".into()),
            running_latest_binary: Some(true),
            application_installed_metadata: Some(program_metadata::interpreted::InstalledVersion {
                name: "example-editor".into(),
                version: "2.4.1".into(),
                metadata_path: "/opt/editor/resources/app/package.json".into(),
                kind: "Electron package",
                identification: "Static installed payload, not loaded assets".into(),
            }),
            ..Default::default()
        };
        let rows = process_version_rows("Application", Some(&result), false);
        assert_eq!(
            rows.iter()
                .find(|row| row.label == VERSION_FIELD_LABEL)
                .unwrap()
                .value,
            "Electron 42.0.0 (running native engine; loaded application version unknown) (Unavailable)"
        );
        let installed = &rows
            .iter()
            .find(|row| row.label == LATEST_VERSION_FIELD_LABEL)
            .unwrap()
            .value;
        assert!(
            installed.starts_with("2.4.1 (Electron package;")
                && installed.contains("not a running-version receipt")
        );
        assert_eq!(result.process_build_is_current(), None);
    }

    #[test]
    fn python_and_java_installed_versions_never_claim_the_running_loaded_version() {
        use crate::windows::versions::{ProcessVersion, ScriptApplication};
        for runtime in ["Python", "Java"] {
            let version = ProcessVersion {
                runtime_name: Some(runtime),
                script_application: Some(ScriptApplication {
                    name: "tool".into(),
                    entry_point: Some("/installed/tool".into()),
                    target: "/installed/tool".into(),
                    kind: runtime,
                    identification: "Exact argument",
                }),
                application_installed_metadata: Some(
                    program_metadata::interpreted::InstalledVersion {
                        name: "tool".into(),
                        version: "2.4".into(),
                        metadata_path: "/installed/METADATA".into(),
                        kind: "Installed application",
                        identification: "Exact installed ownership, not a running receipt".into(),
                    },
                ),
                ..Default::default()
            };
            let rows = application_version_rows("Application", Some(&version), false);
            assert_eq!(
                rows.iter()
                    .find(|row| row.label == "Installed version")
                    .unwrap()
                    .value,
                "2.4"
            );
            assert!(
                rows.iter()
                    .find(|row| row.label == "Running version")
                    .unwrap()
                    .value
                    .starts_with("Unknown")
            );
            assert!(
                rows.iter()
                    .find(|row| row.label == "Installed metadata")
                    .unwrap()
                    .value
                    .contains("/installed/METADATA")
            );
        }
    }

    #[test]
    fn completed_application_metadata_is_not_refetched_on_repeated_polls() {
        let InfoTarget::App(mut popup) = app("/fixture/no-live-query.desktop") else {
            unreachable!()
        };
        popup.app.exec = "/fixture/nonexistent-application".into();
        popup.metadata = Some(crate::windows::versions::ApplicationInfo {
            version: Some("retained".into()),
            ..Default::default()
        });
        let previous_cancel = popup.cancelled.clone();
        let ctx = egui::Context::default();
        for _ in 0..100 {
            popup.poll_metadata(&ctx, egui::ViewportId::ROOT);
            assert!(popup.receiver.is_none());
            assert!(!previous_cancel.load(Ordering::Acquire));
            assert!(Arc::ptr_eq(&previous_cancel, &popup.cancelled));
            assert_eq!(
                popup.metadata.as_ref().unwrap().version.as_deref(),
                Some("retained")
            );
        }
    }

    #[test]
    fn hosted_application_fields_end_with_description_and_capabilities_after_comment() {
        use crate::windows::versions::{
            NodeApplication, PackageTarget, ProcessVersion, ScriptApplication,
        };
        for runtime in ["Node.js", "Python", "Java"] {
            let mut version = ProcessVersion {
                runtime_name: Some(runtime),
                application_installation: Some("[DEV] DEV (User-managed)".into()),
                application_package_target: Some(PackageTarget {
                    name: "tool".into(),
                    inferred: true,
                }),
                ..Default::default()
            };
            if runtime == "Node.js" {
                version.application = Some(NodeApplication {
                    package: "@example/tool".into(),
                    installed_version: "1.0".into(),
                    entry_point: "/installed/tool/cli.js".into(),
                    manifest: "/installed/tool/package.json".into(),
                    identification: "Installed metadata",
                });
            } else {
                version.script_application = Some(ScriptApplication {
                    name: "tool".into(),
                    entry_point: Some("/installed/tool/script".into()),
                    target: "script".into(),
                    kind: runtime,
                    identification: "Process argument",
                });
            }
            let mut rows = application_version_rows("Application", Some(&version), false);
            append_desktop_entry_rows(&mut rows, desktop_entry_rows(None, None, false, None));
            let positions: Vec<_> = [
                "Type",
                "Package",
                "Latest version (Pacman/AUR)",
                "Launch chain",
                "Desktop file",
                "Icon path",
                "Name",
                "Comment",
                "Description",
                "Capabilities",
            ]
            .iter()
            .map(|label| rows.iter().position(|row| row.label == *label).unwrap())
            .collect();
            assert!(
                positions.windows(2).all(|pair| pair[0] < pair[1]),
                "{runtime}"
            );
            assert_eq!(rows.last().unwrap().label, "Capabilities");
            assert!(
                !rows
                    .iter()
                    .any(|row| row.label == "Executable path (on disk)")
            );
        }
    }

    #[test]
    fn installed_application_and_process_views_share_the_same_application_fields() {
        use crate::windows::versions::{
            ApplicationInfo, ApplicationLaunch, ProcessVersion, ProgsMetadata,
        };
        let InfoTarget::App(mut popup) = app("/test/tool.desktop") else {
            unreachable!()
        };
        let details = ApplicationInfo {
            executable: Some("/installed/current/tool".into()),
            chain: vec!["/local/bin/tool".into(), "/installed/current/tool".into()],
            resolution: "Current installation, not running identity".into(),
            version: Some("tool 2.0".into()),
            build_id: Some("current-build".into()),
            installation: Some("[FRK] Fork (User-managed)".into()),
            package: Some("tool-package 2.0".into()),
            uncompiled_changes: Some("No".into()),
            metadata: ProgsMetadata {
                capabilities: Some("CLI".into()),
                description: Some("A tool".into()),
                ..Default::default()
            },
            desktop_exists: Some(true),
            ..Default::default()
        };
        let version = ProcessVersion {
            version: Some("tool 1.0".into()),
            build_id: Some("old-build".into()),
            running_latest_binary: Some(false),
            latest_version: details.version.clone(),
            latest_build_id: details.build_id.clone(),
            installation: details.installation.clone(),
            package: details.package.clone(),
            uncompiled_changes: details.uncompiled_changes.clone(),
            progs_metadata: details.metadata.clone(),
            launch: Some(ApplicationLaunch {
                executable: details.executable.clone(),
                chain: details.chain.clone(),
                identification: details.resolution.clone(),
                desktop_entry: Some(popup.app.clone()),
                desktop_exists: details.desktop_exists,
                published_target_verified: false,
            }),
            ..Default::default()
        };
        popup.metadata = Some(details);
        let app_rows = application_info_rows(&popup);
        let mut process_rows = process_version_rows("Application", Some(&version), false);
        let launch = version.launch.as_ref().unwrap();
        append_desktop_entry_rows(
            &mut process_rows,
            desktop_entry_rows(
                launch.desktop_entry.as_ref(),
                launch.desktop_exists,
                false,
                None,
            ),
        );
        assert_eq!(process_rows[0].value, "tool 1.0 (old-build)");
        assert_eq!(process_rows[0].binary_is_current, Some(false));
        assert_eq!(process_rows[6].label, "Launch chain");
        assert_eq!(
            process_rows
                .iter()
                .filter(|row| row.label != VERSION_FIELD_LABEL)
                .map(|row| (&row.label, &row.value))
                .collect::<Vec<_>>(),
            app_rows
                .iter()
                .map(|row| (&row.label, &row.value))
                .collect::<Vec<_>>()
        );
        let labels: Vec<_> = app_rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                LATEST_VERSION_FIELD_LABEL,
                "Uncompiled changes",
                "Type",
                "Package",
                "Root package",
                "Launch chain",
                "Desktop file",
                "Icon path",
                "Name",
                "Comment",
                "Description",
                "Capabilities",
            ]
        );
        assert_eq!(
            labels
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            labels.len()
        );
    }

    #[test]
    fn not_applicable_uncompiled_changes_use_dash_in_application_process_and_runtime_rows() {
        use crate::windows::versions::{ApplicationInfo, ProcessVersion};
        for (status, expected) in [
            ("Not applicable", "-"),
            ("Not applicable (package-managed installation)", "-"),
            ("Not applicable (prebuilt uv-managed runtime)", "-"),
            (
                "Not applicable (Interpreted script; no compiled artifact to compare)",
                "-",
            ),
            (
                "Unknown (no supported local build metadata)",
                "Unknown (no supported local build metadata)",
            ),
            ("Loading...", "Loading..."),
            (
                "No newer recorded inputs detected",
                "No newer recorded inputs detected",
            ),
            (
                "Yes (recorded build inputs changed)",
                "Yes (recorded build inputs changed)",
            ),
            (
                "Yes · 25 inputs checked · 3 newer · 2 missing · added: unknown",
                "Yes · 25 inputs checked · 3 newer · 2 missing · added: unknown",
            ),
            (
                "No newer recorded inputs detected · 25 inputs checked · 0 newer · 0 missing · added: unknown · full build status unknown",
                "No newer recorded inputs detected · 25 inputs checked · 0 newer · 0 missing · added: unknown · full build status unknown",
            ),
            ("Unknown · scan incomplete", "Unknown · scan incomplete"),
        ] {
            let InfoTarget::App(mut popup) = app("/test/tool.desktop") else {
                unreachable!()
            };
            popup.metadata = Some(ApplicationInfo {
                uncompiled_changes: Some(status.into()),
                ..Default::default()
            });
            let version = ProcessVersion {
                uncompiled_changes: Some(status.into()),
                ..Default::default()
            };
            let row_value = |rows: Vec<InfoPopupRow>| {
                rows.into_iter()
                    .find(|row| row.label == "Uncompiled changes")
                    .unwrap()
                    .value
            };
            assert_eq!(
                row_value(application_info_rows(&popup)),
                expected,
                "{status}"
            );
            for scope in ["Application", "Runtime"] {
                assert_eq!(
                    row_value(process_version_rows(scope, Some(&version), false)),
                    expected,
                    "{scope}: {status}"
                );
            }
            assert_eq!(
                popup
                    .metadata
                    .as_ref()
                    .unwrap()
                    .uncompiled_changes
                    .as_deref(),
                Some(status)
            );
            assert_eq!(version.uncompiled_changes.as_deref(), Some(status));
        }
    }

    #[test]
    fn application_info_omits_settings_module_field_for_both_entry_types() {
        for is_settings_module in [false, true] {
            let InfoTarget::App(mut popup) = app("/test/tool.desktop") else {
                unreachable!();
            };
            popup.app.is_settings_module = is_settings_module;
            let rows = application_info_rows(&popup);
            assert!(!rows.iter().any(|row| row.label == "System settings module"));
            assert!(!rows.iter().any(|row| row.label == "Pinned"));
        }
    }

    #[test]
    fn application_desktop_file_row_combines_path_and_captured_existence() {
        use crate::windows::versions::ApplicationInfo;
        let InfoTarget::App(mut popup) = app("/test/browser.desktop") else {
            unreachable!();
        };
        for (metadata, expected) in [
            (None, "/test/browser.desktop (checking...)"),
            (Some(Some(true)), "/test/browser.desktop"),
            (Some(Some(false)), "/test/browser.desktop (missing)"),
            (Some(None), "/test/browser.desktop (existence unknown)"),
        ] {
            popup.metadata = metadata.map(|desktop_exists| ApplicationInfo {
                desktop_exists,
                ..Default::default()
            });
            let rows = application_info_rows(&popup);
            let desktop = rows.iter().find(|row| row.label == "Desktop file").unwrap();
            assert_eq!(desktop.value, expected);
            assert!(desktop.searched && !desktop.separator_before);
            assert_eq!(
                rows.iter()
                    .filter(|row| row.label.starts_with("Desktop file"))
                    .count(),
                1
            );
        }
        let InfoTarget::App(popup) = app("") else {
            unreachable!();
        };
        assert!(
            application_info_rows(&popup)
                .iter()
                .any(|row| row.label == "Desktop file" && row.value == "None")
        );
    }

    #[test]
    fn absent_desktop_entries_use_none_and_dashes_for_apps_and_processes() {
        let InfoTarget::App(mut popup) = app("") else {
            unreachable!();
        };
        popup.app.icon_path = Some("/test/fallback.png".into());
        popup.app.comment = Some("Not desktop-entry metadata".into());
        for rows in [
            application_info_rows(&popup),
            desktop_entry_rows(None, None, false, Some(Path::new("/test/fallback.png"))),
        ] {
            for (label, expected) in [
                ("Desktop file", "None"),
                ("Icon path", "-"),
                ("Name", "-"),
                ("Comment", "-"),
            ] {
                let row = rows.iter().find(|row| row.label == label).unwrap();
                assert_eq!(row.value, expected, "{label}");
            }
        }
    }

    #[test]
    fn pending_desktop_entries_are_not_reported_as_absent() {
        let rows = desktop_entry_rows(None, None, true, Some(Path::new("/test/pending.png")));
        for row in &rows {
            assert_eq!(
                row.value,
                if row.label == "Icon path" {
                    "/test/pending.png"
                } else {
                    "Loading..."
                }
            );
        }
    }

    #[test]
    fn application_info_keeps_wrapper_metadata_separate_and_only_draws_captured_file_state() {
        use crate::windows::versions::{ApplicationInfo, ProgsMetadata};
        let InfoTarget::App(mut popup) = app("/not-a-real-directory/browser.desktop") else {
            unreachable!();
        };
        popup.metadata = Some(ApplicationInfo {
            launcher: Some("/not-a-real-directory/browser-fast".into()),
            launcher_resolved: Some("/config/browser-fast".into()),
            launcher_exists: true,
            desktop_exists: Some(true),
            chain: vec![
                "/not-a-real-directory/browser-fast".into(),
                "/config/browser-fast".into(),
                "/opt/browser/browser".into(),
            ],
            executable: Some("/opt/browser/browser".into()),
            installation: Some("[AEP] AUR Explicit (Package-managed)".into()),
            package: Some("browser 1.0-1".into()),
            version: Some("Browser 1.0".into()),
            build_id: Some("abcd1234".into()),
            uncompiled_changes: Some("Not applicable (package-managed installation)".into()),
            metadata: ProgsMetadata {
                capabilities: Some("GUI".into()),
                description: Some("Actual browser description".into()),
                ..Default::default()
            },
            launcher_installation: Some("[DEV] DEV (User-managed)".into()),
            launcher_metadata: ProgsMetadata {
                capabilities: Some("GUI".into()),
                description: Some("Custom launch wrapper".into()),
                ..Default::default()
            },
            ..Default::default()
        });
        for _ in 0..3 {
            let rows = application_info_rows(&popup);
            let value = |label: &str| {
                rows.iter()
                    .find(|row| row.label == label)
                    .unwrap()
                    .value
                    .as_str()
            };
            assert_eq!(value("Description"), "Actual browser description");
            assert!(
                !rows
                    .iter()
                    .any(|row| row.label == "Executable path (on disk)")
            );
            assert_eq!(
                value("Launch chain"),
                "/not-a-real-directory/browser-fast -> /config/browser-fast -> /opt/browser/browser"
            );
            assert_eq!(value(LATEST_VERSION_FIELD_LABEL), "Browser 1.0 (abcd1234)");
            assert_eq!(value("Package"), "browser 1.0-1");
            assert!(!rows.iter().any(|row| row.label.starts_with("Launcher ")
                || matches!(row.label.as_str(), "Raw Exec" | "Cleaned command")));
            assert_eq!(
                value("Desktop file"),
                "/not-a-real-directory/browser.desktop"
            );
            assert!(
                !rows
                    .iter()
                    .any(|row| row.label.contains("PID") || row.label == "Running version")
            );
            let desktop_index = rows
                .iter()
                .position(|row| row.label == "Desktop file")
                .unwrap();
            assert!(rows[..desktop_index].iter().all(|row| !row.searched));
        }
    }

    #[test]
    fn unresolved_application_info_does_not_substitute_the_wrapper_description() {
        let InfoTarget::App(mut popup) = app("/test/browser.desktop") else {
            unreachable!();
        };
        popup.metadata = Some(crate::windows::versions::ApplicationInfo {
            resolution: "Unknown (dynamic wrapper target)".into(),
            launcher_metadata: crate::windows::versions::ProgsMetadata {
                description: Some("Local custom wrapper".into()),
                ..Default::default()
            },
            ..Default::default()
        });
        let rows = application_info_rows(&popup);
        assert!(
            rows.iter()
                .any(|row| row.label == "Description" && row.value == "Unavailable")
        );
        assert!(!rows.iter().any(|row| row.value == "Local custom wrapper"));
        assert!(!rows.iter().any(|row| row.label == "Identification"));
        assert!(
            !rows
                .iter()
                .any(|row| row.label.contains("Latest package version"))
        );
    }

    #[test]
    fn closing_application_info_cancels_its_worker_and_disconnects_the_bounded_channel() {
        let InfoTarget::App(mut popup) = app("/test/browser.desktop") else {
            unreachable!();
        };
        let cancelled = popup.cancelled.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        popup.receiver = Some(rx);
        drop(popup);
        assert!(cancelled.load(Ordering::Acquire));
        assert!(
            tx.try_send(
                crate::windows::versions::ApplicationInfoUpdate::PackageVersion("stale".into())
            )
            .is_err()
        );
    }

    #[test]
    fn failed_package_worker_does_not_leave_the_application_info_checking_forever() {
        use crate::windows::versions::{ApplicationInfo, PackageTarget};
        let InfoTarget::App(mut popup) = app("/test/browser.desktop") else {
            unreachable!();
        };
        popup.metadata = Some(ApplicationInfo {
            version: Some("Browser 1.0".into()),
            package_target: Some(PackageTarget {
                name: "browser".into(),
                inferred: false,
            }),
            ..Default::default()
        });
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        popup.receiver = Some(rx);
        drop(tx);
        popup.poll_metadata(&egui::Context::default(), egui::ViewportId::ROOT);
        let metadata = popup.metadata.as_ref().unwrap();
        assert_eq!(metadata.version.as_deref(), Some("Browser 1.0"));
        assert_eq!(
            metadata
                .latest_package_version
                .as_ref()
                .map(|version| version.text.as_str()),
            Some("Unavailable (package lookup ended without a result)")
        );
        assert!(popup.receiver.is_none());
    }

    pub(super) fn window(id: &str) -> InfoTarget {
        InfoTarget::Window(WindowInfoPopup {
            window: WindowInfo {
                id: id.into(),
                title: "Same window title".into(),
                raw_title: "Same window title".into(),
                class: "test-app".into(),
                desktop_file_name: None,
                minimized: None,
                demands_attention: false,
                icon_path: None,
                active_process: None,
                exe_path: None,
                cwd_path: None,
                command_line: None,
                command_summary: None,
                geometry: None,
                process_chain: Vec::new(),
                tmux_pane: None,
                pid: None,
                last_activated_at_ms: None,
                activation_sequence: 0,
            },
            lookup: None,
            versions: None,
            tmux: None,
        })
    }

    #[test]
    fn terminal_update_target_requires_a_verified_outdated_terminal_window_owner() {
        let InfoTarget::Window(mut popup) = window("exact-kwin-window-id") else {
            unreachable!();
        };
        popup.window.pid = Some(176434);
        popup.window.class = "Xfce4-terminal".into();
        let mut node = ProcessInfoNode {
            identity: ProcessIdentity {
                pid: 176434,
                start_ticks: 42,
            },
            parent_pid: 3140,
            depth: 2,
            name: "xfce4-terminal".into(),
            script_name: None,
            header: "xfce4-terminal (pid 176434) [window owner]".into(),
            executable_error: None,
            cwd_error: None,
            raw_argv0: None,
            executable: Some("/fixture/xfce4-terminal (deleted)".into()),
            executable_identity: None,
            cwd: None,
            command_line: None,
        };
        for status in [None, Some(true), Some(false)] {
            assert_eq!(
                terminal_update_window(&popup.window, &node, status).as_deref(),
                (status == Some(false)).then_some("exact-kwin-window-id")
            );
        }
        node.identity.pid += 1;
        assert!(terminal_update_window(&popup.window, &node, Some(false)).is_none());
        node.identity.pid -= 1;
        for executable in [
            None,
            Some("/usr/bin/fish".into()),
            Some("/usr/bin/bash".into()),
        ] {
            node.executable = executable;
            assert!(terminal_update_window(&popup.window, &node, Some(false)).is_none());
        }
        node.executable = Some("/fixture/xfce4-terminal".into());
        popup.window.class = "alacritty".into();
        assert!(terminal_update_window(&popup.window, &node, Some(false)).is_none());
    }

    #[test]
    fn program_update_target_requires_an_outdated_exact_fish_or_codex_in_a_terminal() {
        let InfoTarget::Window(mut popup) = window("program-window") else {
            unreachable!();
        };
        popup.window.pid = Some(176434);
        popup.window.class = "Xfce4-terminal".into();
        let mut node = ProcessInfoNode {
            identity: ProcessIdentity {
                pid: 1234,
                start_ticks: 456,
            },
            parent_pid: 176434,
            depth: 3,
            name: "untrusted-title".into(),
            script_name: None,
            header: "program (pid 1234)".into(),
            executable_error: None,
            cwd_error: None,
            raw_argv0: None,
            executable: None,
            executable_identity: None,
            cwd: None,
            command_line: None,
        };
        for (name, application) in [
            ("fish", InfoProgram::Fish),
            ("codex", InfoProgram::Codex),
            ("codex-code-mode", InfoProgram::Codex),
        ] {
            for suffix in ["", " (deleted)"] {
                node.executable = Some(format!("/fixture/{name}{suffix}").into());
                for status in [None, Some(true), Some(false)] {
                    let expected = (status == Some(false)).then(|| InfoUpdateTarget::Program {
                        window: "program-window".into(),
                        process: node.identity,
                        application,
                    });
                    assert_eq!(
                        process_update_target(&popup.window, &node, status),
                        expected
                    );
                }
            }
        }
        for executable in [
            None,
            Some("/usr/bin/bash".into()),
            Some("/fixture/not-codex".into()),
        ] {
            node.executable = executable;
            assert!(process_update_target(&popup.window, &node, Some(false)).is_none());
        }
        node.executable = Some("/fixture/fish".into());
        for mutation in 0..4 {
            let mut window = popup.window.clone();
            let mut node = node.clone();
            match mutation {
                0 => window.id.clear(),
                1 => window.class = "another-terminal".into(),
                2 => node.identity.pid = 1,
                _ => node.identity.start_ticks = 0,
            }
            assert!(process_update_target(&window, &node, Some(false)).is_none());
        }
    }

    #[test]
    fn copyq_update_target_is_only_the_main_window_owner_independent_of_build_status() {
        let InfoTarget::Window(mut popup) = window("copyq-window") else {
            unreachable!();
        };
        popup.window.pid = Some(123);
        let mut node = ProcessInfoNode {
            identity: ProcessIdentity {
                pid: 123,
                start_ticks: 456,
            },
            parent_pid: 3140,
            depth: 2,
            name: "copyq".into(),
            script_name: None,
            header: "copyq (pid 123) [window owner]".into(),
            executable_error: None,
            cwd_error: None,
            raw_argv0: None,
            executable: Some("/fixture/copyq".into()),
            executable_identity: None,
            cwd: None,
            command_line: None,
        };
        for status in [None, Some(true), Some(false)] {
            for executable in ["/fixture/copyq", "/fixture/copyq (deleted)"] {
                node.executable = Some(executable.into());
                assert_eq!(
                    process_update_target(&popup.window, &node, status),
                    Some(InfoUpdateTarget::CopyQ(node.identity))
                );
            }
        }
        node.identity.pid += 1;
        assert!(process_update_target(&popup.window, &node, Some(false)).is_none());
        node.identity.pid -= 1;
        node.identity.start_ticks = 0;
        assert!(process_update_target(&popup.window, &node, Some(false)).is_none());
        node.identity.start_ticks = 456;
        for executable in [
            None,
            Some("/usr/bin/bash".into()),
            Some("/usr/bin/not-copyq".into()),
        ] {
            node.executable = executable;
            assert!(process_update_target(&popup.window, &node, Some(false)).is_none());
        }
    }

    #[test]
    fn window_and_console_info_follow_the_requested_field_order_without_duplicates() {
        let InfoTarget::Window(mut popup) = window("deduplicated-info") else {
            unreachable!();
        };
        popup.window.pid = Some(1234);
        popup.window.exe_path = Some("/usr/bin/test-app".into());
        let node = ProcessInfoNode {
            identity: ProcessIdentity {
                pid: 1234,
                start_ticks: 42,
            },
            parent_pid: 1,
            depth: 0,
            name: "test-app".into(),
            script_name: None,
            header: "test-app (pid 1234)".into(),
            executable_error: None,
            cwd_error: None,
            raw_argv0: None,
            executable: popup.window.exe_path.clone(),
            executable_identity: None,
            cwd: None,
            command_line: Some("test-app".into()),
        };
        let InfoTarget::App(application) = app("/test/test-app.desktop") else {
            unreachable!();
        };
        let version = crate::windows::versions::ProcessVersion {
            package_target: Some(crate::windows::versions::PackageTarget {
                name: "test-app".into(),
                inferred: false,
            }),
            ..Default::default()
        };
        for (version, pending) in [(None, true), (None, false), (Some(&version), false)] {
            for include_window in [false, true] {
                let mut versions = process_version_rows("Application", version, pending);
                append_desktop_entry_rows(
                    &mut versions,
                    desktop_entry_rows(Some(&application.app), Some(true), pending, None),
                );
                let mut rows = Vec::new();
                let mut expected = Vec::new();
                if include_window {
                    rows.extend(window_info_rows(&popup.window, Vec::new()));
                    expected.extend([
                        "Applicationlauncher title",
                        "Raw window title",
                        "Application key",
                        "Class",
                        "Window ID",
                        "Geometry",
                        "Minimized",
                        "Last activated",
                        "Activation sequence",
                    ]);
                }
                rows.extend(process_info_rows(&popup.window, &node, versions));
                expected.extend([
                    "Process name",
                    "PID",
                    "Parent PID",
                    "Process start (ticks since boot)",
                    "Working directory",
                    "Command line",
                    "Executable path",
                    VERSION_FIELD_LABEL,
                    LATEST_VERSION_FIELD_LABEL,
                    "Uncompiled changes",
                    "Type",
                    "Package",
                ]);
                if version.is_some() {
                    expected.push("Latest version (Pacman/AUR)");
                }
                expected.extend([
                    "Launch chain",
                    "Desktop file",
                    "Icon path",
                    "Name",
                    "Comment",
                    "Description",
                    "Capabilities",
                ]);
                assert_eq!(
                    rows.iter()
                        .map(|row| row.label.as_str())
                        .collect::<Vec<_>>(),
                    expected
                );
                for (label, expected) in [
                    ("PID", "1234"),
                    ("Executable path", "/usr/bin/test-app"),
                    ("Desktop file", "/test/test-app.desktop"),
                ] {
                    let matching: Vec<_> = rows.iter().filter(|row| row.label == label).collect();
                    assert_eq!(matching.len(), 1, "{label}");
                    assert_eq!(matching[0].value, expected);
                }
                assert!(rows.iter().all(|row| !row.separator_before));
                assert!(!rows.iter().any(|row| matches!(
                    row.label.as_str(),
                    "Command summary"
                        | "Installed package"
                        | "Process desktop file"
                        | "Window PID"
                        | "Window executable"
                        | "Window executable path"
                        | "Pinned"
                        | "Identification"
                )));
            }
        }
    }

    #[test]
    fn window_info_height_fits_loading_and_loaded_content_but_not_later_updates() {
        let ctx = egui::Context::default();
        let mut popups = InfoPopups::default();
        popups.open(window("height")).unwrap();
        let popup = &mut popups.items[0];
        let mut data = InfoPopupData {
            title: "Window Info".into(),
            heading: "nvtop - Terminal".into(),
            subtitle: "Process details".into(),
            scroll_sensitivity: 1.0,
            rows: Vec::new(),
            process_tree: None,
        };
        let mut measure = |ready, data: &InfoPopupData| {
            let mut size = [0.0; 2];
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                size = popup.window_initial_size(ctx, data, ready);
            });
            size
        };
        let loading = measure(false, &data);
        data.rows = (0..40)
            .map(|index| InfoPopupRow {
                label: format!("Metadata {index}"),
                scope: InfoFieldScope::Neutral,
                value: "Value".into(),
                searched: false,
                separator_before: false,
                binary_is_current: None,
            })
            .collect();
        assert_eq!(measure(false, &data), loading);
        let loaded = measure(true, &data);
        assert_eq!(loaded[0], 1200.0);
        assert!(loaded[1] > loading[1], "{loading:?} -> {loaded:?}");
        data.rows.clear();
        assert_eq!(measure(true, &data), loaded);
        assert_eq!(measure(false, &data), loaded);
    }

    #[test]
    fn show_info_keeps_distinct_windows_and_applications_open_independently() {
        let mut popups = InfoPopups::default();
        let first = popups.open(window("first")).unwrap();
        let second = popups.open(window("second")).unwrap();
        let app_a = popups.open(app("/test/a.desktop")).unwrap();
        let app_b = popups.open(app("/test/b.desktop")).unwrap();
        assert_eq!(HashSet::from([first, second, app_a, app_b]).len(), 4);
        assert_eq!(popups.items.len(), 4);
        let InfoTarget::Window(state) = &popups.items[0].target else {
            panic!("The first information window was replaced");
        };
        assert_eq!(state.window.id, "first");
        assert!(popups.close(second));
        assert_eq!(popups.items.len(), 3);
        assert_eq!(popups.items[0].id, first);
        assert!(popups.items.iter().any(|popup| popup.id == app_a));
        assert!(popups.items.iter().any(|popup| popup.id == app_b));
    }

    #[test]
    fn focusing_launcher_cancels_old_info_focus_without_closing_or_resetting_popups() {
        let mut popups = InfoPopups::default();
        let first = popups.open(window("first")).unwrap();
        let second = popups.open(app("/test/a.desktop")).unwrap();
        popups.items[0].initial_size = Some([1200.0, 620.0]);
        popups.cancel_focus_requests();
        assert_eq!(popups.items.len(), 2);
        assert_eq!(popups.items[0].id, first);
        assert_eq!(popups.items[1].id, second);
        assert_eq!(popups.items[0].initial_size, Some([1200.0, 620.0]));
        assert!(
            popups
                .items
                .iter()
                .all(|popup| !popup.focus_requested.load(Ordering::Relaxed))
        );

        assert_eq!(popups.open(window("first")).unwrap(), first);
        assert!(
            popups
                .items
                .last()
                .unwrap()
                .focus_requested
                .load(Ordering::Relaxed)
        );
    }

    #[test]
    fn reopening_same_info_target_preserves_state_and_requests_focus_only_on_demand() {
        let mut popups = InfoPopups::default();
        let first = popups.open(window("first")).unwrap();
        let focus = popups.items[0].focus_requested.clone();
        assert!(focus.swap(false, Ordering::Relaxed));
        let InfoTarget::Window(state) = &mut popups.items[0].target else {
            unreachable!();
        };
        state.window.title = "Retained snapshot".into();
        popups.open(window("second")).unwrap();
        assert!(!focus.load(Ordering::Relaxed));
        assert_eq!(popups.open(window("first")).unwrap(), first);
        assert_eq!(popups.items.len(), 2);
        let reopened = popups.items.last().unwrap();
        assert!(Arc::ptr_eq(&focus, &reopened.focus_requested));
        assert!(focus.load(Ordering::Relaxed));
        let InfoTarget::Window(state) = &reopened.target else {
            unreachable!();
        };
        assert_eq!(state.window.title, "Retained snapshot");
        popups.close_last();
        assert!(!popups.items.iter().any(|popup| popup.id == first));
    }

    #[test]
    fn reopening_same_application_reuses_its_info_window_and_keeps_loaded_metadata_and_size() {
        let mut popups = InfoPopups::default();
        let first = popups.open(app("/test/a.desktop")).unwrap();
        let focus = popups.items[0].focus_requested.clone();
        assert!(focus.swap(false, Ordering::Relaxed));
        let popup = &mut popups.items[0];
        popup.initial_size = Some([1200.0, 620.0]);
        popup.initial_size_ready = true;
        let InfoTarget::App(state) = &mut popup.target else {
            unreachable!()
        };
        state.metadata = Some(crate::windows::versions::ApplicationInfo {
            version: Some("Retained metadata".into()),
            ..Default::default()
        });
        popups.open(app("/test/b.desktop")).unwrap();
        assert_eq!(popups.open(app("/test/a.desktop")).unwrap(), first);
        assert_eq!(popups.items.len(), 2);
        let reopened = popups.items.last().unwrap();
        assert!(Arc::ptr_eq(&focus, &reopened.focus_requested));
        assert!(focus.load(Ordering::Relaxed));
        assert_eq!(reopened.initial_size, Some([1200.0, 620.0]));
        assert!(reopened.initial_size_ready);
        let InfoTarget::App(state) = &reopened.target else {
            unreachable!()
        };
        assert_eq!(
            state.metadata.as_ref().unwrap().version.as_deref(),
            Some("Retained metadata")
        );
    }

    #[test]
    fn delayed_info_close_events_cannot_close_a_reopened_target() {
        let mut popups = InfoPopups::default();
        let old = popups.open(window("first")).unwrap();
        let other = popups.open(window("second")).unwrap();
        assert!(popups.close(old));
        let reopened = popups.open(window("first")).unwrap();
        assert_ne!(old, reopened);
        assert!(!popups.close(old));
        assert_eq!(popups.items.len(), 2);
        assert!(popups.items.iter().any(|popup| popup.id == other));
        assert!(popups.items.iter().any(|popup| popup.id == reopened));
    }

    #[test]
    fn info_popup_limit_does_not_replace_existing_windows_or_prevent_refocusing() {
        let mut popups = InfoPopups::default();
        let first = popups.open(app("first.desktop")).unwrap();
        for index in 1..MAX_INFO_POPUPS {
            popups.open(app(&format!("{index}.desktop"))).unwrap();
        }
        assert!(popups.open(app("over-limit.desktop")).is_err());
        assert_eq!(popups.items.len(), MAX_INFO_POPUPS);
        assert_eq!(popups.open(app("first.desktop")).unwrap(), first);
        assert!(popups.close(first));
        assert!(popups.open(app("over-limit.desktop")).is_ok());
    }
}
