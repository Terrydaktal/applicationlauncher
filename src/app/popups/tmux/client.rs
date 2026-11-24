use super::*;

fn text(data: &Value) -> Option<&str> {
    data.as_str()
        .filter(|text| !text.is_empty() && *text != "-")
}

fn endpoint(address: Option<&str>, port: &Value, protocol: Option<&str>) -> Option<String> {
    let port = port.as_u64();
    if address.is_none() && port.is_none() && protocol.is_none() {
        return None;
    }
    let address = address.unwrap_or("?");
    let address = if address.contains(':') && !address.starts_with('[') {
        format!("[{address}]")
    } else {
        address.into()
    };
    let endpoint = format!(
        "{address}:{}",
        port.map(|port| port.to_string())
            .unwrap_or_else(|| "?".into())
    );
    Some(match protocol {
        Some(protocol) => format!("{protocol}:{endpoint}"),
        None => endpoint,
    })
}

fn canonical_sockets(client: &mut Value) {
    let local = client["transport"] == "LOCAL";
    let socket = text(&client["socket"]).map(str::to_owned);
    let protocol =
        text(&client["socket_protocol"]).filter(|protocol| matches!(*protocol, "tcp" | "udp"));
    let network = !local
        && (matches!(client["transport"].as_str(), Some("SSH" | "MOSH"))
            || protocol.is_some()
            || socket
                .as_ref()
                .is_some_and(|socket| socket.starts_with("tcp:") || socket.starts_with("udp:")));
    let peer = text(&client["peer_address"])
        .or_else(|| text(&client["peer"]))
        .filter(|peer| !peer.starts_with("socket:[") && !peer.starts_with("unix:"));
    let mut endpoints = vec![
        (
            "tmux_client_socket",
            client["socket_inode"]
                .as_u64()
                .filter(|inode| *inode > 0)
                .map(|inode| format!("socket:[{inode}]")),
        ),
        (
            "tmux_server_socket",
            client["peer_inode"]
                .as_u64()
                .filter(|inode| *inode > 0)
                .map(|inode| format!("socket:[{inode}]")),
        ),
        (
            "network_local_socket",
            network
                .then(|| {
                    endpoint(
                        text(&client["local_address"]),
                        &client["local_port"],
                        protocol,
                    )
                })
                .flatten(),
        ),
        (
            "network_peer_socket",
            network
                .then(|| endpoint(peer, &client["peer_port"], None))
                .flatten(),
        ),
    ];
    if let Some(socket) = &socket {
        if socket.starts_with("socket:[") && endpoints[0].1.is_none() {
            endpoints[0].1 = Some(socket.clone());
        } else if !local
            && (socket.starts_with("tcp:") || socket.starts_with("udp:"))
            && (endpoints[2].1.is_none()
                || text(&client["local_address"]).is_none()
                || client["local_port"].is_null())
        {
            endpoints[2].1 = Some(socket.clone());
        }
    }
    if endpoints[1].1.is_none() {
        endpoints[1].1 = text(&client["peer"])
            .filter(|peer| peer.starts_with("socket:["))
            .map(str::to_owned);
    }
    let Some(object) = client.as_object_mut() else {
        return;
    };
    for (key, fallback) in endpoints {
        let entry = object.entry(key).or_insert(Value::Null);
        if text(entry).is_none() {
            *entry = fallback.map(Value::String).unwrap_or(Value::Null);
        }
    }
    // These are components/legacy aliases of the four endpoint fields above.
    for key in [
        "socket",
        "socket_inode",
        "peer_inode",
        "socket_protocol",
        "local_address",
        "local_port",
        "peer_address",
        "peer_port",
        "peer",
    ] {
        object.remove(key);
    }
    if let Some(socket) = socket.filter(|socket| {
        !socket.starts_with("socket:[")
            && !socket.starts_with("unix:")
            && !socket.starts_with("tcp:")
            && !socket.starts_with("udp:")
    }) {
        object.insert("legacy_socket".into(), Value::String(socket));
    }
}

pub(super) fn rows(
    inventory: &Inventory,
    original: &Value,
    own: bool,
    shown: &[ProcessInfoNode],
) -> Vec<InfoPopupRow> {
    let frontend_shown = shown.iter().any(|node| {
        original["frontend_pid"].as_i64() == Some(i64::from(node.identity.pid))
            && original["frontend_started_ticks"].as_u64() == Some(node.identity.start_ticks)
    });
    let mut client = original.clone();
    canonical_sockets(&mut client);
    if let Some(object) = client.as_object_mut() {
        if own {
            object.remove("pid");
            object.remove("started_ticks");
        }
        if frontend_shown {
            for key in [
                "frontend_pid",
                "frontend_started_ticks",
                "frontend_binary_mtime",
                "frontend_outdated",
                "frontend_version",
            ] {
                object.remove(key);
            }
        }
        if object.get("state").is_none_or(Value::is_null)
            || object.get("state") == object.get("attachment")
            || object.get("state") == object.get("reachability")
        {
            object.remove("state");
        }
        if object.get("app_label").is_some_and(Value::is_null) {
            object.remove("app_label");
        }
        if object.get("mosh_client_version")
            == object
                .get("mosh_client_build")
                .and_then(|build| build.get("version"))
        {
            object.remove("mosh_client_version");
        }
    }
    let mut rows = Vec::new();
    fields(&mut rows, "", &client, InfoFieldScope::InstanceContext, 0);
    let servers = &inventory.data["servers"];
    if !frontend_shown {
        let instances = match original["frontend"].as_str() {
            Some("xfce4-terminal") => servers["xfce4-terminal"].as_array(),
            Some("mosh-server" | "mosh-native-server") => servers["mosh"].as_array(),
            _ => None,
        };
        if let Some(instances) = instances {
            let mut matches = instances.iter().filter(|instance| {
                original["frontend_pid"].as_i64().is_some()
                    && original["frontend_started_ticks"].as_u64().is_some()
                    && instance["pid"] == original["frontend_pid"]
                    && instance["started_ticks"] == original["frontend_started_ticks"]
            });
            if let Some(instance) = matches.next().filter(|_| matches.next().is_none()) {
                rows.push(scoped_row(
                    InfoFieldScope::RunningInstance,
                    "Frontend running SHA256",
                    value("sha256", &instance["sha256"]),
                ));
            }
        }
        let reference = match original["frontend"].as_str() {
            Some("xfce4-terminal") => Some(&servers["xfce4-terminal_latest"]),
            Some("mosh-server" | "mosh-native-server") => {
                Some(&servers["mosh_latest"]["mosh-server"])
            }
            _ => None,
        };
        if let Some(reference) = reference.filter(|reference| !reference.is_null()) {
            fields(
                &mut rows,
                "Frontend latest installed build",
                reference,
                InfoFieldScope::Installation,
                0,
            );
        }
    }
    if client["transport"] == "MOSH" && !servers["mosh_client_latest"].is_null() {
        fields(
            &mut rows,
            "Mosh client latest installed build",
            &servers["mosh_client_latest"],
            InfoFieldScope::Installation,
            0,
        );
    }
    rows
}
