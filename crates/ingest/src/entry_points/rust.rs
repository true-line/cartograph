//! Rust programs (#2041): what starts work in a Rust system, read with the
//! Rust grammar; comments and string literals state nothing.
//!
//! - a top-level `fn main()` in a binary root (`main.rs`, or a file
//!   directly under a `bin/` directory) is a process start. It is a desktop
//!   application when it builds a `tauri::Builder` (or only calls a
//!   top-level function of its own package's library that does, read in
//!   `entry_points.rs`), a gRPC server when it builds a `tonic`
//!   `Server::builder()`, an HTTP server when it serves axum, actix-web,
//!   hyper or warp; a command under `bin/` or an operator
//!   directory; otherwise a process;
//! - a function annotated `#[tauri::command]` (or `#[command]` with
//!   `tauri::command` imported) is an IPC command the desktop front end
//!   invokes by the function's name;
//! - each method of an `impl Trait for Type` in a file that names `tonic`,
//!   whose return type is `Result<Response<…>, Status>`, is a gRPC method
//!   of the `Trait` service.
//!
//! Each enters the function's Symbol when the Rust adapter emitted it (the
//! same identity: inline modules, then `Type.Trait.method` for a trait
//! impl), else the file. Test, example, bench, fuzz and end-to-end trees and
//! `build.rs` start nothing.

use super::{Kind, OPERATOR_DIRS, Start, Target};
use core_prov::ConfidenceTier;
use std::collections::BTreeSet;
use tree_sitter::{Node as TsNode, Parser};

/// Directories whose programs are not part of the running system.
const NOT_SYSTEM_DIRS: &[&str] = &[
    "e2e", "testdata", "examples", "example", "test", "tests", "benches", "fuzz", "target",
];

/// Calls that make a binary an HTTP server.
const HTTP_SERVES: &[&str] = &[
    "axum::serve(",
    "HttpServer::new(",
    "Server::bind(",
    "warp::serve(",
];

/// Whether `rel` is a Rust file this reader considers.
pub(super) fn candidate(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    rel.ends_with(".rs")
        && name != "build.rs"
        && !rel
            .split('/')
            .rev()
            .skip(1)
            .any(|dir| NOT_SYSTEM_DIRS.contains(&dir))
}

fn rust_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("tree-sitter-rust grammar");
    parser
}

fn span(node: TsNode) -> (u64, u64) {
    (node.start_byte() as u64, node.end_byte() as u64)
}

/// The call a binary's `main` delegates to: `fn main() { krate::name(); }`
/// with nothing else in the body gives `(krate, name)`.
pub(super) fn delegated_call(text: &str, span: (u64, u64)) -> Option<(String, String)> {
    let tree = rust_parser().parse(text, None)?;
    let code = code_only(text, tree.root_node());
    let function = code.get(span.0 as usize..span.1 as usize)?;
    let body = function.split_once('{')?.1.trim_end().strip_suffix('}')?;
    let call = body.trim().strip_suffix(';').unwrap_or(body.trim()).trim();
    let call = call.strip_suffix("()")?.trim_end();
    let (krate, name) = call.split_once("::")?;
    let ident = |part: &str| {
        !part.is_empty()
            && part.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
            && !part.starts_with(|c: char| c.is_ascii_digit())
    };
    (ident(krate) && ident(name) && !matches!(krate, "crate" | "self" | "super"))
        .then(|| (krate.to_string(), name.to_string()))
}

/// The package directory of a binary root (`<pkg>/src/main.rs` or
/// `<pkg>/src/bin/<name>.rs`), with a trailing `/` unless it is the root.
pub(super) fn package_dir(rel: &str) -> Option<&str> {
    rel.strip_suffix("src/main.rs")
        .or_else(|| {
            let (dir, _) = rel.rsplit_once("/")?;
            dir.strip_suffix("src/bin")
        })
        .filter(|dir| dir.is_empty() || dir.ends_with('/'))
}

/// The library crate a `Cargo.toml` declares: its name (`[lib] name`, else
/// the package name with `-` as `_`) and its root relative to the manifest
/// (`[lib] path`, else `src/lib.rs`).
pub(super) fn library(manifest: &str) -> Option<(String, String)> {
    let value: toml::Value = toml::from_str(manifest).ok()?;
    let lib = value.get("lib");
    let name = lib
        .and_then(|lib| lib.get("name"))
        .and_then(|name| name.as_str())
        .map(str::to_string)
        .or_else(|| {
            value
                .get("package")?
                .get("name")?
                .as_str()
                .map(|name| name.replace('-', "_"))
        })?;
    let path = lib
        .and_then(|lib| lib.get("path"))
        .and_then(|path| path.as_str())
        .unwrap_or("src/lib.rs")
        .trim_start_matches("./")
        .to_string();
    Some((name, path))
}

/// The span of the top-level `pub fn <name>` in a library root when its
/// body builds a `tauri::Builder`.
pub(super) fn desktop_function(text: &str, name: &str) -> Option<(u64, u64)> {
    let tree = rust_parser().parse(text, None)?;
    let root = tree.root_node();
    let code = code_only(text, root);
    let mut walk = root.walk();
    let found = root.named_children(&mut walk).find(|node| {
        node.kind() == "function_item"
            && node
                .child_by_field_name("name")
                .is_some_and(|n| &text[n.byte_range()] == name)
            && node
                .named_child(0)
                .is_some_and(|first| first.kind() == "visibility_modifier")
            && code[node.byte_range()].contains("tauri::Builder")
    })?;
    Some(span(found))
}

/// Every start a Rust file declares.
pub(super) fn rust_starts(
    repo: &str,
    rel: &str,
    text: &str,
    symbols: &BTreeSet<String>,
) -> Vec<Start> {
    if !text.contains("fn ") {
        return Vec::new();
    }
    let Some(tree) = rust_parser().parse(text, None) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let code = code_only(text, root);
    let file = File {
        repo,
        rel,
        text,
        code: &code,
        symbols,
    };
    let mut starts = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "function_item" => {
                if let Some(start) = file.main(node).or_else(|| file.tauri_command(node)) {
                    starts.push(start);
                }
            }
            "impl_item" => starts.extend(file.grpc_methods(node)),
            _ => {}
        }
        let mut walk = node.walk();
        let children: Vec<TsNode> = node.named_children(&mut walk).collect();
        stack.extend(children.into_iter().rev());
    }
    starts
}

struct File<'a> {
    repo: &'a str,
    rel: &'a str,
    text: &'a str,
    /// The source with comments and string literals blanked.
    code: &'a str,
    symbols: &'a BTreeSet<String>,
}

impl File<'_> {
    fn text(&self, node: TsNode) -> &str {
        &self.text[node.byte_range()]
    }

    fn name(&self, node: TsNode) -> Option<String> {
        node.child_by_field_name("name")
            .map(|name| self.text(name).to_string())
    }

    /// The `main` start of a binary root.
    fn main(&self, function: TsNode) -> Option<Start> {
        let top = function.parent().is_some_and(|p| p.kind() == "source_file");
        if !top || self.name(function)? != "main" || !self.binary_root() {
            return None;
        }
        let dirs = self.dirs();
        // What `main` itself runs, not a helper elsewhere in the file.
        let body = &self.code[function.byte_range()];
        let kind = if body.contains("tauri::Builder") {
            "desktop-app"
        } else if self.code.contains("tonic") && body.contains("Server::builder(") {
            "grpc-server"
        } else if HTTP_SERVES.iter().any(|call| body.contains(call)) {
            "http-server"
        } else if dirs
            .iter()
            .any(|dir| *dir == "bin" || OPERATOR_DIRS.contains(dir))
        {
            "command"
        } else {
            "process"
        };
        Some(self.start(
            function,
            "main".into(),
            "fn main".into(),
            kind,
            Vec::new(),
            "main",
        ))
    }

    /// A `#[tauri::command]` function.
    fn tauri_command(&self, function: TsNode) -> Option<Start> {
        let imported = self.imports_tauri_command(function);
        let annotated = outer_attributes(function).into_iter().any(|attr| {
            let attr: String = self.text(attr).split_whitespace().collect();
            let path = attr
                .strip_prefix("#[")
                .map(|rest| rest.split(['(', ']']).next().unwrap_or(rest));
            path == Some("tauri::command") || (imported && path == Some("command"))
        });
        if !annotated {
            return None;
        }
        let name = self.name(function)?;
        let local = self.local_name(function, &name);
        Some(self.start(
            function,
            format!("ipc:{local}"),
            "#[tauri::command]".into(),
            "ipc-command",
            vec![
                ("handler", local.clone().into()),
                ("command_name", name.into()),
            ],
            &local,
        ))
    }

    /// Whether a `use` in the module enclosing `node` (the file or its inline
    /// `mod {}`) brings `tauri::command` in as `command`.
    fn imports_tauri_command(&self, node: TsNode) -> bool {
        let mut scope = node;
        while let Some(parent) = scope.parent() {
            scope = parent;
            if scope.kind() == "source_file" {
                break;
            }
            if scope.kind() == "mod_item" {
                match scope.child_by_field_name("body") {
                    Some(body) => {
                        scope = body;
                        break;
                    }
                    None => return false,
                }
            }
        }
        let mut walk = scope.walk();
        let uses: Vec<TsNode> = scope
            .named_children(&mut walk)
            .filter(|child| child.kind() == "use_declaration")
            .collect();
        uses.into_iter().any(|decl| {
            let Some(argument) = decl.child_by_field_name("argument") else {
                return false;
            };
            let path: String = self.code[argument.byte_range()]
                .split_whitespace()
                .collect();
            let path = path.strip_prefix("::").unwrap_or(&path);
            path == "tauri::command"
                || path
                    .strip_prefix("tauri::{")
                    .and_then(|rest| rest.strip_suffix('}'))
                    .is_some_and(|list| list.split(',').any(|item| item == "command"))
        })
    }

    /// The gRPC methods of a tonic service implementation.
    fn grpc_methods(&self, imp: TsNode) -> Vec<Start> {
        let (Some(trait_node), Some(ty)) = (
            imp.child_by_field_name("trait"),
            imp.child_by_field_name("type"),
        ) else {
            return Vec::new();
        };
        if !self.code.contains("tonic") {
            return Vec::new();
        }
        let service: String = self.text(trait_node).split_whitespace().collect();
        let ty = self.text(ty);
        let ty = ty.split('<').next().unwrap_or(ty).trim();
        let ty = ty.rsplit("::").next().unwrap_or(ty).trim();
        let Some(body) = imp.child_by_field_name("body") else {
            return Vec::new();
        };
        let mut starts = Vec::new();
        let mut walk = body.walk();
        for method in body.named_children(&mut walk) {
            if method.kind() != "function_item" {
                continue;
            }
            let Some(returns) = method.child_by_field_name("return_type") else {
                continue;
            };
            let returns: String = self.text(returns).split_whitespace().collect();
            let grpc = ["Result<Response<", "Result<tonic::Response<"]
                .iter()
                .any(|p| returns.starts_with(p))
                && (returns.ends_with(",Status>") || returns.ends_with(",tonic::Status>"));
            let Some(name) = self.name(method).filter(|_| grpc) else {
                continue;
            };
            let identity = format!("{ty}.{service}.{name}");
            let local = self.local_name(imp, &identity);
            starts.push(self.start(
                method,
                format!("grpc:{local}"),
                "tonic service".into(),
                "grpc-method",
                vec![
                    ("handler", local.clone().into()),
                    ("service", service.clone().into()),
                    ("method", name.into()),
                ],
                &local,
            ));
        }
        starts
    }

    /// `identity` behind the inline modules enclosing `node`, as the Rust
    /// adapter names the symbol.
    fn local_name(&self, node: TsNode, identity: &str) -> String {
        let mut mods = Vec::new();
        let mut at = node;
        while let Some(parent) = at.parent() {
            if parent.kind() == "mod_item"
                && let Some(name) = self.name(parent)
            {
                mods.push(name);
            }
            at = parent;
        }
        mods.reverse();
        mods.push(identity.to_string());
        mods.join("::")
    }

    fn start(
        &self,
        node: TsNode,
        key: String,
        how: String,
        kind: &'static str,
        extra: Vec<(&'static str, serde_json::Value)>,
        local: &str,
    ) -> Start {
        let proven = self
            .symbols
            .contains(&format!("sym:{}@{}#{local}", self.repo, self.rel));
        Start {
            declared_in: self.rel.to_string(),
            span: span(node),
            key,
            how,
            command: None,
            kind: Kind {
                kind,
                evidence: None,
            },
            port: None,
            extra,
            extra_spans: Vec::new(),
            target: Target::File {
                rel: self.rel.to_string(),
                function: proven.then(|| local.to_string()),
                confidence: ConfidenceTier::Confirmed,
                marker: None,
            },
        }
    }

    fn dirs(&self) -> Vec<&str> {
        let mut dirs: Vec<&str> = self.rel.split('/').collect();
        dirs.pop();
        dirs
    }

    /// `main.rs`, or a file directly under `bin/`.
    fn binary_root(&self) -> bool {
        self.rel.rsplit('/').next() == Some("main.rs") || self.dirs().last() == Some(&"bin")
    }
}

fn outer_attributes(node: TsNode) -> Vec<TsNode> {
    let mut attrs = Vec::new();
    let mut at = node.prev_sibling();
    while let Some(sibling) = at {
        match sibling.kind() {
            "attribute_item" => attrs.push(sibling),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        at = sibling.prev_sibling();
    }
    attrs
}

/// `text` with comments and string literals blanked to spaces (newlines
/// kept), so what they say is never read as code.
fn code_only(text: &str, root: TsNode) -> String {
    let mut out = text.as_bytes().to_vec();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "line_comment" | "block_comment" | "string_literal" | "raw_string_literal"
        ) {
            for byte in &mut out[node.byte_range()] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
            continue;
        }
        let mut walk = node.walk();
        stack.extend(node.children(&mut walk));
    }
    // Whole nodes were blanked byte for byte; a multi-byte character inside
    // one became spaces, so the text stays UTF-8.
    String::from_utf8(out).unwrap_or_default()
}
