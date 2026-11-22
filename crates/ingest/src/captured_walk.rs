//! The file listing a reader sees over a source capture (#385). A capture
//! retains the reader's selection and every file a checkout read of it
//! cites, so over the capture the reader lists the capture's own paths and
//! never the checkout: whether a file a declaration names exists is decided
//! by the snapshot, not by the live tree.

use std::collections::BTreeSet;
use std::path::Path;

use core_graph::{Edge, Node};
use source_walk::WalkedFile;

/// The checkout walk the readers share.
pub(crate) fn walk(root: &Path) -> std::io::Result<Vec<WalkedFile>> {
    source_walk::files(
        root,
        &crate::preflight::skip_dir,
        source_walk::Gitignores::Honor,
    )
}

/// What a capture retains for a reader, in path order: every walked path
/// `selects` accepts, and every walked file the reader's facts over the
/// checkout cite (a `File` it enters, an evidence span), so a target a
/// declaration names is retained whatever its name.
pub(crate) fn selected(
    root: &Path,
    selects: fn(&str) -> bool,
    facts: impl FnOnce() -> std::io::Result<(Vec<Node>, Vec<Edge>)>,
) -> std::io::Result<Vec<String>> {
    let walked: BTreeSet<String> = walk(root)?.into_iter().map(|file| file.rel).collect();
    let mut paths: BTreeSet<String> = walked.iter().filter(|rel| selects(rel)).cloned().collect();
    let (nodes, edges) = facts()?;
    let mut cited = BTreeSet::new();
    for props in nodes
        .iter()
        .map(|node| &node.props)
        .chain(edges.iter().map(|edge| &edge.props))
    {
        paths_in(props, &mut cited);
    }
    // An env file holds secrets: no capture retains it (AC-0097).
    paths.extend(cited.into_iter().filter(|rel| {
        walked.contains(rel)
            && !crate::toolchain::is_env_name(rel.rsplit('/').next().unwrap_or(rel))
    }));
    Ok(paths.into_iter().collect())
}

/// Every string a `path` key holds anywhere in `value`.
fn paths_in(value: &serde_json::Value, out: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if key == "path"
                    && let Some(path) = value.as_str()
                {
                    out.insert(path.to_string());
                }
                paths_in(value, out);
            }
        }
        serde_json::Value::Array(values) => values.iter().for_each(|value| paths_in(value, out)),
        _ => {}
    }
}

/// The listing over a capture: `captured`, the capture's own paths, in path
/// order.
pub(crate) fn listing(root: &Path, captured: &[String]) -> Vec<WalkedFile> {
    let held: BTreeSet<&str> = captured.iter().map(String::as_str).collect();
    held.into_iter()
        .map(|rel| WalkedFile {
            rel: rel.to_string(),
            path: root.join(rel),
            name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
        })
        .collect()
}
