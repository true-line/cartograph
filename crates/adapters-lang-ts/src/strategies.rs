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

/// One member call on a receiver named for an interface.
#[derive(Debug, Clone)]
struct Call {
    src: String,
    interface: String,
    method: String,
    evidence: EvidenceRef,
}

/// A file's strategy implementations and calls, awaiting the directory.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pending {
    implementations: Vec<Implementation>,
    calls: Vec<Call>,
}

impl Pending {
    pub(crate) fn extend(&mut self, other: Self) {
        self.implementations.extend(other.implementations);
        self.calls.extend(other.calls);
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

/// The file's implementing classes and strategy calls.
pub(super) fn sites(cx: &FileCx<'_>, nodes: &[TsNode<'_>], out: &mut Pending) {
    for node in nodes {
        match node.kind() {
            "class_declaration" => {
                let Some(class) = node.child_by_field_name("name") else {
                    continue;
                };
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
                let Some(interface) = receiver_name(cx, receiver).and_then(interface_of) else {
                    continue;
                };
                let Some(src) = enclosing_symbol(cx, *node) else {
                    continue;
                };
                out.calls.push(Call {
                    src,
                    interface,
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
    let mut by_interface: BTreeMap<&str, Vec<&Implementation>> = BTreeMap::new();
    for implementation in &pending.implementations {
        by_interface
            .entry(implementation.interface.as_str())
            .or_default()
            .push(implementation);
    }
    let mut edges = Vec::new();
    for call in &pending.calls {
        let Some(implementations) = by_interface.get(call.interface.as_str()) else {
            continue;
        };
        let targets: Vec<(String, &str)> = implementations
            .iter()
            .map(|i| {
                (
                    sym_id(repo, &i.path, &format!("{}.{}", i.class, call.method)),
                    i.class.as_str(),
                )
            })
            .filter(|(dst, _)| known_symbols.contains(dst) && *dst != call.src)
            .collect();
        if targets.is_empty() || targets.len() > MAX_ALTERNATIVES {
            continue;
        }
        let alternatives = targets.len();
        for (dst, class) in targets {
            let fact = format!(
                "CALLS {}.{} -> {dst} at {}",
                call.interface, call.method, call.evidence.byte_start
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
                    "interface": call.interface,
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
}
