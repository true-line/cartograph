//! Flask-Admin's own routes, for the releases whose source was checked
//! (#1203, AC-0415).
//!
//! A `BaseModelView` subclass, and an `AdminIndexView` that does not define
//! `index`, are routed by methods Flask-Admin itself `@expose`s. Those rules
//! are fixed by the release, so they are projected only when every exact
//! `flask-admin` pin the repository's root lockfiles declare names a release
//! in [`RELEASES`], whose tables agree. No pin, an inexact requirement, an
//! unchecked release or a lockfile that cannot be read keeps the existing
//! Gap, naming why.
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

/// Lockfiles larger than this are not read, unless a project raises the
/// bound (AC-1805).
pub(crate) const MAX_LOCK_BYTES: u64 = 8 << 20;

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
pub(crate) fn pins(root: &Path, repo: &str, commit: &str, max_lock_bytes: u64) -> Pins {
    package_pins(
        root,
        repo,
        commit,
        "flask-admin",
        max_lock_bytes,
        |version| RELEASES.contains(&version),
    )
}

/// Read every root lockfile's `flask-login` pins: 0.3 made
/// `is_authenticated` a property, false for `AnonymousUserMixin`, and it
/// still is through 0.6.
pub(crate) fn login_pins(root: &Path, repo: &str, commit: &str, max_lock_bytes: u64) -> Pins {
    package_pins(
        root,
        repo,
        commit,
        "flask-login",
        max_lock_bytes,
        |version| {
            let mut parts = version.split('.');
            parts.next() == Some("0")
                && parts
                    .next()
                    .is_some_and(|minor| matches!(minor, "3" | "4" | "5" | "6"))
                && parts.all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        },
    )
}

/// Every root lockfile's exact pins of `package`, each a release `checked`
/// accepts. Gap reasons name the package as `flask_admin` / `flask_login`.
fn package_pins(
    root: &Path,
    repo: &str,
    commit: &str,
    package: &str,
    max_lock_bytes: u64,
    checked: impl Fn(&str) -> bool,
) -> Pins {
    let prefix = package.replace('-', "_");
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
        // A lockfile that cannot be read (a symlink, a directory, too large,
        // not UTF-8) may pin anything, so it refuses rather than being skipped.
        let text = std::fs::symlink_metadata(&path)
            .ok()
            .filter(|meta| meta.is_file() && meta.len() <= max_lock_bytes)
            .and_then(|_| std::fs::read_to_string(&path).ok());
        let Some(text) = text else {
            return Pins::Unchecked(format!("{prefix}_lockfile_not_inspected:{name}"));
        };
        let found = if name.ends_with(".lock") && !name.starts_with("requirements") {
            let Some(found) = package_table_pins(&text, package) else {
                return Pins::Unchecked(format!("{prefix}_lockfile_syntax_not_modelled:{name}"));
            };
            found
        } else {
            requirement_pins(&text, package)
        };
        for (version, start, end) in found {
            let Some(version) = version else {
                return Pins::Unchecked(format!("{prefix}_version_not_exact:{name}"));
            };
            if !checked(&version) {
                return Pins::Unchecked(format!("{prefix}_version_not_modelled:{version}"));
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
fn is_package(name: &str, package: &str) -> bool {
    name.to_ascii_lowercase().replace(['_', '.'], "-") == package
}

/// Lines with their byte offsets.
fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split_inclusive('\n').scan(0, |offset, line| {
        let start = *offset;
        *offset += line.len();
        Some((start, line.trim_end_matches(['\n', '\r'])))
    })
}

/// `[[package]]` tables (`uv.lock`, `poetry.lock`, `pdm.lock`): for each
/// table named `package`, in whatever key order, its `version` and that
/// line's span. A table whose `name` or `version` is not a plain string
/// (another syntax, a repeated key) yields `None`, refusing rather than
/// skipping it. An escaped key or header, or an inline `package` array,
/// could name a table this reader cannot see, so the whole file is refused
/// (`None`).
fn package_table_pins(text: &str, package: &str) -> Option<Vec<(Option<String>, usize, usize)>> {
    #[derive(Default)]
    struct Table {
        names: Vec<Option<String>>,
        versions: Vec<(Option<String>, usize, usize)>,
        start: usize,
    }
    fn close(table: Option<Table>, package: &str, found: &mut Vec<(Option<String>, usize, usize)>) {
        let Some(table) = table else { return };
        let named = table
            .names
            .iter()
            .any(|name| name.as_deref().is_none_or(|name| is_package(name, package)));
        if !named {
            return;
        }
        match (table.names.as_slice(), table.versions.as_slice()) {
            ([Some(_)], [(Some(version), start, end)]) => {
                found.push((Some(version.clone()), *start, *end));
            }
            (_, [(_, start, end), ..]) => found.push((None, *start, *end)),
            _ => found.push((None, table.start, table.start)),
        }
    }
    let mut found = Vec::new();
    let mut table: Option<Table> = None;
    for (start, line) in lines(text) {
        let code = strip_comment(line).trim();
        if code.starts_with('[') && code.contains('\\') {
            return None;
        }
        if code.starts_with('[') {
            close(table.take(), package, &mut found);
            if code.replace([' ', '\t', '"', '\''], "") == "[[package]]" {
                table = Some(Table {
                    start,
                    ..Table::default()
                });
            }
            continue;
        }
        let Some((key, value)) = code.split_once('=') else {
            continue;
        };
        if key.contains('\\') {
            return None;
        }
        let Some(table) = table.as_mut() else {
            if toml_key(key) == "package" {
                return None;
            }
            continue;
        };
        match toml_key(key) {
            "name" => table.names.push(toml_string(value)),
            "version" => table
                .versions
                .push((toml_string(value), start, start + line.len())),
            _ => {}
        }
    }
    close(table.take(), package, &mut found);
    Some(found)
}

/// A bare, `"basic"` or `'literal'` key, unquoted.
fn toml_key(key: &str) -> &str {
    let key = key.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = key
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    key
}

/// A line without its `#` comment, outside a basic or literal string.
fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    for (index, c) in line.char_indices() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (c, Some(open)) if c == open => quote = None,
            ('#', None) => return &line[..index],
            _ => {}
        }
    }
    line
}

/// A plain `"value"`: no escapes, quotes or other TOML string forms.
fn toml_string(value: &str) -> Option<String> {
    let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
    (!value.contains(['"', '\\'])).then(|| value.to_string())
}

/// `<package>==X` requirement lines: `Some(X)` for an exact pin, with
/// only `--hash=` options after it, `None` for any other specifier (or none).
fn requirement_pins(text: &str, package: &str) -> Vec<(Option<String>, usize, usize)> {
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
        if end == 0 || !is_package(&requirement[..end], package) {
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
        // `==X` may be followed only by `--hash=` options and a `\`
        // continuation, as pip-compile writes them.
        let version = rest.strip_prefix("==").and_then(|pinned| {
            let mut tokens = pinned.split_whitespace();
            let version = tokens.next()?;
            let exact = version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.')
                && tokens.all(|token| token == "\\" || token.starts_with("--hash="));
            exact.then(|| version.to_string())
        });
        found.push((version, start, start + line.len()));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-1805 (#1653): the lockfile bound is the project's. A lockfile past
    // it may pin anything, so the pins are unchecked; under the default the
    // same file is read.
    #[test]
    fn the_lockfile_bound_decides_which_lockfiles_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let requirements = "flask-admin==1.5.8\n";
        std::fs::write(dir.path().join("requirements.txt"), requirements).unwrap();
        assert!(matches!(
            pins(dir.path(), "local/app", "workdir", MAX_LOCK_BYTES),
            Pins::Checked(_)
        ));
        let tight = requirements.len() as u64 - 1;
        assert!(matches!(
            pins(dir.path(), "local/app", "workdir", tight),
            Pins::Unchecked(reason) if reason == "flask_admin_lockfile_not_inspected:requirements.txt"
        ));
        assert!(matches!(
            login_pins(dir.path(), "local/app", "workdir", tight),
            Pins::Unchecked(reason) if reason == "flask_login_lockfile_not_inspected:requirements.txt"
        ));
    }

    #[test]
    fn package_tables_and_requirements_yield_exact_pins_only() {
        let lock = "[[package]]\nname = \"flask\"\nversion = \"2.0\"\n\n[[package]]\nname = \"flask-admin\"\nversion = \"1.5.7\"\ndependencies = [\n    { name = \"flask-admin\" },\n]\n\n[package.optional-dependencies]\nname = \"flask-admin\"\nversion = \"9\"\n";
        assert_eq!(
            package_table_pins(lock, "flask-admin"),
            Some(vec![(Some("1.5.7".into()), 77, 94)])
        );
        assert_eq!(&lock[77..94], "version = \"1.5.7\"");
        let reordered = "[[package]]\nversion = \"1.5.8\" # x\n  name = \"Flask_Admin\"\n\n[[package]]\nname = \"flask-admin\"\nversion = '1'\n\n[[package]]\nname = \"flask-admin\"\n";
        assert_eq!(
            package_table_pins(reordered, "flask-admin"),
            Some(vec![
                (Some("1.5.8".into()), 12, 33),
                (None, 91, 104),
                (None, 106, 106)
            ])
        );
        assert_eq!(&reordered[12..33], "version = \"1.5.8\" # x");
        for escaped in [
            "[[package]]\n\"n\\u0061me\" = \"flask-admin\"\nversion = \"1.6.0\"\n",
            "[[\"p\\u0061ckage\"]]\nname = \"flask-admin\"\nversion = \"1.6.0\"\n",
            "package = [{ name = \"flask-admin\", version = \"1.6.0\" }]\n",
        ] {
            assert_eq!(
                package_table_pins(escaped, "flask-admin"),
                None,
                "{escaped}"
            );
        }
        // pip-compile's hash options, inline or continued, keep a pin exact.
        let hashed = "flask-admin==1.5.8 \\\n    --hash=sha256:aa \\\n    --hash=sha256:bb\nflask-admin==1.5.8 --hash=sha256:aa\nflask-admin==1.5.8 --pre\n";
        assert_eq!(
            requirement_pins(hashed, "flask-admin"),
            vec![
                (Some("1.5.8".into()), 0, 20),
                (Some("1.5.8".into()), 65, 100),
                (None, 101, 125),
            ]
        );
        let requirements = "Flask_Admin[sqla]==1.5.8 ; python_version > '3'\nflask-admin>=1.5\nflask-adminx==1\n# flask-admin==0\n";
        assert_eq!(
            requirement_pins(requirements, "flask-admin"),
            vec![(Some("1.5.8".into()), 0, 47), (None, 48, 64)]
        );
    }
}
