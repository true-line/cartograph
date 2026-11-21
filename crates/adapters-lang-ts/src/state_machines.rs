//! Declarative state machines (#2067): a `transitions` table written as data.
//!
//! Frameworks such as Vendure declare an entity's life cycle as an object
//! literal: each state names the states it may move to.
//!
//! ```ts
//! const orderProcess: OrderProcess<OrderState> = {
//!     transitions: {
//!         Created: { to: ['AddingItems', 'Draft'] },
//!         AddingItems: { to: ['ArrangingPayment', 'Cancelled'] },
//!         Cancelled: { to: [] },
//!     },
//! };
//! ```
//!
//! No guarded exit states such a table, so rule evidence never sees it. A
//! `transitions` property is a `StateMachine` fact only when its value is an
//! object of at least two entries, every entry is a state whose value is an
//! object with a `to` array, and every `to` element is a plain string
//! literal; any other shape (a spread, a computed key or target, a function)
//! is not read, so nothing is guessed. The machine is named by the outermost
//! declaration that holds it (`configureDefaultOrderProcess`,
//! `defaultPaymentProcess`), with the type its nearest declaration states,
//! and cites the table and each state's entry (Deterministic, `Confirmed`).

use super::{Extraction, FileCx, Node, TsNode, literal_string, object_entries};
use core_prov::ConfidenceTier;

/// The extractor id on every state-machine fact.
pub(crate) const EXTRACTOR: &str = "t0.ts-state-machine";

/// Most states read from one table; a larger one is not a life cycle.
const MAX_STATES: usize = 128;

/// One state's entry: its name, where it may move, and its span.
struct State<'t> {
    name: String,
    to: Vec<String>,
    node: TsNode<'t>,
}

/// The states of a `transitions` object, or `None` when it is not wholly a
/// table of that shape.
fn table<'t>(cx: &FileCx<'_>, value: TsNode<'t>) -> Option<Vec<State<'t>>> {
    if value.kind() != "object" {
        return None;
    }
    let mut walk = value.walk();
    let entries: Vec<TsNode<'t>> = value
        .named_children(&mut walk)
        .filter(|n| n.kind() != "comment")
        .collect();
    if entries.len() < 2 || entries.len() > MAX_STATES {
        return None;
    }
    let mut states = Vec::new();
    for entry in entries {
        if entry.kind() != "pair" {
            return None;
        }
        let key = entry.child_by_field_name("key")?;
        let name = match key.kind() {
            "property_identifier" => cx.text(&key).to_string(),
            "string" => literal_string(cx, key)?,
            _ => return None,
        };
        let state = entry.child_by_field_name("value")?;
        if state.kind() != "object" {
            return None;
        }
        let (_, to) = object_entries(cx, state)
            .into_iter()
            .find(|(key, _)| key == "to")?;
        if to.kind() != "array" {
            return None;
        }
        let mut walk = to.walk();
        let targets = to
            .named_children(&mut walk)
            .filter(|n| n.kind() != "comment")
            .map(|target| {
                (target.kind() == "string")
                    .then(|| literal_string(cx, target))
                    .flatten()
            })
            .collect::<Option<Vec<String>>>()?;
        states.push(State {
            name,
            to: targets,
            node: entry,
        });
    }
    Some(states)
}

/// The outermost declaration holding `node`, and the type its nearest
/// variable declaration states.
fn named(cx: &FileCx<'_>, node: TsNode<'_>) -> (Option<String>, Option<String>) {
    let (mut name, mut declared) = (None, None);
    let mut at = node.parent();
    while let Some(parent) = at {
        let found = match parent.kind() {
            "variable_declarator" => {
                if declared.is_none() {
                    declared = parent
                        .child_by_field_name("type")
                        .map(|t| cx.text(&t).trim_start_matches(':').trim().to_string());
                }
                parent.child_by_field_name("name")
            }
            "function_declaration" | "class_declaration" | "method_definition" => {
                parent.child_by_field_name("name")
            }
            _ => None,
        };
        if let Some(found) = found.filter(|n| {
            matches!(
                n.kind(),
                "identifier" | "type_identifier" | "property_identifier"
            )
        }) {
            name = Some(cx.text(&found).to_string());
        }
        at = parent.parent();
    }
    (name, declared)
}

/// Every declarative state machine in the file.
pub(super) fn extract(cx: &FileCx<'_>, nodes: &[TsNode<'_>], out: &mut Extraction) {
    for pair in nodes.iter().filter(|n| n.kind() == "pair") {
        let Some(key) = pair.child_by_field_name("key") else {
            continue;
        };
        let key_text = match key.kind() {
            "property_identifier" => cx.text(&key).to_string(),
            "string" => literal_string(cx, key).unwrap_or_default(),
            _ => continue,
        };
        if key_text != "transitions" {
            continue;
        }
        let Some(value) = pair.child_by_field_name("value") else {
            continue;
        };
        let Some(states) = table(cx, value) else {
            continue;
        };
        // The hooks beside the table (`onTransitionStart`), by name and the
        // start of their definition, which their callable's id ends with.
        let hooks: Vec<serde_json::Value> = pair
            .parent()
            .filter(|object| object.kind() == "object")
            .map(|object| {
                let mut walk = object.walk();
                object
                    .named_children(&mut walk)
                    .filter_map(|member| {
                        // A shorthand method's callable starts at the method;
                        // a property's at its function value.
                        let (key, start) = match member.kind() {
                            "method_definition" => {
                                (member.child_by_field_name("name")?, member.start_byte())
                            }
                            "pair" => {
                                let value = member.child_by_field_name("value")?;
                                matches!(
                                    value.kind(),
                                    "arrow_function" | "function_expression" | "function"
                                )
                                .then_some(())?;
                                (member.child_by_field_name("key")?, value.start_byte())
                            }
                            _ => return None,
                        };
                        let name = cx.text(&key);
                        name.starts_with("onTransition")
                            .then(|| serde_json::json!({ "name": name, "byte_start": start }))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (name, declared) = named(cx, *pair);
        let name = name.unwrap_or_else(|| "transitions".to_owned());
        let mut all: Vec<&str> = Vec::new();
        for state in &states {
            for s in std::iter::once(&state.name).chain(&state.to) {
                if !all.contains(&s.as_str()) {
                    all.push(s);
                }
            }
        }
        let transitions: Vec<serde_json::Value> = states
            .iter()
            .map(|state| {
                serde_json::json!({
                    "from": state.name,
                    "to": state.to,
                    "byte_start": state.node.start_byte(),
                    "byte_end": state.node.end_byte(),
                })
            })
            .collect();
        let id = format!(
            "statemachine:{}@{}#{}@{}",
            cx.id.repo,
            cx.path,
            name,
            value.start_byte()
        );
        out.nodes.push(Node {
            id,
            label: "StateMachine".into(),
            props: serde_json::json!({
                "name": name,
                "declared_type": declared,
                "file": cx.path,
                "states": all,
                "transitions": transitions,
                "hooks": hooks,
                "extractor": EXTRACTOR,
                "prov": cx.prov_with_confidence(
                    &value,
                    ConfidenceTier::Confirmed,
                    &format!("StateMachine {name}"),
                ),
            }),
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::{SourceId, extract_source};

    const ORDER: &str = "
import { OrderProcess } from './order-process';
export function configureDefaultOrderProcess(options: Options) {
    const orderProcess: OrderProcess<OrderState> = {
        transitions: {
            Created: { to: ['AddingItems', 'Draft'] },
            AddingItems: { to: ['ArrangingPayment', 'Cancelled'] },
            // A comment between states is fine.
            'ArrangingPayment': { to: ['PaymentSettled', 'AddingItems', 'Cancelled'] },
            Cancelled: { to: [] },
        },
        async onTransitionStart(fromState, toState, { order }) {
            if (toState === 'ArrangingPayment' && order.lines.length === 0) {
                return 'message.cannot-transition-to-payment-when-order-is-empty';
            }
        },
    };
    return orderProcess;
}
export const notATable = { transitions: { a: { to: [next] }, b: { to: [] } } };
export const spread = { transitions: { ...base, b: { to: [] } } };
export const single = { transitions: { a: { to: [] } } };
";

    // AC-0951: a `transitions` table of states with `to` string arrays is a
    // StateMachine named by its outermost declaration, with its declared
    // type, every state (sources and targets, in order) and each state's
    // allowed targets; a computed target, a spread or a single entry is not
    // read.
    #[test]
    fn a_transitions_table_is_a_state_machine() {
        let out = extract_source(
            ORDER.as_bytes(),
            "src/config/order/default-order-process.ts",
            &SourceId {
                repo: "repo",
                commit: "c0ffee",
            },
        )
        .unwrap();
        let machines: Vec<_> = out
            .nodes
            .iter()
            .filter(|n| n.label == "StateMachine")
            .collect();
        assert_eq!(machines.len(), 1, "{machines:#?}");
        let machine = &machines[0].props;
        assert_eq!(machine["name"], "configureDefaultOrderProcess");
        assert_eq!(machine["declared_type"], "OrderProcess<OrderState>");
        assert_eq!(
            machine["states"],
            serde_json::json!([
                "Created",
                "AddingItems",
                "Draft",
                "ArrangingPayment",
                "Cancelled",
                "PaymentSettled"
            ])
        );
        let transitions = machine["transitions"].as_array().unwrap();
        assert_eq!(transitions.len(), 4);
        assert_eq!(transitions[2]["from"], "ArrangingPayment");
        assert_eq!(
            transitions[2]["to"],
            serde_json::json!(["PaymentSettled", "AddingItems", "Cancelled"])
        );
        assert_eq!(transitions[3]["to"], serde_json::json!([]));
        let start = transitions[0]["byte_start"].as_u64().unwrap() as usize;
        assert!(ORDER[start..].starts_with("Created: { to:"));
        assert_eq!(
            machine["prov"]["evidence"][0]["path"],
            "src/config/order/default-order-process.ts"
        );
        // The guard in the hook beside the table is ordinary rule evidence
        // of the hook, whose id ends with the recorded name and start.
        let hook = &machine["hooks"][0];
        assert_eq!(hook["name"], "onTransitionStart");
        let suffix = format!(".onTransitionStart@{}", hook["byte_start"]);
        let governed = out
            .edges
            .iter()
            .find(|e| e.label == "GOVERNS" && e.dst.ends_with(&suffix))
            .expect("the hook's guard governs it");
        let rule = out.nodes.iter().find(|n| n.id == governed.src).unwrap();
        // Its condition names the move; a long message key is withheld by
        // the shared redaction policy.
        assert!(
            rule.props
                .to_string()
                .contains("toState === 'ArrangingPayment' && order.lines.length === 0"),
            "{}",
            rule.props
        );
    }
    // AC-0951: a hook written as a property (`onTransitionStart: async () =>`)
    // is recorded at its function's start, which its callable's id ends with.
    #[test]
    fn a_property_hook_is_recorded_at_its_function() {
        const PAYMENT: &str = "
export const defaultPaymentProcess: PaymentProcess<PaymentState> = {
    transitions: {
        Created: { to: ['Authorized', 'Declined'] },
        Declined: { to: [] },
    },
    onTransitionStart: async (fromState, toState, { payment }) => {
        if (toState === 'Authorized' && payment.amount <= 0) {
            return 'message.payment-amount-must-be-positive';
        }
    },
};
";
        let out = extract_source(
            PAYMENT.as_bytes(),
            "src/config/payment/default-payment-process.ts",
            &SourceId {
                repo: "repo",
                commit: "c0ffee",
            },
        )
        .unwrap();
        let machine = &out
            .nodes
            .iter()
            .find(|n| n.label == "StateMachine")
            .expect("a machine")
            .props;
        assert_eq!(machine["name"], "defaultPaymentProcess");
        let hook = &machine["hooks"][0];
        assert_eq!(hook["name"], "onTransitionStart");
        let start = hook["byte_start"].as_u64().unwrap() as usize;
        assert!(PAYMENT[start..].starts_with("async (fromState"), "{hook}");
        let suffix = format!(".onTransitionStart@{start}");
        assert!(
            out.edges
                .iter()
                .any(|e| e.label == "GOVERNS" && e.dst.ends_with(&suffix)),
            "{:#?}",
            out.edges
                .iter()
                .filter(|e| e.label == "GOVERNS")
                .collect::<Vec<_>>()
        );
    }
}
