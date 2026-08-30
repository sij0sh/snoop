use crate::core::{AnchorKind, BuiltAnchor};

pub(super) fn git_routing(short: &str, subject: &str, path: &str, symbol: Option<&str>) -> String {
    format!(
        "source: git_change\ncommit: {short}\nmessage: {subject}\nchanged file: {path}\nchanged symbol: {}",
        symbol.unwrap_or("-")
    )
}

pub(super) fn git_anchors(
    oid: &str,
    old_path: Option<&str>,
    new_path: &str,
    old_symbol: Option<&str>,
    new_symbol: Option<&str>,
) -> Vec<BuiltAnchor> {
    let mut anchors = vec![
        BuiltAnchor {
            kind: AnchorKind::Commit,
            value: oid.to_string(),
            relationship: "part_of".to_string(),
        },
        BuiltAnchor {
            kind: AnchorKind::File,
            value: new_path.to_string(),
            relationship: "changes".to_string(),
        },
    ];
    if let Some(old_path) = old_path.filter(|old| *old != new_path) {
        anchors.push(BuiltAnchor {
            kind: AnchorKind::File,
            value: old_path.to_string(),
            relationship: "renamed_from".to_string(),
        });
    }
    match (new_symbol, old_symbol) {
        (Some(new), old) => {
            anchors.push(BuiltAnchor {
                kind: AnchorKind::Symbol,
                value: new.to_string(),
                relationship: "changes".to_string(),
            });
            if let Some(old) = old.filter(|old| !old.is_empty()) {
                anchors.push(BuiltAnchor {
                    kind: AnchorKind::Symbol,
                    value: old.to_string(),
                    relationship: "renamed_from".to_string(),
                });
            }
        }
        (None, Some(old)) => {
            if !old.is_empty() {
                anchors.push(BuiltAnchor {
                    kind: AnchorKind::Symbol,
                    value: old.to_string(),
                    relationship: "deleted".to_string(),
                });
            }
        }
        (None, None) => {}
    }
    anchors
}
