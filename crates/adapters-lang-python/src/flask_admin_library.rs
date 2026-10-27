//! Flask-Admin's own routes, for the releases whose source was checked
//! (#1203, AC-0415).
//!
//! A `BaseModelView` subclass, and an `AdminIndexView` that does not define
//! `index`, are routed by methods Flask-Admin itself `@expose`s. Those rules
//! are fixed by the release, so they are projected only when every exact
//! `flask-admin` pin the repository's root lockfiles declare names a release
//! in [`RELEASES`], whose tables agree. No pin, an inexact requirement or an
//! unchecked release keeps the existing Gap, naming why.
//!
//! The tables are read from the released sdists (`flask_admin/model/base.py`
//! and `flask_admin/base.py`); 1.5.7 and 1.5.8 expose the same methods with
//! the same rules, and their `base.py` is identical. A method is exposed
//! whatever `can_create`/`can_edit`/`can_delete` say: those are checked per
//! request, not at registration.

use core_prov::EvidenceRef;
use std::path::Path;

/// Releases whose route tables below were checked against their source.
const RELEASES: &[&str] = &["1.5.7", "1.5.8"];

/// `BaseModelView`'s exposed methods: name, rule, methods.
pub(crate) const MODEL_ROUTES: &[(&str, &str, &[&str])] = &[
    ("index_view", "/", &["GET"]),
    ("create_view", "/new/", &["GET", "POST"]),
    ("edit_view", "/edit/", &["GET", "POST"]),
    ("details_view", "/details/", &["GET"]),
    ("delete_view", "/delete/", &["POST"]),
    ("action_view", "/action/", &["POST"]),
    ("export", "/export/<export_type>/", &["GET"]),
    ("ajax_lookup", "/ajax/lookup/", &["GET"]),
    ("ajax_update", "/ajax/update/", &["POST"]),
];

/// `AdminIndexView`'s exposed `index` (`@expose()`, so rule `/`).
pub(crate) const INDEX_ROUTES: &[(&str, &str, &[&str])] = &[("index", "/", &["GET"])];

/// The library symbol a projected route's handler names.
pub(crate) fn handler(model: bool, method: &str) -> String {
    let (path, class) = if model {
        ("flask_admin/model/base.py", "BaseModelView")
    } else {
        ("flask_admin/base.py", "AdminIndexView")
    };
    format!("{LIBRARY_SYMBOLS}{path}#{class}.{method}")
}

/// The id prefix of every Flask-Admin library symbol a route names; the
/// placeholder closure classifies these as external.
pub(crate) const LIBRARY_SYMBOLS: &str = "sym:pypi/flask-admin@";

/// Lockfiles larger than this are not read.
const MAX_LOCK_BYTES: u64 = 8 << 20;

/// What the repository's pins allow.
#[derive(Debug, Clone, Default)]
pub(crate) enum Pins {
    /// No root lockfile pins `flask-admin`.
    #[default]
    Absent,
    /// Every pin is a checked release; the pins' evidence.
    Checked(Vec<EvidenceRef>),
    /// A pin the tables do not cover: the Gap reason.
    Unchecked(String),
}

/// Read every root lockfile's `flask-admin` pins.
pub(crate) fn pins(root: &Path, repo: &str, commit: &str) -> Pins {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Pins::Absent;
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| {
            matches!(name.as_str(), "uv.lock" | "poetry.lock" | "pdm.lock")
                || name.starts_with("requirements")
                    && (name.ends_with(".txt") || name.ends_with(".lock"))
        })
        .collect();
    names.sort();
    let mut evidence = Vec::new();
    for name in names {
        let path = root.join(&name);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_LOCK_BYTES {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let found = if name.ends_with(".lock") && !name.starts_with("requirements") {
            package_table_pins(&text)
        } else {
            requirement_pins(&text)
        };
        for (version, start, end) in found {
            let Some(version) = version else {
                return Pins::Unchecked(format!("flask_admin_version_not_exact:{name}"));
            };
            if !RELEASES.contains(&version.as_str()) {
                return Pins::Unchecked(format!("flask_admin_version_not_modelled:{version}"));
            }
            evidence.push(EvidenceRef {
                repo: repo.into(),
                path: name.clone(),
                byte_start: start as u64,
                byte_end: end as u64,
                commit_sha: commit.into(),
            });
        }
    }
    if evidence.is_empty() {
        Pins::Absent
    } else {
        Pins::Checked(evidence)
    }
}

/// PEP 503 normalization: `Flask_Admin` and `flask.admin` are `flask-admin`.
fn is_flask_admin(name: &str) -> bool {
    name.to_ascii_lowercase().replace(['_', '.'], "-") == "flask-admin"
}

/// Lines with their byte offsets.
fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split_inclusive('\n').scan(0, |offset, line| {
        let start = *offset;
        *offset += line.len();
        Some((start, line.trim_end_matches(['\n', '\r'])))
    })
}

/// `[[package]]` tables (`uv.lock`, `poetry.lock`, `pdm.lock`): the
/// `version` of each table named `flask-admin`, with its line's span.
fn package_table_pins(text: &str) -> Vec<(Option<String>, usize, usize)> {
    let mut found = Vec::new();
    let (mut in_package, mut named) = (false, false);
    for (start, line) in lines(text) {
        if line.starts_with('[') {
            in_package = line == "[[package]]";
            named = false;
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(value) = toml_string(line, "name") {
            named = is_flask_admin(&value);
        } else if named && let Some(version) = toml_string(line, "version") {
            found.push((Some(version), start, start + line.len()));
            named = false;
        }
    }
    found
}

/// `key = "value"` at the start of a line.
fn toml_string(line: &str, key: &str) -> Option<String> {
    let rest = line
        .strip_prefix(key)?
        .trim_start()
        .strip_prefix('=')?
        .trim();
    let value = rest.strip_prefix('"')?.strip_suffix('"')?;
    (!value.contains(['"', '\\'])).then(|| value.to_string())
}

/// `flask-admin==X` requirement lines: `Some(X)` for an exact pin, `None`
/// for any other specifier (or none).
fn requirement_pins(text: &str) -> Vec<(Option<String>, usize, usize)> {
    let mut found = Vec::new();
    for (start, line) in lines(text) {
        let requirement = line
            .split(" #")
            .next()
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("");
        let requirement = requirement.trim();
        let end = requirement
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            .unwrap_or(requirement.len());
        if end == 0 || !is_flask_admin(&requirement[..end]) {
            continue;
        }
        let mut rest = requirement[end..].trim_start();
        if let Some(extras) = rest.strip_prefix('[') {
            let Some((_, after)) = extras.split_once(']') else {
                found.push((None, start, start + line.len()));
                continue;
            };
            rest = after.trim_start();
        }
        let version = rest
            .strip_prefix("==")
            .map(str::trim)
            .filter(|version| {
                !version.is_empty()
                    && version
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '.')
            })
            .map(str::to_string);
        found.push((version, start, start + line.len()));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_tables_and_requirements_yield_exact_pins_only() {
        let lock = "[[package]]\nname = \"flask\"\nversion = \"2.0\"\n\n[[package]]\nname = \"flask-admin\"\nversion = \"1.5.7\"\ndependencies = [\n    { name = \"flask-admin\" },\n]\n\n[package.optional-dependencies]\nname = \"flask-admin\"\nversion = \"9\"\n";
        assert_eq!(
            package_table_pins(lock),
            vec![(Some("1.5.7".into()), 77, 94)]
        );
        assert_eq!(&lock[77..94], "version = \"1.5.7\"");
        let requirements = "Flask_Admin[sqla]==1.5.8 ; python_version > '3'\nflask-admin>=1.5\nflask-adminx==1\n# flask-admin==0\n";
        assert_eq!(
            requirement_pins(requirements),
            vec![(Some("1.5.8".into()), 0, 47), (None, 48, 64)]
        );
    }
}
