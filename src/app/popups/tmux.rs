use super::*;
use crate::windows::tmux_info::{Inventory, Lookup, Target};
use serde_json::Value;

mod client;
mod server;
use client::rows as client_rows;

pub(super) fn process_rows(window: &WindowInfo, node: &ProcessInfoNode) -> Vec<InfoPopupRow> {
    let Some(pane) = &window.tmux_pane else {
        return Vec::new();
    };
    let client = node.identity.pid == pane.client_pid;
    if !client && node.identity.pid != pane.session.server_pid {
        return Vec::new();
    }
    let mut rows = vec![
        row("Tmux role", if client { "Client" } else { "Server" }.into()),
        row("Tmux listener", pane.session.socket_path.clone()),
    ];
    if client {
        rows.extend([
            row("Tmux server PID", pane.session.server_pid.to_string()),
            row("Tmux session", pane.session.session_name.clone()),
            row("Tmux session ID", pane.session.session_id.clone()),
            row("Tmux active pane", pane.pane_id.clone()),
            row("Tmux pane title", pane.title.clone()),
            row("Tmux pane PID", pane.pane_pid.to_string()),
            row("Tmux pane directory", display_path(&pane.cwd)),
            row("Tmux foreground program", pane.process_name.clone()),
            row("Tmux foreground PID", pane.process_pid.to_string()),
        ]);
        rows.iter_mut()
            .find(|row| row.label == "Tmux session")
            .unwrap()
            .searched = true;
    }
    rows
}

impl WindowInfoPopup {
    pub(super) fn poll_tmux(&mut self, ctx: &egui::Context, viewport: egui::ViewportId) {
        let Some(snapshot) = self
            .lookup
            .as_ref()
            .and_then(|lookup| lookup.result.as_deref())
        else {
            return;
        };
        let target = Target::for_window(&self.window, snapshot);
        if self
            .tmux
            .as_ref()
            .is_some_and(|lookup| Some(&lookup.target) != target.as_ref())
        {
            self.tmux = None;
        }
        let Some(target) = target else {
            return;
        };
        let requested = requested(ctx, viewport, &self.window, snapshot, &target);
        if requested && self.tmux.is_none() {
            self.tmux = Some(Lookup::start(target, ctx.clone(), viewport));
        }
    }
}

fn requested(
    ctx: &egui::Context,
    viewport: egui::ViewportId,
    window: &WindowInfo,
    snapshot: &ProcessTreeSnapshot,
    target: &Target,
) -> bool {
    snapshot.nodes.iter().any(|node| {
        let related = node.identity.pid == target.server.pid
            || window
                .tmux_pane
                .as_ref()
                .is_some_and(|pane| pane.client_pid == node.identity.pid);
        related
            && process_info_section_is_open(
                ctx,
                process_info_section_id((viewport, &window.id), node.identity),
                false,
            )
    })
}

fn row(label: &str, value: String) -> InfoPopupRow {
    scoped_row(InfoFieldScope::InstanceContext, label, value)
}

fn scoped_row(scope: InfoFieldScope, label: &str, value: String) -> InfoPopupRow {
    InfoPopupRow {
        label: label.into(),
        value,
        scope,
        searched: false,
        separator_before: false,
        binary_is_current: None,
    }
}

fn label(key: &str) -> String {
    let known = match key {
        "pid" => "PID",
        "started_ticks" => "Process start (ticks since boot)",
        "tty" => "TTY",
        "id" | "session_id" => "Session ID",
        "created_at" => "Created",
        "last_activity_at" => "Last input activity",
        "activity_idle_seconds" => "Input idle (seconds)",
        "idle_seconds" => "Network packet age (seconds)",
        "network_last_seen_at" => "Last network packet",
        "network_last_rx_monotonic_ms" => "Last network packet (monotonic ms)",
        "attachment" => "Attachment state",
        "state" => "Transport state",
        "reachability" => "Network reachability",
        "frontend" => "Connection frontend",
        "frontend_pid" => "Frontend PID",
        "frontend_started_ticks" => "Frontend start (ticks since boot)",
        "frontend_binary_mtime" => "Frontend binary modified",
        "frontend_version" => "Frontend running version",
        "frontend_outdated" => "Frontend outdated",
        "pane_pid" => "Pane PID",
        "window_id" => "Tmux window ID",
        "width" => "Width (columns)",
        "height" => "Height (rows)",
        "flags" => "Client flags",
        "sizing" => "Sizing participation",
        "sizing_policy" => "Sizing policy",
        "sizing_client_pid" => "Sizing controlling client PID",
        "active_sizing" => "Controls window sizing",
        "tmux_client_socket" => "Tmux client socket",
        "tmux_server_socket" => "Tmux server connected socket",
        "network_local_socket" => "Network local socket",
        "network_peer_socket" => "Network peer socket",
        "app" => "Foreground executable",
        "app_label" => "Foreground application",
        "app_pid" => "Foreground PID",
        "command" => "Foreground command line",
        "opsec_connection" => "Guest connection",
        "sha256" => "SHA256",
        _ => "",
    };
    if !known.is_empty() {
        return known.into();
    }
    let mut value = key.replace('_', " ");
    if let Some(first) = value.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    value
}

fn value(key: &str, value: &Value) -> String {
    if (key.ends_with("_at") || key.ends_with("_mtime"))
        && let Some(seconds) = value
            .as_f64()
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0 && *seconds < 253_402_300_800.0)
    {
        return format!("{} UTC", format_utc_timestamp((seconds * 1000.0) as i64));
    }
    match value {
        Value::Null => "Unavailable".into(),
        Value::String(text) => text.replace('\r', "\\r").replace('\n', "\\n"),
        Value::Array(items) if items.is_empty() => "-".into(),
        Value::Object(items) if items.is_empty() => "-".into(),
        value => value.to_string(),
    }
}

fn field_scope(key: &str, inherited: InfoFieldScope) -> InfoFieldScope {
    if inherited == InfoFieldScope::Installation {
        return inherited;
    }
    // Inventory objects mix connection context with build records, including guest clients.
    match key {
        "pid" | "parent_pid" | "ppid" | "started_ticks" | "cwd" | "working_directory" => {
            InfoFieldScope::RunningInstance
        }
        "frontend_pid"
        | "frontend_started_ticks"
        | "pane_pid"
        | "app_pid"
        | "command"
        | "tty"
        | "socket" => InfoFieldScope::InstanceContext,
        "version"
        | "sha256"
        | "build_id"
        | "elf_build_id"
        | "binary_mtime"
        | "outdated"
        | "build_unknown"
        | "frontend_version"
        | "frontend_binary_mtime"
        | "frontend_outdated"
        | "mosh_client_version"
        | "mosh_client_build"
        | "mosh_client_outdated"
        | "app_build"
        | "server_binary_mtime"
        | "tmux_binary_mtime" => InfoFieldScope::RunningInstance,
        _ => inherited,
    }
}

fn fields(
    rows: &mut Vec<InfoPopupRow>,
    prefix: &str,
    data: &Value,
    scope: InfoFieldScope,
    depth: usize,
) {
    if let Some(object) = data.as_object().filter(|_| depth < 8) {
        for (key, data) in object {
            if matches!(key.as_str(), "app_outdated" | "app_build_unknown") {
                continue;
            }
            let scope = field_scope(key, scope);
            let name = if prefix.is_empty() {
                label(key)
            } else {
                format!("{prefix} / {}", label(key))
            };
            if data.is_object()
                || data.is_array() && data.as_array().unwrap().iter().any(Value::is_object)
            {
                fields(rows, &name, data, scope, depth + 1);
            } else {
                rows.push(scoped_row(scope, &name, value(key, data)));
            }
        }
    } else if let Some(array) = data.as_array().filter(|_| depth < 8) {
        for (index, data) in array.iter().enumerate() {
            fields(
                rows,
                &format!("{prefix} [{}]", index + 1),
                data,
                scope,
                depth + 1,
            );
        }
    } else {
        rows.push(scoped_row(scope, prefix, value("", data)));
    }
}

fn section(
    ctx: &egui::Context,
    id: egui::Id,
    header: String,
    default_open: bool,
    rows: impl FnOnce() -> Vec<InfoPopupRow>,
) -> InfoMetadataSection {
    InfoMetadataSection {
        id,
        header: header.into(),
        header_scope: None,
        field_header: None,
        binary_is_current: None,
        default_open,
        rows: if process_info_section_is_open(ctx, id, default_open) {
            rows()
        } else {
            Vec::new()
        },
        children: Vec::new(),
    }
}

fn inventory(lookup: Option<&Lookup>) -> Result<Arc<Inventory>, String> {
    match lookup.and_then(Lookup::result) {
        Some(Ok(inventory)) => Ok(inventory),
        Some(Err(error)) => Err(format!("Unavailable: {error}")),
        None if lookup.is_none() => {
            Err("Unavailable: the tmux server process identity could not be captured".into())
        }
        None => Err("Loading read-only tmux-mosh inventory...".into()),
    }
}

pub(super) fn merge_process_rows(
    rows: &mut Vec<InfoPopupRow>,
    versions: &mut Vec<InfoPopupRow>,
    window: &WindowInfo,
    node: &ProcessInfoNode,
    lookup: Option<&Lookup>,
) {
    if window
        .tmux_pane
        .as_ref()
        .is_some_and(|pane| pane.session.server_pid == node.identity.pid)
    {
        server::merge(rows, versions, window, node, lookup);
        server::place_context_after_desktop(rows, versions);
    }
}

fn merge_client_rows(
    rows: &mut Vec<InfoPopupRow>,
    window: &WindowInfo,
    node: &ProcessInfoNode,
    lookup: Option<&Lookup>,
    shown: &[ProcessInfoNode],
) -> Vec<InfoPopupRow> {
    if window
        .tmux_pane
        .as_ref()
        .is_none_or(|pane| pane.client_pid != node.identity.pid)
    {
        return Vec::new();
    }
    let extra = inventory(lookup)
        .map(|inventory| {
            inventory
                .client(node.identity)
                .map(|client| client_rows(&inventory, client, true, shown))
                .unwrap_or_else(|| {
                    vec![row(
                        "Tmux inventory",
                        "Unavailable: no unique client record matches this PID and start time"
                            .into(),
                    )]
                })
        })
        .unwrap_or_else(|error| vec![row("Tmux inventory", error)]);
    let mut installation = Vec::new();
    for mut field in extra {
        if field.scope == InfoFieldScope::Installation {
            installation.push(field);
            continue;
        }
        field.label = match field.label.as_str() {
            "Session" => "Tmux session",
            "Session ID" => "Tmux session ID",
            "Pane PID" => "Tmux pane PID",
            "Foreground PID" => "Tmux foreground PID",
            "Foreground application" => "Tmux foreground program",
            "Foreground executable" => "Tmux foreground executable",
            "Foreground command line" => "Tmux foreground command line",
            _ => &field.label,
        }
        .to_owned();
        if let Some(existing) = rows
            .iter_mut()
            .find(|existing| existing.label == field.label)
        {
            if field.value != "Unavailable" {
                existing.value = field.value;
            }
        } else {
            rows.push(field);
        }
    }
    installation
}

pub(super) fn sections(
    ctx: &egui::Context,
    id: egui::Id,
    window: &WindowInfo,
    node: &ProcessInfoNode,
    lookup: Option<&Lookup>,
    shown: &[ProcessInfoNode],
) -> Vec<InfoMetadataSection> {
    let Some(pane) = &window.tmux_pane else {
        return Vec::new();
    };
    if node.identity.pid == pane.client_pid {
        let name = inventory(lookup)
            .ok()
            .and_then(|inventory| {
                inventory
                    .client(node.identity)
                    .and_then(|client| client["session"].as_str())
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| pane.session.session_name.clone());
        let header = format!("Session  {name}");
        let mut session = section(
            ctx,
            id.with("tmux-client-session"),
            header.clone(),
            false,
            || {
                let mut rows = connection_rows(window, node);
                let installed = merge_client_rows(&mut rows, window, node, lookup, shown);
                rows.extend(installed);
                rows
            },
        );
        session.field_header = Some(InfoMetadataFieldHeader {
            value_range: "Session  ".len()..header.len(),
            label_scope: InfoFieldScope::InstanceContext,
            after_row: Some("Desktop file"),
        });
        return vec![session];
    }
    let server = node.identity.pid == pane.session.server_pid;
    if !server {
        return Vec::new();
    }
    if lookup.is_none_or(|lookup| !server::matches(node, pane, lookup)) {
        return Vec::new();
    }
    let Ok(inventory) = inventory(lookup) else {
        return Vec::new();
    };
    let mut sections = Vec::new();
    for session in inventory.sessions() {
        let identity = session["id"].as_str().unwrap_or("?");
        let name = session["name"].as_str().unwrap_or("Unavailable");
        sections.push(section(
            ctx,
            id.with(("tmux-session", identity)),
            format!("Tmux session: {name} ({identity})"),
            false,
            || {
                let mut rows = Vec::new();
                fields(&mut rows, "", session, InfoFieldScope::InstanceContext, 0);
                rows
            },
        ));
    }
    for client in inventory.clients() {
        let pid = client["pid"].as_i64();
        let started = client["started_ticks"].as_u64();
        let session = client["session"].as_str().unwrap_or("Unavailable");
        sections.push(section(
            ctx,
            id.with(("tmux-attached-client", pid, started)),
            format!(
                "Attached tmux client: {} - {session}",
                value("pid", &client["pid"])
            ),
            false,
            || client_rows(&inventory, client, false, shown),
        ));
    }
    sections
}

#[cfg(test)]
mod tests;
