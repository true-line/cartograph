//! The file listing a reader sees over a source capture (#385): the
//! checkout is listed for its directories and the files the reader does not
//! select, and the reader's own selection comes from the capture alone, so a
//! selected file created after capture is not listed and a captured file the
//! checkout no longer lists is.

use std::collections::BTreeSet;
use std::path::Path;

use source_walk::WalkedFile;

/// The checkout walk the readers share.
pub(crate) fn walk(root: &Path) -> std::io::Result<Vec<WalkedFile>> {
    source_walk::files(
        root,
        &crate::preflight::skip_dir,
        source_walk::Gitignores::Honor,
    )
}

/// Every walked path `selects` accepts, in path order: what a capture
/// retains for the reader.
pub(crate) fn selected(root: &Path, selects: fn(&str) -> bool) -> std::io::Result<Vec<String>> {
    Ok(walk(root)?
        .into_iter()
        .map(|file| file.rel)
        .filter(|rel| selects(rel))
        .collect())
}

/// The walk with every selected file replaced by the capture's: `captured`
/// is the capture's selection, in any order.
pub(crate) fn listing(
    root: &Path,
    captured: &[String],
    selects: fn(&str) -> bool,
) -> std::io::Result<Vec<WalkedFile>> {
    let held: BTreeSet<&str> = captured.iter().map(String::as_str).collect();
    let mut walked = walk(root)?;
    walked.retain(|file| !selects(&file.rel) || held.contains(file.rel.as_str()));
    let listed: BTreeSet<String> = walked.iter().map(|file| file.rel.clone()).collect();
    for rel in held {
        if !listed.contains(rel) {
            walked.push(WalkedFile {
                rel: rel.to_string(),
                path: root.join(rel),
                name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
            });
        }
    }
    walked.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(walked)
}
