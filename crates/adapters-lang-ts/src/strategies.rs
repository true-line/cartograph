//! Calls through an injected strategy (#2069).
//!
//! Frameworks such as Vendure dispatch business logic through configuration:
//!
//! ```ts
//! const { taxZoneStrategy } = this.configService.taxOptions;
//! const zone = await taxZoneStrategy.determineTaxZone(ctx, zones, channel, order);
//! ```
//!
//! The receiver's type is an interface (`TaxZoneStrategy`), so no typed
//! member resolution reaches code and the call graph ends at the call. The
//! repository's own classes that `implements TaxZoneStrategy` hold the
//! rules (`DefaultTaxZoneStrategy.determineTaxZone`).
//!
//! A member call whose receiver is named for an interface (its last name,
//! `taxZoneStrategy` or `…taxOptions.taxZoneStrategy`, is the camel case of
//! the interface name) and whose name ends in `Strategy`, `Handler`,
//! `Calculator` or `Process` is linked, once the whole directory is read, to
//! the method of the same name on every class in the repository that
//! `implements` that interface. The edge is `CALLS` with resolution
//! `strategy-implementation`, InferredStrong (the receiver's value is chosen
//! at runtime by configuration), and counts the `alternatives` when more
//! than one class implements it. A receiver no class implements, or a class
//! without that method, adds nothing.
//!
//! Configuration also holds plain objects (#2069, AC-0977):
//!
//! ```ts
//! export const defaultPaymentProcess: PaymentProcess<PaymentState> = {
//!     async onTransitionStart(from, to, data) { … },
//! };
//! // vendure-config.ts
//! export interface PaymentOptions { process?: Array<PaymentProcess<any>>; }
//! // payment-state-machine.ts
//! const processes = [...(this.configService.paymentOptions.process ?? [])];
//! for (const process of processes) { await process.onTransitionStart(…); }
//! ```
//!
//! An object literal declared with an interface type (`const x: I = {…}`,
//! at any scope) implements `I` with its methods. A loop variable of
//! `for (const x of xs)`, where `xs` is a `const` bound in the loop's
//! function or up to two functions out, is typed by the configuration it
//! reads: each `….owner.prop` in `xs`'s initializer names the interface
//! that the repository's `interface Owner { prop?: Array<I> }` (or `I[]`,
//! or `I`) declares, so the call on `x` links to the method of every class
//! and typed object implementing those interfaces. The edge carries
//! `implementation` (the class, or the object's name) and counts the
//! `alternatives` the same way.
//!
//! The configuration a state machine builds is an object too: a function
//! or method declared to return `I` that `return`s an object literal
//! implements `I` with that object's methods, and a call on `this.field`,
//! where the class declares `field: I` (a field or a constructor parameter
//! property), links to every implementation of `I`. So
//! `FSM.transitionTo`'s `this.config.onTransitionStart(…)` reaches each
//! `initConfig(): StateMachineConfig` object's hook, and from there the
//! configured processes.

use super::{FileCx, TsNode, enclosing_symbol, sym_id};
use core_graph::facts::FactAccess;
use core_graph::{Edge, GraphError};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, HashSet};

/// The extractor id on every strategy edge.
pub(crate) const EXTRACTOR: &str = "t0.ts-strategy-dispatch";

/// Receiver names read as an injected strategy.
const SUFFIXES: [&str; 4] = ["Strategy", "Handler", "Calculator", "Process"];

/// Most implementations one call is linked to; a wider interface is not a
/// configured strategy.
const MAX_ALTERNATIVES: usize = 8;

/// One class that implements an interface.
#[derive(Debug, Clone)]
struct Implementation {
    interface: String,
    class: String,
    path: String,
}

/// One object literal declared with an interface type, and its methods.
#[derive(Debug, Clone)]
struct ObjectImplementation {
    interface: String,
    name: String,
    /// Method name -> its callable's symbol id.
    methods: BTreeMap<String, String>,
}

/// What a call's receiver is typed as.
#[derive(Debug, Clone)]
enum Receiver {
    /// Named for the interface.
    Interface(String),
    /// A loop variable over configuration reads: `(owner, prop)` pairs.
    Config(Vec<(String, String)>),
}

/// One member call on a receiver typed by an interface.
#[derive(Debug, Clone)]
struct Call {
    src: String,
    receiver: Receiver,
    method: String,
    evidence: EvidenceRef,
}

/// One interface property typed by another interface:
/// `interface Owner { prop?: Array<I> }`.
#[derive(Debug, Clone)]
struct Property {
    owner: String,
    prop: String,
    interface: String,
}

/// How many objects and pairs up from a configured pair its
/// `default…Config` declaration may be.
const MAX_CONFIG_DEPTH: usize = 16;

/// Whether `pair` sits in the object a top-level `defaultConfig` (any
/// `default…Config` name) declaration initializes (AC-1054).
fn in_default_config(cx: &FileCx<'_>, pair: TsNode<'_>) -> bool {
    let mut up = pair;
    // A configuration key sits a few objects deep; a bound keeps deeply
    // nested literals linear.
    for _ in 0..MAX_CONFIG_DEPTH {
        let Some(parent) = up.parent() else {
            return false;
        };
        match parent.kind() {
            "object" | "pair" => up = parent,
            "variable_declarator" => {
                let declared = parent
                    .parent()
                    .and_then(|d| d.parent())
                    .is_some_and(|scope| {
                        scope.kind() == "program"
                            || (scope.kind() == "export_statement"
                                && scope.parent().is_some_and(|p| p.kind() == "program"))
                    });
                return declared
                    && parent.child_by_field_name("value") == Some(up)
                    && parent.child_by_field_name("name").is_some_and(|n| {
                        let name = cx.text(&n);
                        name.starts_with("default") && name.ends_with("Config")
                    });
            }
            _ => return false,
        }
    }
    false
}

/// A class configured for an interface by name: `stockLocationStrategy:
/// new MultiChannelStockLocationStrategy()` in a configuration object
/// (AC-1054).
#[derive(Debug, Clone)]
struct Configured {
    interface: String,
    class: String,
}

/// A file's strategy implementations and calls, awaiting the directory.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pending {
    implementations: Vec<Implementation>,
    objects: Vec<ObjectImplementation>,
    properties: Vec<Property>,
    calls: Vec<Call>,
    /// Every declared class, with its file.
    classes: Vec<(String, String)>,
    configured: Vec<Configured>,
}

impl Pending {
    pub(crate) fn extend(&mut self, other: Self) {
        self.implementations.extend(other.implementations);
        self.objects.extend(other.objects);
        self.properties.extend(other.properties);
        self.calls.extend(other.calls);
        self.classes.extend(other.classes);
        self.configured.extend(other.configured);
    }

    pub(crate) fn retarget(&mut self, commit: &str) {
        for call in &mut self.calls {
            call.evidence.commit_sha = commit.to_string();
        }
    }
}

/// `taxZoneStrategy` → `TaxZoneStrategy`, for a name with a strategy suffix.
fn interface_of(receiver: &str) -> Option<String> {
    let mut chars = receiver.chars();
    let first = chars.next()?;
    if !first.is_ascii_lowercase()
        || !SUFFIXES.iter().any(|suffix| receiver.ends_with(suffix))
        || !receiver.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(first.to_ascii_uppercase().to_string() + chars.as_str())
}

/// The last name of a call's receiver: `x` or `….x`.
fn receiver_name<'s>(cx: &'s FileCx<'_>, receiver: TsNode<'_>) -> Option<&'s str> {
    match receiver.kind() {
        "identifier" => Some(cx.text(&receiver)),
        "member_expression" => receiver
            .child_by_field_name("property")
            .filter(|p| p.kind() == "property_identifier")
            .map(|p| cx.text(&p)),
        _ => None,
    }
}

/// The interface names a class's `implements` clause lists.
fn implemented(cx: &FileCx<'_>, class: TsNode<'_>) -> Vec<String> {
    let mut walk = class.walk();
    let Some(heritage) = class
        .named_children(&mut walk)
        .find(|n| n.kind() == "class_heritage")
    else {
        return Vec::new();
    };
    let mut walk = heritage.walk();
    let clauses: Vec<TsNode<'_>> = heritage
        .named_children(&mut walk)
        .filter(|n| n.kind() == "implements_clause")
        .collect();
    let mut names = Vec::new();
    for clause in clauses {
        let mut walk = clause.walk();
        for ty in clause.named_children(&mut walk) {
            let name = match ty.kind() {
                "type_identifier" => Some(ty),
                "generic_type" => ty.child_by_field_name("name"),
                _ => None,
            };
            if let Some(name) = name.filter(|n| n.kind() == "type_identifier") {
                names.push(cx.text(&name).to_string());
            }
        }
    }
    names
}

/// The interface a type annotation names: `I`, `I<…>`, `Array<I>`,
/// `ReadonlyArray<I>` or `I[]`, when `I` is a plain identifier.
fn named_interface(cx: &FileCx<'_>, ty: TsNode<'_>) -> Option<String> {
    match ty.kind() {
        "type_annotation" => named_interface(cx, ty.named_child(0)?),
        "type_identifier" => Some(cx.text(&ty).to_string()),
        "array_type" => named_interface(cx, ty.named_child(0)?),
        "generic_type" => {
            let name = ty
                .child_by_field_name("name")
                .filter(|n| n.kind() == "type_identifier")?;
            let name = cx.text(&name);
            if matches!(name, "Array" | "ReadonlyArray") {
                let args = ty.child_by_field_name("type_arguments")?;
                if args.named_child_count() != 1 {
                    return None;
                }
                named_interface(cx, args.named_child(0)?)
            } else {
                Some(name.to_string())
            }
        }
        _ => None,
    }
}

/// `xs.owner.prop` member chains in a configuration read, looking through
/// spreads, arrays, `??`/`||` and parentheses.
fn config_reads(cx: &FileCx<'_>, expr: TsNode<'_>, out: &mut Vec<(String, String)>) {
    match expr.kind() {
        "member_expression" => {
            let (Some(object), Some(prop)) = (
                expr.child_by_field_name("object"),
                expr.child_by_field_name("property")
                    .filter(|p| p.kind() == "property_identifier"),
            ) else {
                return;
            };
            if let Some(owner) = receiver_name(cx, object)
                && object.kind() == "member_expression"
            {
                out.push((owner.to_string(), cx.text(&prop).to_string()));
            }
        }
        "array" | "spread_element" | "parenthesized_expression" => {
            let mut walk = expr.walk();
            for child in expr.named_children(&mut walk) {
                config_reads(cx, child, out);
            }
        }
        "binary_expression" => {
            let op = expr
                .child_by_field_name("operator")
                .map(|o| cx.text(&o).to_string());
            if matches!(op.as_deref(), Some("??" | "||")) {
                for field in ["left", "right"] {
                    if let Some(side) = expr.child_by_field_name(field) {
                        config_reads(cx, side, out);
                    }
                }
            }
        }
        _ => {}
    }
}

/// The configuration a `for (const x of xs)` loop variable `name`, used at
/// `site`, iterates: `xs` is a `const`, bound in the loop's function or up
/// to two functions out, whose initializer reads configuration.
fn loop_config(cx: &FileCx<'_>, site: TsNode<'_>, name: &str) -> Vec<(String, String)> {
    let mut current = site;
    let mut iterable = None;
    while let Some(parent) = current.parent() {
        if parent.kind() == "for_in_statement"
            && parent
                .child_by_field_name("left")
                .is_some_and(|l| l.kind() == "identifier" && cx.text(&l) == name)
            && parent
                .child_by_field_name("kind")
                .is_some_and(|k| cx.text(&k) == "const")
            && parent
                .child_by_field_name("operator")
                .is_some_and(|o| cx.text(&o) == "of")
        {
            iterable = parent
                .child_by_field_name("right")
                .filter(|r| r.kind() == "identifier");
            break;
        }
        if super::callable::is_callable(parent) {
            return Vec::new();
        }
        current = parent;
    }
    let Some(iterable) = iterable else {
        return Vec::new();
    };
    let iterable_name = cx.text(&iterable);
    // The nearest `const` binding the iterable that is visible at the loop:
    // declared directly in a block (or the program) that encloses it,
    // searched from the loop outward and across at most two enclosing
    // functions, as lexical scope reads it.
    let mut reads = Vec::new();
    let mut crossed = 0;
    let mut node = iterable;
    while let Some(parent) = node.parent() {
        if super::callable::is_callable(parent) {
            crossed += 1;
            if crossed > 2 {
                break;
            }
        }
        if matches!(parent.kind(), "statement_block" | "program") {
            let mut walk = parent.walk();
            let value = parent
                .named_children(&mut walk)
                .filter(|statement| {
                    statement.kind() == "lexical_declaration"
                        && statement.child(0).is_some_and(|k| cx.text(&k) == "const")
                })
                .flat_map(|statement| {
                    let mut walk = statement.walk();
                    statement
                        .named_children(&mut walk)
                        .filter(|d| d.kind() == "variable_declarator")
                        .collect::<Vec<_>>()
                })
                .find(|declarator| {
                    declarator
                        .child_by_field_name("name")
                        .is_some_and(|n| n.kind() == "identifier" && cx.text(&n) == iterable_name)
                });
            if let Some(declarator) = value {
                if let Some(value) = declarator.child_by_field_name("value") {
                    config_reads(cx, value, &mut reads);
                }
                break;
            }
        }
        node = parent;
    }
    reads
}

/// The interface `this.field` is declared as by its class: a field
/// `field: I` or a constructor parameter property `private field: I`.
fn this_field_type(cx: &FileCx<'_>, receiver: TsNode<'_>) -> Option<String> {
    if receiver.kind() != "member_expression"
        || receiver
            .child_by_field_name("object")
            .is_none_or(|o| o.kind() != "this")
    {
        return None;
    }
    let field = receiver
        .child_by_field_name("property")
        .filter(|p| p.kind() == "property_identifier")?;
    let field = cx.text(&field);
    let class = super::callable::this_class_node(receiver)?;
    let body = class.child_by_field_name("body")?;
    let mut walk = body.walk();
    for member in body.named_children(&mut walk) {
        match member.kind() {
            "public_field_definition" => {
                if member
                    .child_by_field_name("name")
                    .is_some_and(|n| cx.text(&n) == field)
                {
                    return named_interface(cx, member.child_by_field_name("type")?);
                }
            }
            "method_definition"
                if member
                    .child_by_field_name("name")
                    .is_some_and(|n| cx.text(&n) == "constructor") =>
            {
                let params = member.child_by_field_name("parameters")?;
                let mut walk = params.walk();
                for param in params.named_children(&mut walk) {
                    let mut inner = param.walk();
                    let property = param
                        .named_children(&mut inner)
                        .any(|c| matches!(c.kind(), "accessibility_modifier" | "readonly"));
                    if property
                        && param
                            .child_by_field_name("pattern")
                            .is_some_and(|p| p.kind() == "identifier" && cx.text(&p) == field)
                    {
                        return named_interface(cx, param.child_by_field_name("type")?);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// The methods of an object literal, by name: shorthand methods and
/// function-valued pairs.
pub(super) fn object_methods(cx: &FileCx<'_>, object: TsNode<'_>) -> BTreeMap<String, String> {
    let mut methods = BTreeMap::new();
    let mut walk = object.walk();
    for member in object.named_children(&mut walk) {
        let (key, function) = match member.kind() {
            "method_definition" => (member.child_by_field_name("name"), Some(member)),
            "pair" => (
                member.child_by_field_name("key"),
                member
                    .child_by_field_name("value")
                    .filter(|v| matches!(v.kind(), "arrow_function" | "function_expression")),
            ),
            _ => continue,
        };
        let (Some(key), Some(function)) =
            (key.filter(|k| k.kind() == "property_identifier"), function)
        else {
            continue;
        };
        methods.insert(cx.text(&key).to_string(), super::callable::id(cx, function));
    }
    methods
}

/// The file's implementing classes and strategy calls.
pub(super) fn sites(cx: &FileCx<'_>, nodes: &[TsNode<'_>], out: &mut Pending) {
    for node in nodes {
        match node.kind() {
            "variable_declarator" => {
                let (Some(name), Some(ty), Some(value)) = (
                    node.child_by_field_name("name")
                        .filter(|n| n.kind() == "identifier"),
                    node.child_by_field_name("type"),
                    node.child_by_field_name("value")
                        .filter(|v| v.kind() == "object"),
                ) else {
                    continue;
                };
                let Some(interface) = named_interface(cx, ty) else {
                    continue;
                };
                let methods = object_methods(cx, value);
                if !methods.is_empty() {
                    out.objects.push(ObjectImplementation {
                        interface,
                        name: cx.text(&name).to_string(),
                        methods,
                    });
                }
            }
            "function_declaration" | "method_definition" => {
                let (Some(name), Some(ty), Some(body)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("return_type"),
                    node.child_by_field_name("body"),
                ) else {
                    continue;
                };
                let Some(interface) = named_interface(cx, ty) else {
                    continue;
                };
                // Every `return {…}` of this function, in branches too, but
                // not those of nested functions.
                let mut returns = Vec::new();
                let mut stack = vec![body];
                while let Some(current) = stack.pop() {
                    if current.kind() == "return_statement" {
                        returns.push(current);
                        continue;
                    }
                    let mut walk = current.walk();
                    for child in current.named_children(&mut walk) {
                        if !super::callable::is_callable(child) {
                            stack.push(child);
                        }
                    }
                }
                returns.sort_by_key(|r| r.start_byte());
                for statement in returns {
                    let Some(object) = statement.named_child(0).filter(|v| v.kind() == "object")
                    else {
                        continue;
                    };
                    let methods = object_methods(cx, object);
                    if !methods.is_empty() {
                        out.objects.push(ObjectImplementation {
                            interface: interface.clone(),
                            name: cx.text(&name).to_string(),
                            methods,
                        });
                    }
                }
            }
            "interface_declaration" => {
                let (Some(owner), Some(body)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("body"),
                ) else {
                    continue;
                };
                let mut walk = body.walk();
                for member in body.named_children(&mut walk) {
                    if member.kind() != "property_signature" {
                        continue;
                    }
                    let (Some(prop), Some(ty)) = (
                        member
                            .child_by_field_name("name")
                            .filter(|n| n.kind() == "property_identifier"),
                        member.child_by_field_name("type"),
                    ) else {
                        continue;
                    };
                    if let Some(interface) = named_interface(cx, ty) {
                        out.properties.push(Property {
                            owner: cx.text(&owner).to_string(),
                            prop: cx.text(&prop).to_string(),
                            interface,
                        });
                    }
                }
            }
            // A configured instance: `stockLocationStrategy: new
            // MultiChannelStockLocationStrategy()` makes that class an
            // implementation of `StockLocationStrategy` though it only
            // `extends` a base (AC-1054). Only a pair inside the default
            // configuration (`const defaultConfig = { … }`) counts: a test
            // fixture or other local object configures nothing.
            "pair" if in_default_config(cx, *node) => {
                let (Some(key), Some(value)) = (
                    node.child_by_field_name("key")
                        .filter(|k| k.kind() == "property_identifier"),
                    node.child_by_field_name("value")
                        .filter(|v| v.kind() == "new_expression"),
                ) else {
                    continue;
                };
                let (Some(interface), Some(class)) = (
                    interface_of(cx.text(&key)),
                    value
                        .child_by_field_name("constructor")
                        .filter(|c| c.kind() == "identifier"),
                ) else {
                    continue;
                };
                out.configured.push(Configured {
                    interface,
                    class: cx.text(&class).to_string(),
                });
            }
            "class_declaration" => {
                let Some(class) = node.child_by_field_name("name") else {
                    continue;
                };
                out.classes
                    .push((cx.text(&class).to_string(), cx.path.to_string()));
                for interface in implemented(cx, *node) {
                    out.implementations.push(Implementation {
                        interface,
                        class: cx.text(&class).to_string(),
                        path: cx.path.to_string(),
                    });
                }
            }
            "call_expression" => {
                let Some(function) = node
                    .child_by_field_name("function")
                    .filter(|f| f.kind() == "member_expression")
                else {
                    continue;
                };
                let (Some(receiver), Some(method)) = (
                    function.child_by_field_name("object"),
                    function
                        .child_by_field_name("property")
                        .filter(|p| p.kind() == "property_identifier"),
                ) else {
                    continue;
                };
                let named = receiver_name(cx, receiver)
                    .and_then(interface_of)
                    .or_else(|| this_field_type(cx, receiver));
                let receiver = match named {
                    Some(interface) => Receiver::Interface(interface),
                    None if receiver.kind() == "identifier" => {
                        let reads = loop_config(cx, *node, cx.text(&receiver));
                        if reads.is_empty() {
                            continue;
                        }
                        Receiver::Config(reads)
                    }
                    None => continue,
                };
                let Some(src) = enclosing_symbol(cx, *node) else {
                    continue;
                };
                out.calls.push(Call {
                    src,
                    receiver,
                    method: cx.text(&method).to_string(),
                    evidence: EvidenceRef {
                        repo: cx.id.repo.into(),
                        path: cx.path.into(),
                        byte_start: node.start_byte() as u64,
                        byte_end: node.end_byte() as u64,
                        commit_sha: cx.id.commit.into(),
                    },
                });
            }
            _ => {}
        }
    }
}

/// Link each strategy call to the implementing classes' method.
pub(crate) fn complete(
    pending: Pending,
    repo: &str,
    known_symbols: &HashSet<String>,
    facts: &mut impl FactAccess,
) -> Result<(), GraphError> {
    // A configured class is an implementation when exactly one class of
    // that name is declared (AC-1054).
    let mut configured: Vec<Implementation> = Vec::new();
    for c in &pending.configured {
        let mut declared = pending.classes.iter().filter(|(name, _)| *name == c.class);
        if let (Some((_, path)), None) = (declared.next(), declared.next())
            && !pending
                .implementations
                .iter()
                .chain(&configured)
                .any(|i| i.interface == c.interface && i.class == c.class && i.path == *path)
        {
            configured.push(Implementation {
                interface: c.interface.clone(),
                class: c.class.clone(),
                path: path.clone(),
            });
        }
    }
    let mut by_interface: BTreeMap<&str, Vec<&Implementation>> = BTreeMap::new();
    for implementation in pending.implementations.iter().chain(&configured) {
        by_interface
            .entry(implementation.interface.as_str())
            .or_default()
            .push(implementation);
    }
    let mut objects: BTreeMap<&str, Vec<&ObjectImplementation>> = BTreeMap::new();
    for object in &pending.objects {
        objects
            .entry(object.interface.as_str())
            .or_default()
            .push(object);
    }
    let mut typed: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
    for property in &pending.properties {
        typed
            .entry((property.owner.as_str(), property.prop.as_str()))
            .or_default()
            .push(property.interface.as_str());
    }
    let mut edges = Vec::new();
    for call in &pending.calls {
        let interfaces: Vec<String> = match &call.receiver {
            Receiver::Interface(interface) => vec![interface.clone()],
            Receiver::Config(reads) => {
                let mut found: Vec<String> = Vec::new();
                for (owner, prop) in reads {
                    let mut chars = owner.chars();
                    let Some(first) = chars.next() else {
                        continue;
                    };
                    let owner = first.to_ascii_uppercase().to_string() + chars.as_str();
                    for interface in typed
                        .get(&(owner.as_str(), prop.as_str()))
                        .into_iter()
                        .flatten()
                    {
                        if !found.iter().any(|f| f == interface) {
                            found.push((*interface).to_string());
                        }
                    }
                }
                found
            }
        };
        // (target, implementation name, interface)
        let mut targets: Vec<(String, &str, &str)> = Vec::new();
        for interface in &interfaces {
            for i in by_interface.get(interface.as_str()).into_iter().flatten() {
                targets.push((
                    sym_id(repo, &i.path, &format!("{}.{}", i.class, call.method)),
                    i.class.as_str(),
                    interface.as_str(),
                ));
            }
            for o in objects.get(interface.as_str()).into_iter().flatten() {
                if let Some(dst) = o.methods.get(&call.method) {
                    targets.push((dst.clone(), o.name.as_str(), interface.as_str()));
                }
            }
        }
        targets.retain(|(dst, ..)| known_symbols.contains(dst) && *dst != call.src);
        targets.sort();
        targets.dedup_by(|a, b| a.0 == b.0);
        if targets.is_empty() || targets.len() > MAX_ALTERNATIVES {
            continue;
        }
        let alternatives = targets.len();
        for (dst, class, interface) in targets {
            let fact = format!(
                "CALLS {interface}.{} -> {dst} at {}",
                call.method, call.evidence.byte_start
            );
            let prov = Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::InferredStrong,
                vec![call.evidence.clone()],
                EXTRACTOR,
                fact.as_bytes(),
            )
            .expect("Deterministic/InferredStrong is within ceiling");
            edges.push(Edge {
                src: call.src.clone(),
                dst,
                label: "CALLS".into(),
                props: serde_json::json!({
                    "resolution": "strategy-implementation",
                    "interface": interface,
                    "implementation": class,
                    "alternatives": alternatives,
                    "prov": serde_json::to_value(prov).expect("provenance serializes"),
                }),
            });
        }
    }
    facts.append(Vec::new(), edges)
}

#[cfg(test)]
mod tests {
    use crate::{SourceId, extract_dir};

    // AC-1054 (T-1054): a class configured by name (`stockLocationStrategy:
    // new MultiChannelStockLocationStrategy()`) implements the interface
    // even when it only `extends` a base, so the strategy call reaches it;
    // a class declared twice, or configured under another name, does not.
    #[test]
    fn a_configured_class_implements_the_interface_it_is_configured_for() {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, text: &str| {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "src/config/catalog/multi-channel-stock-location-strategy.ts",
            "export class MultiChannelStockLocationStrategy extends BaseStockLocationStrategy {
    async getAvailableStock(ctx: any, id: any, levels: any[]) {
        return { stockOnHand: 0, stockAllocated: 0 };
    }
}
",
        );
        write(
            "src/config/tax/twice.ts",
            "export class TwiceTaxZoneStrategy { determineTaxZone() { return 1; } }\n",
        );
        write(
            "src/config/tax/twice-again.ts",
            "export class TwiceTaxZoneStrategy { determineTaxZone() { return 2; } }\n",
        );
        write(
            "src/config/default-config.ts",
            "export const defaultConfig = {
    catalogOptions: { stockLocationStrategy: new MultiChannelStockLocationStrategy() },
    taxOptions: { taxZoneStrategy: new TwiceTaxZoneStrategy(), other: new MultiChannelStockLocationStrategy() },
};
",
        );
        // A fixture's configuration is not the default one.
        write(
            "src/config/catalog/test-stock-location-strategy.ts",
            "export class TestStockLocationStrategy extends BaseStockLocationStrategy {
    async getAvailableStock() {
        return { stockOnHand: 1, stockAllocated: 0 };
    }
}
export const testConfig = {
    catalogOptions: { stockLocationStrategy: new TestStockLocationStrategy() },
};
export function configure() {
    const defaultConfig = { stockLocationStrategy: new TestStockLocationStrategy() };
    return defaultConfig;
}
",
        );
        write(
            "src/service/stock-level.service.ts",
            "export class StockLevelService {
    async getAvailableStock(ctx: any, id: any) {
        const { stockLocationStrategy, taxZoneStrategy } = this.configService.catalogOptions;
        taxZoneStrategy.determineTaxZone();
        return stockLocationStrategy.getAvailableStock(ctx, id, []);
    }
}
",
        );
        let out = extract_dir(
            dir.path(),
            &SourceId {
                repo: "repo",
                commit: "c0ffee",
            },
        )
        .unwrap();
        let edges: Vec<_> = out
            .edges
            .iter()
            .filter(|e| e.label == "CALLS" && e.props["resolution"] == "strategy-implementation")
            .collect();
        assert_eq!(edges.len(), 1, "{edges:#?}");
        assert_eq!(
            edges[0].dst,
            "sym:repo@src/config/catalog/multi-channel-stock-location-strategy.ts#MultiChannelStockLocationStrategy.getAvailableStock"
        );
        assert_eq!(edges[0].props["interface"], "StockLocationStrategy");
        assert_eq!(edges[0].props["alternatives"], 1);
    }

    // AC-0953: a call on a receiver named for an interface is linked to the
    // method of every repository class that implements it, InferredStrong
    // and counting the alternatives; so that method's guards are reached.
    // A receiver nothing implements, or a missing method, adds nothing.
    #[test]
    fn a_strategy_call_reaches_its_implementations() {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, text: &str| {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "src/config/tax/tax-zone-strategy.ts",
            "export interface TaxZoneStrategy { determineTaxZone(zones: Zone[], channel: Channel): Zone; }\n",
        );
        write(
            "src/config/tax/default-tax-zone-strategy.ts",
            "import { TaxZoneStrategy } from './tax-zone-strategy';
export class DefaultTaxZoneStrategy implements TaxZoneStrategy {
    determineTaxZone(zones: Zone[], channel: Channel): Zone {
        if (!channel.defaultTaxZone) {
            throw new Error('channel has no default tax zone');
        }
        return channel.defaultTaxZone;
    }
}
",
        );
        write(
            "src/config/tax/address-tax-zone-strategy.ts",
            "export class AddressTaxZoneStrategy implements TaxZoneStrategy<Order> {
    determineTaxZone(zones: Zone[], channel: Channel): Zone {
        return zones[0];
    }
}
",
        );
        write(
            "src/service/tax-rate.service.ts",
            "export class TaxRateService {
    constructor(private configService: ConfigService) {}
    getActiveTaxZone(zones: Zone[], channel: Channel) {
        const { taxZoneStrategy } = this.configService.taxOptions;
        const zone = taxZoneStrategy.determineTaxZone(zones, channel);
        this.configService.taxOptions.taxZoneStrategy.determineTaxZone(zones, channel);
        this.configService.taxOptions.shippingStrategy.choose(zones);
        taxZoneStrategy.notAMethod();
        return zone;
    }
}
",
        );
        let out = extract_dir(
            dir.path(),
            &SourceId {
                repo: "repo",
                commit: "c0ffee",
            },
        )
        .unwrap();
        let src = "sym:repo@src/service/tax-rate.service.ts#TaxRateService.getActiveTaxZone";
        let edges: Vec<_> = out
            .edges
            .iter()
            .filter(|e| e.label == "CALLS" && e.props["resolution"] == "strategy-implementation")
            .collect();
        // Two calls, each to both implementations.
        assert_eq!(edges.len(), 4, "{edges:#?}");
        for edge in &edges {
            assert_eq!(edge.src, src);
            assert_eq!(edge.props["interface"], "TaxZoneStrategy");
            assert_eq!(edge.props["alternatives"], 2);
            assert_eq!(
                edge.props["prov"]["confidence_tier"], "InferredStrong",
                "{}",
                edge.props
            );
        }
        let dst = "sym:repo@src/config/tax/default-tax-zone-strategy.ts#DefaultTaxZoneStrategy.determineTaxZone";
        assert!(edges.iter().any(|e| e.dst == dst));
        assert!(
            out.edges
                .iter()
                .any(|e| e.label == "GOVERNS" && e.dst == dst),
            "the default implementation's guard governs it"
        );
    }

    // AC-0977: an object literal declared with an interface type, or
    // returned by a function declared to return it, implements it; a
    // `for (const x of xs)` loop variable over configuration reads is typed
    // by the repository's `interface Owner { prop?: Array<I> }`, and
    // `this.field` by the class's `field: I`, so a call on either reaches
    // every class and typed object implementing `I`, even when `xs` is
    // bound in an enclosing function. An untyped object, a `let` iterable,
    // an unknown property and an undeclared field add nothing.
    #[test]
    fn a_config_loop_call_reaches_typed_objects() {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, text: &str| {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "src/config/vendure-config.ts",
            "export interface PaymentOptions {
    process?: Array<PaymentProcess<any>>;
    customPaymentProcess?: PaymentProcess<any>[];
    handlers: string[];
}
",
        );
        write(
            "src/config/payment/default-payment-process.ts",
            "export const defaultPaymentProcess: PaymentProcess<PaymentState> = {
    transitions: { Created: { to: ['Settled'] } },
    async onTransitionStart(fromState, toState, data) {
        if (toState === 'Settled' && !data.order.active) {
            return 'cannot settle a payment of an inactive order';
        }
    },
    onTransitionEnd: async (fromState, toState, data) => {},
};
export const untyped = {
    onTransitionStart(fromState, toState) { return true; },
};
",
        );
        write(
            "src/config/payment/custom-process.ts",
            "export function configureProcess(): PaymentProcess<PaymentState> {
    const process: PaymentProcess<PaymentState> = {
        onTransitionStart: (fromState, toState) => true,
    };
    return process;
}
",
        );
        write(
            "src/common/fsm.ts",
            "export class FSM {
    private hooks: StateMachineConfig;
    constructor(private config: StateMachineConfig, initialState: string) {}
    async transitionTo(state: string, data: any) {
        await this.config.onTransitionStart('a', state, data);
        this.hooks.onError('a', state, 'm');
        this.other.onTransitionStart('a', state, data);
    }
}
",
        );
        write(
            "src/service/payment-state-machine.ts",
            "export class PaymentStateMachine {
    constructor(private configService: ConfigService) {}
    private initConfig(): StateMachineConfig {
        const custom = this.configService.paymentOptions.customPaymentProcess ?? [];
        const processes = [...custom, ...(this.configService.paymentOptions.process ?? [])];
        let others = this.configService.paymentOptions.process ?? [];
        const handlers = this.configService.paymentOptions.handlers;
        return {
            onTransitionStart: async (fromState, toState, data) => {
                for (const process of processes) {
                    await process.onTransitionStart(fromState, toState, data);
                }
                for (const process of others) {
                    process.onTransitionEnd(fromState, toState, data);
                }
                for (const handler of handlers) {
                    handler.onTransitionStart();
                }
            },
        };
    }
}
",
        );
        let out = extract_dir(
            dir.path(),
            &SourceId {
                repo: "repo",
                commit: "c0ffee",
            },
        )
        .unwrap();
        let edges: Vec<_> = out
            .edges
            .iter()
            .filter(|e| e.label == "CALLS" && e.props["resolution"] == "strategy-implementation")
            .collect();
        // The FSM's typed `config` field reaches the hook of the object
        // `initConfig(): StateMachineConfig` returns; `hooks` names a
        // method that object lacks, and `other` is undeclared.
        let fsm: Vec<_> = edges
            .iter()
            .filter(|e| e.props["interface"] == "StateMachineConfig")
            .collect();
        assert_eq!(fsm.len(), 1, "{fsm:#?}");
        assert_eq!(fsm[0].src, "sym:repo@src/common/fsm.ts#FSM.transitionTo");
        assert_eq!(fsm[0].props["implementation"], "initConfig");
        assert!(
            fsm[0]
                .dst
                .contains("PaymentStateMachine.initConfig/object@"),
            "{}",
            fsm[0].dst
        );
        let edges: Vec<_> = edges
            .into_iter()
            .filter(|e| e.props["interface"] != "StateMachineConfig")
            .collect();
        // One call (the `processes` loop) to the two typed objects.
        assert_eq!(edges.len(), 2, "{edges:#?}");
        let mut implementations: Vec<&str> = edges
            .iter()
            .map(|e| e.props["implementation"].as_str().unwrap())
            .collect();
        implementations.sort();
        assert_eq!(implementations, ["defaultPaymentProcess", "process"]);
        for edge in &edges {
            assert_eq!(edge.props["interface"], "PaymentProcess");
            assert_eq!(edge.props["alternatives"], 2);
            assert_eq!(edge.props["prov"]["confidence_tier"], "InferredStrong");
            assert!(
                edge.src
                    .starts_with("sym:repo@src/service/payment-state-machine.ts#"),
                "{}",
                edge.src
            );
        }
        let default = edges
            .iter()
            .find(|e| e.props["implementation"] == "defaultPaymentProcess")
            .unwrap();
        assert!(
            default
                .dst
                .contains("default-payment-process.ts#defaultPaymentProcess@"),
            "{}",
            default.dst
        );
        assert!(
            out.edges
                .iter()
                .any(|e| e.label == "GOVERNS" && e.dst == default.dst),
            "the default object's guard governs its method"
        );
    }

    // AC-0977: the loop's iterable is the nearest visible `const` (a
    // sibling block's shadowing one is not); a branch's `return {…}` of a
    // typed factory implements the interface too; `this` inside an object
    // method is that object, not the class.
    #[test]
    fn config_dispatch_follows_lexical_scope() {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, text: &str| {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "src/config.ts",
            "export interface PaymentOptions { process?: Array<PaymentProcess<any>>; }
export interface RefundOptions { process?: Array<RefundProcess<any>>; }
export function makePayment(flag: boolean): PaymentProcess<any> {
    if (flag) {
        return { onTransitionStart(a, b) { return true; } };
    }
    return { onTransitionEnd(a, b) {} };
}
export const refund: RefundProcess<any> = { onTransitionStart(a, b) { return false; } };
",
        );
        write(
            "src/machine.ts",
            "export class Machine {
    constructor(private config: StateMachineConfig, private configService: ConfigService) {}
    run() {
        const processes = this.configService.paymentOptions.process;
        {
            const processes = this.configService.refundOptions.process;
        }
        for (const process of processes) {
            process.onTransitionStart(1, 2);
        }
        const hooks = {
            fire() {
                this.config.onTransitionStart(1, 2);
            },
        };
        return hooks;
    }
}
export function machineConfig(): StateMachineConfig {
    return { onTransitionStart(a, b) { return true; } };
}
",
        );
        let out = extract_dir(
            dir.path(),
            &SourceId {
                repo: "repo",
                commit: "c0ffee",
            },
        )
        .unwrap();
        let edges: Vec<_> = out
            .edges
            .iter()
            .filter(|e| e.label == "CALLS" && e.props["resolution"] == "strategy-implementation")
            .collect();
        // Only the payment process, through the branch's object.
        assert_eq!(edges.len(), 1, "{edges:#?}");
        assert_eq!(edges[0].props["interface"], "PaymentProcess");
        assert_eq!(edges[0].props["implementation"], "makePayment");
        assert_eq!(edges[0].src, "sym:repo@src/machine.ts#Machine.run");
    }
}
