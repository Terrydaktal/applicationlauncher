use super::*;
use program_metadata::desktop::{RelatedEntry, TargetMatch};

fn clarification(entry: &RelatedEntry) -> String {
    let mut notes = vec![format!("ID {}", entry.desktop_id)];
    if let Some(higher) = &entry.overridden_by {
        notes.push(if entry.ambiguous_precedence {
            "overridden by higher-priority ID copies; selection among those copies is ambiguous"
                .into()
        } else {
            format!("overridden by {}", higher.display())
        });
    } else if entry.ambiguous_precedence {
        notes.push("same-priority ID collision; precedence ambiguous".into());
    } else {
        notes.push("effective for this ID".into());
    }
    match entry.target {
        TargetMatch::SameExecutable => {
            notes.push("same on-disk application target; launch options may differ".into())
        }
        TargetMatch::DifferentExecutable => notes.push("different application target".into()),
        TargetMatch::Unresolved => notes.push("application target unverified".into()),
    }
    if entry.hidden == Some(true) {
        notes.push(
            if entry.overridden_by.is_none() && !entry.ambiguous_precedence {
                "Hidden=true; disables this ID".into()
            } else {
                "Hidden=true".into()
            },
        );
    } else if entry.hidden.is_none() {
        notes.push("desktop metadata unreadable".into());
    }
    if entry.no_display == Some(true) {
        notes.push("NoDisplay=true".into());
    }
    format!("[{}]", notes.join("; "))
}

pub(super) fn append(
    rows: &mut [InfoPopupRow],
    primary: Option<&Path>,
    search: Option<&crate::windows::versions::DesktopFileSearch>,
) {
    let Some(row) = rows.iter_mut().find(|row| row.label == "Desktop file") else {
        return;
    };
    let Some(search) = search else {
        return;
    };
    let files = match search {
        Ok(files) => files,
        Err(error) => {
            row.value.push_str(&format!(
                " [related desktop-file search unavailable: {error}]"
            ));
            return;
        }
    };
    let mut seen = HashSet::new();
    let extra = files
        .iter()
        .filter(|entry| Some(entry.path.as_path()) != primary && seen.insert(&entry.path))
        .collect::<Vec<_>>();
    if extra.is_empty() {
        return;
    }
    if primary.is_none() {
        row.value = "No single associated desktop entry".into();
    } else if let Some(entry) = files
        .iter()
        .find(|entry| Some(entry.path.as_path()) == primary)
    {
        row.value.push(' ');
        row.value.push_str(&clarification(entry));
    }
    for entry in extra {
        row.value.push_str(&format!(
            "; {} {}",
            entry.path.display(),
            clarification(entry)
        ));
    }
}

#[cfg(test)]
mod tests;
