//! Calls through configured instances (#2165, AC-1307).
//!
//! Vendure configures much of its business logic as instances of a
//! repository class built from an object literal:
//!
//! ```ts
//! export const defaultShippingCalculator = new ShippingCalculator({
//!     code: 'default-shipping-calculator',
//!     calculate: (ctx, order, args) => { … },
//! });
//! export class ShippingCalculator {
//!     constructor(config: ShippingCalculatorConfig) {
//!         this.calculateFn = config.calculate;
//!     }
//!     calculate(ctx, order, args) { return this.calculateFn(ctx, order, args); }
//! }
//! // shipping-method.entity.ts
//! private readonly allCalculators: { [code: string]: ShippingCalculator } = {};
//! const calculator = this.allCalculators[this.calculator.code];
//! await calculator.calculate(ctx, order, args, this);
//! ```
//!
//! Two hops are linked, each an inferred `CALLS` with its own provenance:
//!
//! - **Forwarded member.** A class whose constructor stores a member of its
//!   first parameter (`this.f = config.k`) and a method that calls
//!   `this.f(…)`: the call is linked to the function `k` of every object
//!   literal the repository passes to `new Class({…})` outside test files.
//!   Resolution `configured-instance`, with the `implementation` (the
//!   instance's variable name, or `new Class@<byte>`) and the
//!   `alternatives` count.
//! - **Instance from a lookup.** A member call `x.m(…)` whose receiver is a
//!   `const x = this.field[…]` in the same function, where the class
//!   declares `field` as `{ [key: string]: C }`, `Record<string, C>`,
//!   `C[]` or `Array<C>` (`C` may be a union of names): the call is linked
//!   to `C.m` of each such repository class. Resolution
//!   `configured-lookup`.
//!
//! Both are InferredStrong: which instance runs is chosen by stored data at
//! runtime. A hop with more candidates than [`MAX_ALTERNATIVES`] is not
//! linked, and a hop with none adds nothing.

use super::{FileCx, TsNode, enclosing_symbol, sym_id};
use core_graph::facts::FactAccess;
use core_graph::{Edge, GraphError};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, HashSet};

/// The extractor id on every configured-instance edge.
pub(crate) const EXTRACTOR: &str = "t0.ts-configured-instance";

/// Most candidates one call is linked to.
const MAX_ALTERNATIVES: usize = 16;

/// `new Class({…})` with the object's functions.
#[derive(Debug, Clone)]
struct Instance {
    class: String,
    name: String,
    /// Member name -> its function's symbol id.
    methods: BTreeMap<String, String>,
}

/// `this.field = param.key` in `class`'s constructor.
#[derive(Debug, Clone)]
struct Forward {
    class: String,
    field: String,
    key: String,
}

/// A call `this.field(…)` in a method of `class`, or `x.method(…)` on an
/// instance looked up from a field typed by `classes`.
#[derive(Debug, Clone)]
enum Target {
    Field {
        class: String,
        field: String,
    },
    Lookup {
        classes: Vec<String>,
        method: String,
    },
}

#[derive(Debug, Clone)]
struct Call {
    src: String,
    target: Target,
    evidence: EvidenceRef,
}

/// A file's instances, forwards, classes and calls, awaiting the directory.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pending {
    instances: Vec<Instance>,
    forwards: Vec<Forward>,
    /// Class name -> the files declaring it.
    classes: Vec<(String, String)>,
    calls: Vec<Call>,
}

impl Pending {
    pub(crate) fn extend(&mut self, other: Self) {
        self.instances.extend(other.instances);
        self.forwards.extend(other.forwards);
        self.classes.extend(other.classes);
        self.calls.extend(other.calls);
    }

    pub(crate) fn retarget(&mut self, commit: &str) {
        for call in &mut self.calls {
            call.evidence.commit_sha = commit.to_string();
        }
    }
}

fn evidence(cx: &FileCx<'_>, node: TsNode<'_>) -> EvidenceRef {
    EvidenceRef {
        repo: cx.id.repo.into(),
        path: cx.path.into(),
        byte_start: node.start_byte() as u64,
        byte_end: node.end_byte() as u64,
        commit_sha: cx.id.commit.into(),
    }
}

/// `this.<name>` → `name`.
fn this_member<'s>(cx: &'s FileCx<'_>, node: TsNode<'_>) -> Option<&'s str> {
    if node.kind() != "member_expression"
        || node
            .child_by_field_name("object")
            .is_none_or(|o| o.kind() != "this")
    {
        return None;
    }
    node.child_by_field_name("property")
        .filter(|p| p.kind() == "property_identifier")
        .map(|p| cx.text(&p))
}

fn class_name(cx: &FileCx<'_>, class: TsNode<'_>) -> Option<String> {
    class
        .child_by_field_name("name")
        .map(|n| cx.text(&n).to_string())
}

/// The `this.f = param.k` assignments of a class's constructor, where
/// `param` is the constructor's first parameter.
fn forwards(cx: &FileCx<'_>, class: TsNode<'_>, name: &str, out: &mut Vec<Forward>) {
    let Some(body) = class.child_by_field_name("body") else {
        return;
    };
    let mut walk = body.walk();
    let Some(constructor) = body.named_children(&mut walk).find(|m| {
        m.kind() == "method_definition"
            && m.child_by_field_name("name")
                .is_some_and(|n| cx.text(&n) == "constructor")
    }) else {
        return;
    };
    let Some(param) = constructor
        .child_by_field_name("parameters")
        .and_then(|p| p.named_child(0))
        .and_then(|p| p.child_by_field_name("pattern"))
        .filter(|p| p.kind() == "identifier")
        .map(|p| cx.text(&p).to_string())
    else {
        return;
    };
    let Some(block) = constructor.child_by_field_name("body") else {
        return;
    };
    let mut stack = vec![block];
    while let Some(node) = stack.pop() {
        if super::callable::is_callable(node) {
            continue;
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if node.kind() != "assignment_expression" {
            continue;
        }
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            continue;
        };
        let Some(field) = this_member(cx, left) else {
            continue;
        };
        if right.kind() != "member_expression"
            || right
                .child_by_field_name("object")
                .is_none_or(|o| o.kind() != "identifier" || cx.text(&o) != param)
        {
            continue;
        }
        if let Some(key) = right
            .child_by_field_name("property")
            .filter(|p| p.kind() == "property_identifier")
        {
            out.push(Forward {
                class: name.to_string(),
                field: field.to_string(),
                key: cx.text(&key).to_string(),
            });
        }
    }
}

/// The class names a lookup field's type holds as values:
/// `{ [k: string]: C }`, `Record<string, C>`, `C[]`, `Array<C>`, each
/// with `C` a name or a union of names.
fn lookup_classes(cx: &FileCx<'_>, ty: TsNode<'_>) -> Vec<String> {
    fn names(cx: &FileCx<'_>, ty: TsNode<'_>, out: &mut Vec<String>) {
        match ty.kind() {
            "type_identifier" => out.push(cx.text(&ty).to_string()),
            "generic_type" => {
                if let Some(name) = ty.child_by_field_name("name") {
                    out.push(cx.text(&name).to_string());
                }
            }
            "union_type" | "parenthesized_type" | "type_annotation" => {
                let mut walk = ty.walk();
                for child in ty.named_children(&mut walk) {
                    names(cx, child, out);
                }
            }
            _ => {}
        }
    }
    let ty = match ty.kind() {
        "type_annotation" => match ty.named_child(0) {
            Some(inner) => inner,
            None => return Vec::new(),
        },
        _ => ty,
    };
    let mut out = Vec::new();
    match ty.kind() {
        "object_type" => {
            let mut walk = ty.walk();
            let members: Vec<_> = ty.named_children(&mut walk).collect();
            if let [signature] = members.as_slice()
                && signature.kind() == "index_signature"
                && let Some(value) = signature.child_by_field_name("type")
            {
                names(cx, value, &mut out);
            }
        }
        "array_type" => {
            if let Some(element) = ty.named_child(0) {
                names(cx, element, &mut out);
            }
        }
        "generic_type" => {
            let name = ty.child_by_field_name("name").map(|n| cx.text(&n));
            let args: Vec<_> = ty
                .child_by_field_name("type_arguments")
                .map(|args| {
                    let mut walk = args.walk();
                    args.named_children(&mut walk).collect()
                })
                .unwrap_or_default();
            match (name, args.as_slice()) {
                (Some("Array" | "ReadonlyArray"), [element]) => names(cx, *element, &mut out),
                (Some("Record"), [_, value]) => names(cx, *value, &mut out),
                _ => {}
            }
        }
        _ => {}
    }
    out
}

/// The classes a `const receiver = this.field[…]` in the call's function
/// looks up, by the field's declared type.
fn looked_up(cx: &FileCx<'_>, call: TsNode<'_>, receiver: &str) -> Vec<String> {
    // The declaration of `receiver` visible at the call: the nearest one
    // before it in an enclosing block, up to the call's function. A nearer
    // declaration of another value shadows the lookup.
    let mut field = None;
    let mut inner = call;
    'scopes: while let Some(scope) = inner.parent() {
        if super::callable::is_callable(scope) {
            break;
        }
        if matches!(scope.kind(), "statement_block" | "program" | "switch_case") {
            let mut walk = scope.walk();
            let before: Vec<_> = scope
                .named_children(&mut walk)
                .take_while(|statement| statement.end_byte() <= inner.start_byte())
                .collect();
            for statement in before.iter().rev() {
                if !matches!(
                    statement.kind(),
                    "lexical_declaration" | "variable_declaration"
                ) {
                    continue;
                }
                let mut walk = statement.walk();
                let Some(declarator) = statement.named_children(&mut walk).find(|d| {
                    d.kind() == "variable_declarator"
                        && d.child_by_field_name("name")
                            .is_some_and(|n| n.kind() == "identifier" && cx.text(&n) == receiver)
                }) else {
                    continue;
                };
                if statement.child(0).is_some_and(|k| cx.text(&k) == "const") {
                    field = declarator
                        .child_by_field_name("value")
                        .filter(|v| v.kind() == "subscript_expression")
                        .and_then(|v| v.child_by_field_name("object"))
                        .and_then(|o| this_member(cx, o))
                        .map(str::to_string);
                }
                break 'scopes;
            }
        }
        inner = scope;
    }
    let Some(field) = field else {
        return Vec::new();
    };
    let Some(class) = super::callable::this_class_node(call) else {
        return Vec::new();
    };
    let Some(class_body) = class.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut walk = class_body.walk();
    for member in class_body.named_children(&mut walk) {
        if member.kind() == "public_field_definition"
            && member
                .child_by_field_name("name")
                .is_some_and(|n| cx.text(&n) == field)
            && let Some(ty) = member.child_by_field_name("type")
        {
            return lookup_classes(cx, ty);
        }
    }
    Vec::new()
}

/// The file's configured instances, forwarding classes and calls.
pub(super) fn sites(cx: &FileCx<'_>, nodes: &[TsNode<'_>], out: &mut Pending) {
    let test_file = super::target_tests::is_test_file(cx.path);
    let forwarded_before = out.forwards.len();
    let calls_before = out.calls.len();
    for node in nodes {
        match node.kind() {
            "class_declaration" => {
                let Some(name) = class_name(cx, *node) else {
                    continue;
                };
                out.classes.push((name.clone(), cx.path.to_string()));
                forwards(cx, *node, &name, &mut out.forwards);
            }
            "new_expression" if !test_file => {
                let (Some(class), Some(object)) = (
                    node.child_by_field_name("constructor")
                        .filter(|c| c.kind() == "identifier"),
                    node.child_by_field_name("arguments")
                        .and_then(|a| a.named_child(0))
                        .filter(|a| a.kind() == "object"),
                ) else {
                    continue;
                };
                let methods = super::strategies::object_methods(cx, object);
                if methods.is_empty() {
                    continue;
                }
                let name = node
                    .parent()
                    .filter(|p| p.kind() == "variable_declarator")
                    .and_then(|p| p.child_by_field_name("name"))
                    .filter(|n| n.kind() == "identifier")
                    .map(|n| cx.text(&n).to_string())
                    .unwrap_or_else(|| format!("new {}@{}", cx.text(&class), node.start_byte()));
                out.instances.push(Instance {
                    class: cx.text(&class).to_string(),
                    name,
                    methods,
                });
            }
            "call_expression" => {
                let Some(function) = node
                    .child_by_field_name("function")
                    .filter(|f| f.kind() == "member_expression")
                else {
                    continue;
                };
                let target = if let Some(field) = this_member(cx, function) {
                    let Some(class) = super::callable::this_class_node(*node)
                        .and_then(|class| class_name(cx, class))
                    else {
                        continue;
                    };
                    Target::Field {
                        class,
                        field: field.to_string(),
                    }
                } else {
                    let (Some(receiver), Some(method)) = (
                        function
                            .child_by_field_name("object")
                            .filter(|o| o.kind() == "identifier"),
                        function
                            .child_by_field_name("property")
                            .filter(|p| p.kind() == "property_identifier"),
                    ) else {
                        continue;
                    };
                    let classes = looked_up(cx, *node, cx.text(&receiver));
                    if classes.is_empty() {
                        continue;
                    }
                    Target::Lookup {
                        classes,
                        method: cx.text(&method).to_string(),
                    }
                };
                let Some(src) = enclosing_symbol(cx, *node) else {
                    continue;
                };
                out.calls.push(Call {
                    src,
                    target,
                    evidence: evidence(cx, *node),
                });
            }
            _ => {}
        }
    }
    // A `this.f(…)` call is kept only when this file's class forwards `f`:
    // the constructor and the method are in the same class body.
    let forwarded: HashSet<(String, String)> = out.forwards[forwarded_before..]
        .iter()
        .map(|f| (f.class.clone(), f.field.clone()))
        .collect();
    let mut at = 0;
    out.calls.retain(|call| {
        at += 1;
        at <= calls_before
            || match &call.target {
                Target::Field { class, field } => {
                    forwarded.contains(&(class.clone(), field.clone()))
                }
                Target::Lookup { .. } => true,
            }
    });
}

/// Link each forwarded-member and lookup call to its candidates.
pub(crate) fn complete(
    pending: Pending,
    repo: &str,
    known_symbols: &HashSet<String>,
    facts: &mut impl FactAccess,
) -> Result<(), GraphError> {
    let mut keys: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
    for forward in &pending.forwards {
        keys.entry((forward.class.as_str(), forward.field.as_str()))
            .or_default()
            .push(forward.key.as_str());
    }
    let mut instances: BTreeMap<&str, Vec<&Instance>> = BTreeMap::new();
    for instance in &pending.instances {
        instances
            .entry(instance.class.as_str())
            .or_default()
            .push(instance);
    }
    let mut classes: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (class, path) in &pending.classes {
        classes
            .entry(class.as_str())
            .or_default()
            .push(path.as_str());
    }
    for paths in classes.values_mut() {
        paths.sort_unstable();
        paths.dedup();
    }
    // A class name declared in more than one file is not resolved: which
    // declaration a `new` or a field type names would need its import, so
    // neither hop through it is linked.
    let ambiguous = |name: &str| classes.get(name).is_some_and(|paths| paths.len() > 1);
    let mut edges = Vec::new();
    for call in &pending.calls {
        // (target, implementation, what the call names)
        let (resolution, mut targets): (&str, Vec<(String, String, String)>) = match &call.target {
            Target::Field { class, field } => {
                let mut targets = Vec::new();
                if ambiguous(class) {
                    continue;
                }
                for key in keys
                    .get(&(class.as_str(), field.as_str()))
                    .into_iter()
                    .flatten()
                {
                    for instance in instances.get(class.as_str()).into_iter().flatten() {
                        if let Some(dst) = instance.methods.get(*key) {
                            targets.push((
                                dst.clone(),
                                instance.name.clone(),
                                format!("{class}.{field}"),
                            ));
                        }
                    }
                }
                ("configured-instance", targets)
            }
            Target::Lookup {
                classes: names,
                method,
            } => {
                let mut targets = Vec::new();
                if names.iter().any(|name| ambiguous(name)) {
                    continue;
                }
                for name in names {
                    for path in classes.get(name.as_str()).into_iter().flatten() {
                        targets.push((
                            sym_id(repo, path, &format!("{name}.{method}")),
                            name.clone(),
                            format!("{name}.{method}"),
                        ));
                    }
                }
                ("configured-lookup", targets)
            }
        };
        targets.retain(|(dst, ..)| known_symbols.contains(dst) && *dst != call.src);
        targets.sort();
        targets.dedup_by(|a, b| a.0 == b.0);
        if targets.is_empty() || targets.len() > MAX_ALTERNATIVES {
            continue;
        }
        let alternatives = targets.len();
        for (dst, implementation, through) in targets {
            let fact = format!("CALLS {through} -> {dst} at {}", call.evidence.byte_start);
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
                    "resolution": resolution,
                    "through": through,
                    "implementation": implementation,
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

    // AC-1307: a configured instance's function is reached from the class
    // method that forwards to it, and an instance looked up from a typed
    // field reaches the class method; each hop is InferredStrong and counts
    // its alternatives, and the instance's guard governs its function.
    #[test]
    fn configured_instances_reach_their_functions() {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, text: &str| {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "src/config/shipping-calculator.ts",
            "export interface ShippingCalculatorConfig { code: string; calculate: CalculateFn; }
export class ShippingCalculator {
    private readonly calculateFn: CalculateFn;
    constructor(config: ShippingCalculatorConfig) {
        this.calculateFn = config.calculate;
    }
    calculate(ctx: Ctx, order: Order, args: Args) {
        return this.calculateFn(ctx, order, args);
    }
    describe() {
        return this.code();
    }
}
",
        );
        write(
            "src/config/default-shipping-calculator.ts",
            "import { ShippingCalculator } from './shipping-calculator';
export const defaultShippingCalculator = new ShippingCalculator({
    code: 'default',
    calculate: (ctx, order, args) => {
        if (args.rate < 0) {
            throw new Error('rate must not be negative');
        }
        return { price: args.rate };
    },
});
export const freeShipping = new ShippingCalculator({
    code: 'free',
    calculate(ctx, order, args) {
        return { price: 0 };
    },
});
",
        );
        // A test double is not a configured instance.
        write(
            "src/config/shipping-calculator.spec.ts",
            "const testCalculator = new ShippingCalculator({ code: 't', calculate: () => ({ price: 1 }) });\n",
        );
        write(
            "src/config/default-config.ts",
            "export const defaultConfig = { shippingOptions: { shippingCalculators: [defaultShippingCalculator] } };\n",
        );
        write(
            "src/entity/shipping-method.entity.ts",
            "export class ShippingMethod {
    private readonly allCalculators: { [code: string]: ShippingCalculator } = {};
    private readonly listed: Record<string, Unknown> = {};
    async apply(ctx: Ctx, order: Order) {
        const calculator = this.allCalculators[this.calculator.code];
        if (calculator) {
            return calculator.calculate(ctx, order, this.calculator.args);
        }
        const other = this.listed[this.calculator.code];
        return other.calculate(ctx, order, this.calculator.args);
    }
    describe(ctx: Ctx) {
        const calculator = this.fallback;
        calculator.calculate(ctx);
        {
            const calculator = this.allCalculators[this.calculator.code];
        }
    }
}
",
        );
        // Two unrelated classes share a name: neither hop through it is linked.
        for module in ["a", "b"] {
            write(
                &format!("src/{module}/rule.ts"),
                "export class Rule {
    constructor(config: RuleConfig) {
        this.checkFn = config.check;
    }
    check(input: Input) {
        return this.checkFn(input);
    }
}
export const rule = new Rule({ check: (input) => input.ok });
export class Rules {
    private readonly all: Record<string, Rule> = {};
    run(code: string, input: Input) {
        const found = this.all[code];
        return found.check(input);
    }
}
",
            );
        }
        let out = extract_dir(
            dir.path(),
            &SourceId {
                repo: "repo",
                commit: "c0ffee",
            },
        )
        .unwrap();
        let edges = |resolution: &str| -> Vec<_> {
            out.edges
                .iter()
                .filter(|e| e.label == "CALLS" && e.props["resolution"] == resolution)
                .collect()
        };

        let lookups = edges("configured-lookup");
        assert_eq!(lookups.len(), 1, "{lookups:#?}");
        let lookup = lookups[0];
        assert_eq!(
            lookup.src,
            "sym:repo@src/entity/shipping-method.entity.ts#ShippingMethod.apply"
        );
        assert_eq!(
            lookup.dst,
            "sym:repo@src/config/shipping-calculator.ts#ShippingCalculator.calculate"
        );
        assert_eq!(lookup.props["alternatives"], 1);
        assert_eq!(lookup.props["prov"]["confidence_tier"], "InferredStrong");

        let forwarded = edges("configured-instance");
        assert_eq!(forwarded.len(), 2, "{forwarded:#?}");
        let mut names: Vec<_> = forwarded
            .iter()
            .map(|e| e.props["implementation"].as_str().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["defaultShippingCalculator", "freeShipping"]);
        for edge in &forwarded {
            assert_eq!(
                edge.src,
                "sym:repo@src/config/shipping-calculator.ts#ShippingCalculator.calculate"
            );
            assert_eq!(edge.props["through"], "ShippingCalculator.calculateFn");
            assert_eq!(edge.props["alternatives"], 2);
            assert_eq!(edge.props["prov"]["confidence_tier"], "InferredStrong");
            assert!(
                edge.dst
                    .starts_with("sym:repo@src/config/default-shipping-calculator.ts#")
            );
        }
        // The default instance's guard governs the function it reaches.
        let default = forwarded
            .iter()
            .find(|e| e.props["implementation"] == "defaultShippingCalculator")
            .unwrap();
        assert!(
            out.edges
                .iter()
                .any(|e| e.label == "GOVERNS" && e.dst == default.dst),
            "{:#?}",
            out.edges
                .iter()
                .filter(|e| e.label == "GOVERNS")
                .collect::<Vec<_>>()
        );
    }
}
