use super::*;

fn executable_is(node: &ProcessInfoNode, basename: &str) -> bool {
    node.executable
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.trim_end_matches(" (deleted)") == basename)
}

fn is_decoder(node: &ProcessInfoNode) -> bool {
    node.executable.as_deref().is_some_and(|path| {
        path.components()
            .any(|part| part.as_os_str() == "glycin-loaders")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.trim_end_matches(" (deleted)")
                        .strip_prefix("glycin-")
                        .is_some_and(|suffix| !suffix.is_empty())
                })
    })
}

pub(super) fn image_loader_helpers(
    target: &WindowProcessTreeTarget,
    nodes: &[ProcessInfoNode],
) -> HashSet<ProcessIdentity> {
    let indices: BTreeMap<_, _> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.identity.pid, index))
        .collect();
    let parents: Vec<_> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            indices.get(&node.parent_pid).copied().filter(|parent| {
                *parent < index && nodes[*parent].identity.start_ticks <= node.identity.start_ticks
            })
        })
        .collect();
    let mut protected = HashSet::new();
    for seed in target.seeds() {
        let mut index = indices.get(&seed).copied();
        while let Some(current) = index {
            if !protected.insert(current) {
                break;
            }
            index = parents[current];
        }
    }

    let mut children = vec![Vec::new(); nodes.len()];
    let mut decoder_branch = vec![false; nodes.len()];
    let mut hidden = vec![false; nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        if let Some(parent) = parents[index] {
            children[parent].push(index);
        }
        decoder_branch[index] = parents[index].is_some_and(|parent| decoder_branch[parent])
            || (is_decoder(node) && !protected.contains(&index));
        hidden[index] = decoder_branch[index] && !protected.contains(&index);
    }
    for index in (0..nodes.len()).rev() {
        // A generic sandbox is not an image helper. Hide its wrappers only when
        // every captured child belongs to a recognised decoder branch.
        if !protected.contains(&index)
            && executable_is(&nodes[index], "bwrap")
            && !children[index].is_empty()
            && children[index].iter().all(|child| hidden[*child])
        {
            hidden[index] = true;
        }
    }
    nodes
        .iter()
        .zip(hidden)
        .filter_map(|(node, hidden)| hidden.then_some(node.identity))
        .collect()
}
