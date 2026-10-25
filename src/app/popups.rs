use super::*;
use crate::models::InfoMetadataSection;
use std::sync::atomic::AtomicBool;

const MAX_INFO_POPUPS: usize = 32;

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
    metadata: Option<crate::windows::versions::ProgsMetadata>,
    receiver: Option<std::sync::mpsc::Receiver<crate::windows::versions::ProgsMetadata>>,
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
        if self.metadata.is_some() {
            return;
        }
        let receiver = self.receiver.get_or_insert_with(|| {
            let exec = self.app.exec.clone();
            let cancelled = self.cancelled.clone();
            let ctx = ctx.clone();
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            applicationlauncher::observability::spawn_named("app-info", move |worker| {
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                worker.set_state("reading-progs-metadata");
                let metadata = crate::windows::versions::progs_metadata(&exec);
                if !cancelled.load(Ordering::Acquire) {
                    let _ = tx.try_send(metadata);
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    ctx.request_repaint_of(viewport_id);
                }
            });
            rx
        });
        match receiver.try_recv() {
            Ok(metadata) => self.metadata = Some(metadata),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.metadata = Some(Default::default());
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if self.metadata.is_some() {
            self.receiver = None;
        }
    }
}

impl Drop for AppInfoPopup {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

fn application_info_rows(state: &AppInfoPopup, is_pinned: bool) -> Vec<InfoPopupRow> {
    let app = &state.app;
    let path = executable_path_from_exec(&app.exec);
    let unavailable = || {
        if state.metadata.is_some() {
            "Unavailable"
        } else {
            "Loading..."
        }
        .to_owned()
    };
    let row = |label: &str, value: String, searched: bool| InfoPopupRow {
        label: label.into(),
        value,
        searched,
        separator_before: false,
        binary_is_current: None,
    };
    vec![
        row("Name", app.name.clone(), true),
        row(
            "Executable basename",
            command_basename(&app.exec).unwrap_or_else(|| "Unavailable".into()),
            true,
        ),
        row(
            "Desktop file stem",
            app.desktop_file_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Unavailable")
                .into(),
            true,
        ),
        row(
            "Comment",
            app.comment.clone().unwrap_or_else(|| "Unavailable".into()),
            true,
        ),
        row(
            "Capabilities",
            state
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.capabilities.clone())
                .unwrap_or_else(unavailable),
            false,
        ),
        row(
            "Description",
            state
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.description.clone())
                .unwrap_or_else(unavailable),
            false,
        ),
        row("Cleaned command", clean_exec_cmd(&app.exec), true),
        row(
            "Executable path",
            path.as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Unavailable".into()),
            false,
        ),
        row(
            "Executable exists",
            path.as_ref().is_some_and(|path| path.is_file()).to_string(),
            false,
        ),
        row("Raw Exec", app.exec.clone(), false),
        row(
            "Desktop file",
            app.desktop_file_path.to_string_lossy().into_owned(),
            false,
        ),
        row(
            "Desktop file exists",
            app.desktop_file_path.exists().to_string(),
            false,
        ),
        row(
            "Icon path",
            app.icon_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Unavailable".into()),
            false,
        ),
        row("Pinned", is_pinned.to_string(), false),
        row(
            "System settings module",
            app.is_settings_module.to_string(),
            false,
        ),
    ]
}

struct WindowInfoPopup {
    window: WindowInfo,
    lookup: Option<WindowProcessTreeLookup>,
    versions: Option<ProcessVersionCache>,
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

fn latest_package_version_field(
    latest_package_version: Option<&str>,
    has_package_target: bool,
) -> Option<(&'static str, String)> {
    has_package_target.then(|| {
        (
            "latest package version (Pacman/AUR)",
            latest_package_version
                .unwrap_or("Checking Pacman/AUR...")
                .to_owned(),
        )
    })
}

pub(super) fn process_version_rows(
    role: &str,
    version: Option<&crate::windows::versions::ProcessVersion>,
    pending: bool,
) -> Vec<InfoPopupRow> {
    let package_field = latest_package_version_field(
        version.and_then(|version| version.latest_package_version.as_deref()),
        version.is_some_and(|version| version.package_target.is_some()),
    );
    [
        (
            "version",
            version.and_then(|version| version.version.clone()),
        ),
        (
            "latest version (on disk)",
            version.and_then(|version| version.latest_version.clone()),
        ),
    ]
    .into_iter()
    .chain(package_field.map(|(label, value)| (label, Some(value))))
    .chain([
        (
            "uncompiled changes",
            version.and_then(|version| version.uncompiled_changes.clone()),
        ),
        (
            "installation",
            version.and_then(|version| {
                version
                    .installation
                    .as_ref()
                    .map(|installation| match version.package.as_deref() {
                        Some(package) => format!("{installation}; installed package: {package}"),
                        None => installation.clone(),
                    })
            }),
        ),
        (
            "capabilities",
            version.and_then(|version| version.progs_metadata.capabilities.clone()),
        ),
        (
            "description",
            version.and_then(|version| version.progs_metadata.description.clone()),
        ),
        (
            "ELF build ID",
            version.and_then(|version| version.build_id.clone()),
        ),
        (
            "latest ELF build ID (on disk)",
            version.and_then(|version| version.latest_build_id.clone()),
        ),
    ])
    .map(|(suffix, value)| {
        let binary_is_current = if suffix == "ELF build ID"
            && value.as_deref().is_some_and(|id| !id.trim().is_empty())
        {
            version.and_then(|version| version.running_latest_binary)
        } else {
            None
        };
        InfoPopupRow {
            label: format!("{role} {suffix}"),
            value: value.unwrap_or_else(|| {
                if matches!(suffix, "version" | "ELF build ID")
                    && let Some(reason) = version.and_then(|version| version.running_unavailable)
                {
                    format!("Unavailable ({reason})")
                } else {
                    if pending { "Loading..." } else { "Unavailable" }.to_owned()
                }
            }),
            searched: false,
            separator_before: false,
            binary_is_current,
        }
    })
    .collect()
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
    {
        if let Some(installation) =
            version.and_then(|version| version.application_installation.as_ref())
        {
            rows.push(InfoPopupRow {
                label: format!("{role} installation"),
                value: installation.clone(),
                searched: false,
                separator_before: false,
                binary_is_current: None,
            });
        }
        for (label, value) in [
            (
                "capabilities",
                version.and_then(|version| version.application_progs_metadata.capabilities.clone()),
            ),
            (
                "description",
                version.and_then(|version| version.application_progs_metadata.description.clone()),
            ),
        ] {
            rows.push(InfoPopupRow {
                label: format!("{role} {label}"),
                value: value.unwrap_or_else(|| "Unavailable".into()),
                searched: false,
                separator_before: false,
                binary_is_current: None,
            });
        }
    }
    if let Some(application) = version.and_then(|version| version.application.as_ref()) {
        let build = version.and_then(|version| version.application_build.as_ref());
        let package_field = latest_package_version_field(
            version.and_then(|version| version.application_latest_package_version.as_deref()),
            version.is_some_and(|version| version.application_package_target.is_some()),
        );
        let row = |label: &str, value: String| InfoPopupRow {
            label: format!("{role} {label}"),
            value,
            searched: false,
            separator_before: false,
            binary_is_current: None,
        };
        for (label, value) in [
            ("package", application.package.clone()),
            (
                "running version",
                build
                    .and_then(|build| build.running_version.clone())
                    .unwrap_or_else(|| {
                        "Unknown (installed metadata does not prove the loaded version)".to_owned()
                    }),
            ),
            ("installed version", application.installed_version.clone()),
        ]
        .into_iter()
        .chain(package_field)
        {
            rows.push(row(label, value));
        }
        if application.package == "@earendil-works/pi-coding-agent" {
            rows.push(row(
                "running entry point",
                build
                    .and_then(|build| build.running_entry.as_ref())
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "Unknown".into()),
            ));
        }
        rows.push(row(
            "installed entry point",
            application.entry_point.display().to_string(),
        ));
        if application.package == "@earendil-works/pi-coding-agent" {
            rows.extend(pi_build_rows(role, build));
        }
        rows.push(row(
            "installed package manifest",
            application.manifest.display().to_string(),
        ));
        rows.push(row("identification", application.identification.to_owned()));
    }
    if let Some(application) = version.and_then(|version| version.script_application.as_ref()) {
        let package_field = latest_package_version_field(
            version.and_then(|version| version.application_latest_package_version.as_deref()),
            version.is_some_and(|version| version.application_package_target.is_some()),
        );
        for (label, value) in [
            ("kind", application.kind.to_owned()),
            ("launch target", application.target.clone()),
            (
                "entry point",
                application
                    .entry_point
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "Unknown (module/class file not inferred)".into()),
            ),
            ("identification", application.identification.to_owned()),
            (
                "package",
                version
                    .and_then(|version| version.application_package_target.as_ref())
                    .map(|package| package.name.clone())
                    .unwrap_or_else(|| {
                        "Unavailable (not inferred from the runtime package)".into()
                    }),
            ),
        ]
        .into_iter()
        .chain(package_field)
        .chain([
            (
                "running version",
                "Unknown (no application build identity published)".into(),
            ),
            (
                "build status",
                "Unknown (no application build identity published)".into(),
            ),
        ]) {
            rows.push(InfoPopupRow {
                label: format!("{role} {label}"),
                value,
                searched: false,
                separator_before: false,
                binary_is_current: None,
            });
        }
    } else if version.is_some_and(|version| version.application.is_none()) {
        rows.push(InfoPopupRow {
            label: format!("{role} identification"),
            value: "Unknown (no unambiguous script, module or package identified)".into(),
            searched: false,
            separator_before: false,
            binary_is_current: None,
        });
    }
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
    let application_rows = if process_info_section_is_open(ctx, app_id, true) && version.is_none() {
        vec![InfoPopupRow {
            label: "Application identification".into(),
            value: "Loading...".into(),
            searched: false,
            separator_before: false,
            binary_is_current: None,
        }]
    } else if process_info_section_is_open(ctx, app_id, true) {
        application_version_rows("Application", version, version.is_none())
    } else {
        Vec::new()
    };
    let runtime_rows = if process_info_section_is_open(ctx, runtime_id, true) {
        let mut rows = vec![InfoPopupRow {
            label: "Runtime executable path".into(),
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
            default_open: true,
            rows: application_rows,
        },
        InfoMetadataSection {
            id: runtime_id,
            header: format!("Runtime: {runtime}").into(),
            default_open: true,
            rows: runtime_rows,
        },
    ]
}

fn pi_build_rows(
    role: &str,
    build: Option<&crate::windows::versions::PiBuildEvidence>,
) -> Vec<InfoPopupRow> {
    let empty = crate::windows::versions::PiBuildEvidence::default();
    let build = build.unwrap_or(&empty);
    [
        ("running code fingerprint", build.running_sdk.clone()),
        (
            "latest installed code fingerprint",
            build.latest_sdk.clone(),
        ),
        (
            "running extensions fingerprint",
            build.running_extensions.clone(),
        ),
        (
            "latest extensions fingerprint",
            build.latest_extensions.clone(),
        ),
        ("installed patch verification", build.patches.clone()),
        (
            "loaded extension roots",
            build.extension_count.map(|count| count.to_string()),
        ),
        (
            "build comparison checked",
            build.checked_at_ms.map(|when| {
                format!(
                    "Unix {when} ms ({} s old)",
                    build.check_age_seconds.unwrap_or(0)
                )
            }),
        ),
        (
            "build comparison reason",
            Some(if build.reason.is_empty() {
                "No startup build receipt available".into()
            } else {
                build.reason.clone()
            }),
        ),
        ("fingerprint coverage", build.scope.clone()),
        (
            "startup build receipt",
            build
                .record_path
                .as_ref()
                .map(|path| path.display().to_string()),
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
    .map(|(label, value)| {
        let binary_is_current = if matches!(
            label,
            "running code fingerprint" | "latest extensions fingerprint"
        ) && value
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            match build.status {
                crate::windows::versions::PiBuildStatus::Current => Some(true),
                crate::windows::versions::PiBuildStatus::RestartRequired
                | crate::windows::versions::PiBuildStatus::InstallationUpdateRequired => {
                    Some(false)
                }
                crate::windows::versions::PiBuildStatus::Unknown => None,
            }
        } else {
            None
        };
        InfoPopupRow {
            label: format!("{role} {label}"),
            value: value.unwrap_or_else(|| "Unknown".into()),
            searched: false,
            separator_before: false,
            binary_is_current,
        }
    })
    .collect()
}

pub(super) fn window_info_rows(
    window: &WindowInfo,
    desktop_file_path: String,
    window_version_rows: Vec<InfoPopupRow>,
) -> Vec<InfoPopupRow> {
    let app_key = window_application_key(window);
    let exe_basename = window
        .exe_path
        .as_ref()
        .and_then(|path| path.file_name().and_then(|name| name.to_str()))
        .unwrap_or("Unavailable")
        .to_string();
    let class_is_searched = !window.class.eq_ignore_ascii_case(&app_key);
    let row = |label: &str, value: String, searched: bool| InfoPopupRow {
        label: label.to_string(),
        value,
        searched,
        separator_before: false,
        binary_is_current: None,
    };

    let mut rows = vec![
        row("Applicationlauncher title", window.title.clone(), true),
        row("Raw window title", window.raw_title.clone(), false),
        row("Application key", app_key, true),
        row("Window ID", window.id.clone(), false),
        row("Class", window.class.clone(), class_is_searched),
        row("Window desktop file", desktop_file_path, false),
        row(
            "Window PID",
            window
                .pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "Unavailable".to_string()),
            false,
        ),
        row("Window executable", exe_basename, true),
        row(
            "Window executable path",
            window
                .exe_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|| "Unavailable".to_string()),
            false,
        ),
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
    desktop_file: String,
    version_rows: Vec<InfoPopupRow>,
) -> Vec<InfoPopupRow> {
    let active = window.active_process.is_some()
        && window
            .process_chain
            .first()
            .is_some_and(|entry| entry.pid == node.identity.pid);
    let row = |label: &str, value: String, searched: bool| InfoPopupRow {
        label: label.to_string(),
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
            "Executable path",
            node.executable
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "Unavailable".into()),
            false,
        ),
        row("Desktop file", desktop_file, false),
        row(
            "Working directory",
            node.cwd
                .as_deref()
                .map(display_path)
                .unwrap_or_else(|| "Unavailable".into()),
            active && node.cwd == window.cwd_path,
        ),
        row(
            "Command summary",
            node.command_summary
                .clone()
                .unwrap_or_else(|| "Unavailable".into()),
            active && node.command_summary == window.command_summary,
        ),
        row(
            "Command line",
            node.command_line
                .clone()
                .unwrap_or_else(|| "Unavailable".into()),
            active && node.command_line == window.command_line,
        ),
        row(
            "Process start (ticks since boot)",
            node.identity.start_ticks.to_string(),
            false,
        ),
    ];
    rows.extend(version_rows);
    if let Some(pane) = &window.tmux_pane
        && [pane.client_pid, pane.session.server_pid].contains(&node.identity.pid)
    {
        rows.extend([
            row("Tmux session", pane.session.session_name.clone(), true),
            row("Tmux socket", pane.session.socket_path.clone(), false),
            row("Tmux client PID", pane.client_pid.to_string(), false),
            row("Tmux active pane", pane.pane_id.clone(), false),
            row("Tmux pane title", pane.title.clone(), false),
            row("Tmux pane PID", pane.pane_pid.to_string(), false),
            row(
                "Tmux server PID",
                pane.session.server_pid.to_string(),
                false,
            ),
            row("Tmux pane directory", display_path(&pane.cwd), false),
        ]);
    }
    rows
}

impl App {
    fn open_info_popup(&mut self, ctx: &egui::Context, target: InfoTarget) {
        match self.info_popups.open(target) {
            Ok(id) => {
                // Wake an existing minimized viewport; a new viewport also focuses on its first frame.
                if !ctx.embed_viewports() {
                    ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
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
                let close_requested = ctx.input(|input| {
                    input.viewport().close_requested()
                        || input.key_pressed(egui::Key::Escape)
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
        let desktop_file_path = self
            .desktop_file_path_for_window(window_info)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unavailable".to_string());
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
            let mut metadata_sections = Vec::new();
            let rows = if open {
                let application_open =
                    process_info_section_is_open(ctx, id.with("hosted-application"), true);
                let version = popup
                    .versions
                    .as_ref()
                    .and_then(|cache| cache.request_expanded(node, application_open));
                details_ready &= version.is_some();
                if application_open
                    && version.as_ref().is_some_and(|version| {
                        version.application.as_ref().is_some_and(|application| {
                            application.package == "@earendil-works/pi-coding-agent"
                        })
                    })
                {
                    // Repaint only an expanded Pi section. This schedules bounded receipt reads,
                    // not ELF, package-manager or whole-SDK probes, and stops when collapsed.
                    ctx.request_repaint_after(crate::windows::versions::PI_RECEIPT_REFRESH);
                }
                metadata_sections = hosted_metadata_sections(ctx, id, node, version.as_deref());
                let versions = if metadata_sections.is_empty() {
                    application_version_rows("Application", version.as_deref(), version.is_none())
                } else {
                    Vec::new()
                };
                if owner {
                    window_info_rows(window_info, desktop_file_path.clone(), versions)
                } else {
                    let desktop_file = self
                        .desktop_file_path_for_process(&node.name)
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "Unavailable".into());
                    process_info_rows(window_info, node, desktop_file, versions)
                }
            } else {
                Vec::new()
            };
            sections.push(InfoProcessSection {
                id,
                depth: node.depth,
                header: node.header.clone(),
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
                rows: window_info_rows(
                    window_info,
                    desktop_file_path,
                    process_version_rows("Application", None, snapshot.is_none()),
                ),
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
        let is_pinned = self.pinned_apps.contains(&app_info.desktop_file_path);

        InfoPopupData {
            title: format!("Application Info: {}", app_info.name),
            heading: app_info.name.clone(),
            subtitle: "Installed application, launch chain and desktop-entry metadata".to_string(),
            scroll_sensitivity: self.info_scroll_sensitivity,
            rows: application_info_rows(state, is_pinned),
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
                    crate::popups::render_info_popup_scroll_area(ui, &data);
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

    fn window(id: &str) -> InfoTarget {
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
        })
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
