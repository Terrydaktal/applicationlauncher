use super::*;

pub(super) fn matches(
    node: &ProcessInfoNode,
    pane: &applicationlauncher::tracker::LiveTmuxPane,
    lookup: &Lookup,
) -> bool {
    lookup.target.server == node.identity
        && lookup.target.socket == Path::new(&pane.session.socket_path)
}

fn insert_after(rows: &mut Vec<InfoPopupRow>, after: &str, field: InfoPopupRow) {
    let position = rows
        .iter()
        .position(|row| row.label == after)
        .map(|index| index + 1)
        .unwrap_or_else(|| {
            rows.iter()
                .position(|row| row.scope == InfoFieldScope::Installation)
                .unwrap_or(rows.len())
        });
    rows.insert(position, field);
}

pub(super) fn merge(
    rows: &mut Vec<InfoPopupRow>,
    versions: &mut Vec<InfoPopupRow>,
    window: &WindowInfo,
    node: &ProcessInfoNode,
    lookup: Option<&Lookup>,
) {
    let pane = window.tmux_pane.as_ref().unwrap();
    if lookup.is_some_and(|lookup| !matches(node, pane, lookup)) {
        rows.push(row(
            "Tmux inventory",
            "Unavailable: no inventory matches this server PID, start time and listener".into(),
        ));
        return;
    }
    let inventory = match inventory(lookup) {
        Ok(inventory) => inventory,
        Err(error) => {
            rows.push(row("Tmux inventory", error));
            return;
        }
    };
    rows.extend([
        row("Host", value("hostname", &inventory.data["hostname"])),
        row(
            "Snapshot captured",
            value("snapshot_at", &inventory.data["snapshot_at"]),
        ),
        row(
            "Sizing monitor running",
            value("monitor_running", &inventory.data["monitor_running"]),
        ),
        row("Session count", inventory.sessions().len().to_string()),
        row("Client count", inventory.clients().len().to_string()),
        row(
            "Running server binary modified",
            value(
                "server_binary_mtime",
                &inventory.data["server_binary_mtime"],
            ),
        ),
    ]);
    for (key, label, after, scope) in [
        (
            "tmux",
            "SHA256",
            VERSION_FIELD_LABEL,
            InfoFieldScope::RunningInstance,
        ),
        (
            "tmux_latest",
            "Latest SHA256 (on disk)",
            LATEST_VERSION_FIELD_LABEL,
            InfoFieldScope::Installation,
        ),
    ] {
        let mut build = inventory.data["servers"][key].clone();
        if build.is_null() {
            continue;
        }
        let mut fingerprint = row(label, value("sha256", &build["sha256"]));
        fingerprint.scope = scope;
        insert_after(versions, after, fingerprint);
        if let Some(object) = build.as_object_mut() {
            // Version/identity/status already belong to the main process metadata.
            for key in [
                "pid",
                "started_ticks",
                "socket",
                "version",
                "outdated",
                "sha256",
            ] {
                object.remove(key);
            }
        }
        if scope == InfoFieldScope::RunningInstance {
            fields(rows, "Running server", &build, scope, 0);
        } else {
            fields(versions, "Latest installed tmux", &build, scope, 0);
        }
    }
    if let Some(note) = inventory.data["note"]
        .as_str()
        .filter(|note| !note.is_empty())
    {
        rows.push(row("Inventory note", note.replace('\n', "\\n")));
    }
}
