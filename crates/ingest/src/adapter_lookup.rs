//! Adapter lookup (#1071, epic #998): for a coverage cell an ingested repo
//! leaves unsupported or partial, which installed source declares coverage?
//!
//! Two kinds of source count: the compiled-in `adapters-*` crates, through
//! their [`COVERAGE_MATRIX`] claims, and installed WASM plugins (ADR-0017),
//! through the cells their golden corpus declares. The lookup reads
//! declarations only. It takes them as plain data, so it installs, loads and
//! executes nothing, and it never reaches a registry.
//!
//! A claim is *verified* when the evidence behind it has passed: a built-in
//! `Supported` cell names a conformance fixture that the app crate's
//! conformance test extracts and asserts (see `matrix`), and a plugin's cells
//! are verified when its conformance gate passed for the exact artifact and
//! corpus bytes (`gate_key`); the gate also rejects a supported cell whose
//! named golden cases do not pass. Only `Supported` claims count as coverage:
//! a `Partial` claim is a shortfall, not an answer to one.

use crate::matrix::{COVERAGE_MATRIX, ReportedCell, Support};
use crate::preflight::{INSTALLED_ADAPTERS, PluginCoverage};
use serde::Serialize;
use std::collections::BTreeMap;

/// An installed source that declares a claim.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// A compiled-in `adapters-*` crate, by extractor id.
    Builtin {
        /// Extractor id as recorded in provenance.
        adapter: String,
    },
    /// An installed WASM plugin.
    Plugin {
        /// Plugin id (the artifact's file stem).
        id: String,
        /// The artifact's content hash, the plugin's version key.
        version: String,
    },
}

/// What the lookup found for one cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LookupStatus {
    /// Sources whose claim passes its conformance evidence.
    CoveredBy {
        /// The verified sources, in stable order.
        sources: Vec<Source>,
    },
    /// Sources that declare the cell without passing evidence; nothing
    /// verified covers it.
    DeclaredUnverified {
        /// The unverified sources, in stable order.
        sources: Vec<Source>,
    },
    /// No installed source declares the cell.
    None,
}

/// The lookup result for one unsupported or partial cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CellLookup {
    /// The cell as the repo's preflight reported it.
    pub cell: ReportedCell,
    /// Who declares coverage of it.
    #[serde(flatten)]
    pub status: LookupStatus,
}

/// A compiled-in adapter's matrix claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinClaim {
    /// Extractor id of the adapter making the claim.
    pub adapter: String,
    /// The matrix cell it claims.
    pub cell: ReportedCell,
}

/// An installed plugin's declared cells and whether its gate passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDeclaration {
    /// Plugin id.
    pub id: String,
    /// Content hash of the artifact.
    pub version: String,
    /// True when the conformance gate passed for these exact bytes and corpus.
    pub gate_passed: bool,
    /// Cells the plugin's manifest (golden corpus) declares.
    pub cells: Vec<ReportedCell>,
}

impl From<&PluginCoverage> for PluginDeclaration {
    /// A [`PluginCoverage`] only exists for a plugin that passed the gate.
    fn from(plugin: &PluginCoverage) -> Self {
        Self {
            id: plugin.plugin_id.clone(),
            version: plugin.version.clone(),
            gate_passed: true,
            cells: plugin.cells.clone(),
        }
    }
}

/// The compiled-in adapters' matrix claims, in table order. A cell whose
/// language no installed adapter names is skipped.
pub fn builtin_claims() -> Vec<BuiltinClaim> {
    COVERAGE_MATRIX
        .iter()
        .filter_map(|cell| {
            let adapter = INSTALLED_ADAPTERS
                .iter()
                .find(|adapter| adapter.language == cell.language)?;
            Some(BuiltinClaim {
                adapter: adapter.id.to_string(),
                cell: ReportedCell::from(cell),
            })
        })
        .collect()
}

/// Identity of a cell: languages match case-insensitively, as the gate does.
fn key(cell: &ReportedCell) -> (String, &str, &str) {
    (
        cell.language.to_ascii_lowercase(),
        &cell.framework,
        &cell.capability,
    )
}

/// Look up every distinct cell in `cells` that is not `Supported`. Pure and
/// deterministic: output is ordered by (language, framework, capability) and
/// each source list by source, whatever the input order.
pub fn lookup_coverage(
    cells: &[ReportedCell],
    builtins: &[BuiltinClaim],
    plugins: &[PluginDeclaration],
) -> Vec<CellLookup> {
    let mut wanted: BTreeMap<_, &ReportedCell> = BTreeMap::new();
    for cell in cells
        .iter()
        .filter(|cell| cell.support != Support::Supported)
    {
        wanted.entry(key(cell)).or_insert(cell);
    }
    wanted
        .into_iter()
        .map(|(wanted_key, cell)| {
            let mut verified = Vec::new();
            let mut unverified = Vec::new();
            for claim in builtins
                .iter()
                .filter(|claim| claim.cell.support == Support::Supported)
                .filter(|claim| key(&claim.cell) == wanted_key)
            {
                verified.push(Source::Builtin {
                    adapter: claim.adapter.clone(),
                });
            }
            for plugin in plugins {
                let declares = plugin
                    .cells
                    .iter()
                    .any(|claim| claim.support == Support::Supported && key(claim) == wanted_key);
                if declares {
                    let source = Source::Plugin {
                        id: plugin.id.clone(),
                        version: plugin.version.clone(),
                    };
                    if plugin.gate_passed {
                        verified.push(source);
                    } else {
                        unverified.push(source);
                    }
                }
            }
            verified.sort();
            verified.dedup();
            unverified.sort();
            unverified.dedup();
            let status = if !verified.is_empty() {
                LookupStatus::CoveredBy { sources: verified }
            } else if !unverified.is_empty() {
                LookupStatus::DeclaredUnverified {
                    sources: unverified,
                }
            } else {
                LookupStatus::None
            };
            CellLookup {
                cell: cell.clone(),
                status,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(language: &str, framework: &str, capability: &str, support: Support) -> ReportedCell {
        ReportedCell {
            language: language.into(),
            framework: framework.into(),
            capability: capability.into(),
            support,
            note: if support == Support::Supported {
                String::new()
            } else {
                "gap".into()
            },
        }
    }

    fn builtin(adapter: &str, cell: ReportedCell) -> BuiltinClaim {
        BuiltinClaim {
            adapter: adapter.into(),
            cell,
        }
    }

    fn plugin(id: &str, gate_passed: bool, cells: Vec<ReportedCell>) -> PluginDeclaration {
        PluginDeclaration {
            id: id.into(),
            version: format!("hash-{id}"),
            gate_passed,
            cells,
        }
    }

    fn gaps() -> Vec<ReportedCell> {
        vec![
            cell("Go", "gin", "http_endpoints", Support::Partial),
            cell("Ruby", "rails", "http_endpoints", Support::Unsupported),
            cell("Ruby", "sinatra", "http_endpoints", Support::Unsupported),
            cell("Ruby", "core", "imports", Support::Unsupported),
        ]
    }

    #[test]
    fn a_builtin_claim_covers_the_cell() {
        let builtins = [builtin(
            "t0.adapter-go",
            cell("go", "gin", "http_endpoints", Support::Supported),
        )];
        let found = lookup_coverage(&gaps()[..1], &builtins, &[]);
        assert_eq!(
            found[0].status,
            LookupStatus::CoveredBy {
                sources: vec![Source::Builtin {
                    adapter: "t0.adapter-go".into()
                }]
            }
        );
    }

    #[test]
    fn a_gated_plugin_claim_covers_the_cell() {
        let plugins = [plugin(
            "t0.plugin-ruby",
            true,
            vec![cell("Ruby", "rails", "http_endpoints", Support::Supported)],
        )];
        let found = lookup_coverage(&gaps(), &[], &plugins);
        let rails = found.iter().find(|l| l.cell.framework == "rails").unwrap();
        assert_eq!(
            rails.status,
            LookupStatus::CoveredBy {
                sources: vec![Source::Plugin {
                    id: "t0.plugin-ruby".into(),
                    version: "hash-t0.plugin-ruby".into()
                }]
            }
        );
    }

    #[test]
    fn a_plugin_claim_without_a_passing_gate_is_declared_unverified() {
        let plugins = [plugin(
            "t0.plugin-ruby",
            false,
            vec![cell("ruby", "rails", "http_endpoints", Support::Supported)],
        )];
        let found = lookup_coverage(&gaps(), &[], &plugins);
        let rails = found.iter().find(|l| l.cell.framework == "rails").unwrap();
        assert!(matches!(
            &rails.status,
            LookupStatus::DeclaredUnverified { sources } if sources.len() == 1
        ));
    }

    #[test]
    fn a_verified_claim_outranks_an_unverified_one() {
        let claim = cell("Ruby", "rails", "http_endpoints", Support::Supported);
        let plugins = [
            plugin("a.unverified", false, vec![claim.clone()]),
            plugin("b.verified", true, vec![claim]),
        ];
        let found = lookup_coverage(&gaps(), &[], &plugins);
        let rails = found.iter().find(|l| l.cell.framework == "rails").unwrap();
        assert!(matches!(
            &rails.status,
            LookupStatus::CoveredBy { sources }
                if sources == &[Source::Plugin {
                    id: "b.verified".into(),
                    version: "hash-b.verified".into()
                }]
        ));
    }

    #[test]
    fn an_uncovered_cell_is_none_and_partial_claims_do_not_cover() {
        let plugins = [plugin(
            "t0.plugin-ruby",
            true,
            vec![cell("Ruby", "sinatra", "http_endpoints", Support::Partial)],
        )];
        let found = lookup_coverage(&gaps(), &[], &plugins);
        let sinatra = found
            .iter()
            .find(|l| l.cell.framework == "sinatra")
            .unwrap();
        assert_eq!(sinatra.status, LookupStatus::None);
    }

    #[test]
    fn supported_cells_are_not_looked_up() {
        let cells = [cell("Go", "core", "calls", Support::Supported)];
        assert!(lookup_coverage(&cells, &[], &[]).is_empty());
    }

    #[test]
    fn order_is_stable_whatever_the_input_order() {
        let claim = cell("Ruby", "rails", "http_endpoints", Support::Supported);
        let plugins = vec![
            plugin("z.plugin", true, vec![claim.clone()]),
            plugin("a.plugin", true, vec![claim.clone()]),
        ];
        let builtins = vec![builtin("t0.adapter-ruby", claim)];
        let mut cells = gaps();
        cells.push(cells[0].clone());
        let forward = lookup_coverage(&cells, &builtins, &plugins);

        let mut reversed_cells = cells.clone();
        reversed_cells.reverse();
        let mut reversed_plugins = plugins.clone();
        reversed_plugins.reverse();
        assert_eq!(
            forward,
            lookup_coverage(&reversed_cells, &builtins, &reversed_plugins)
        );

        let ids: Vec<_> = forward
            .iter()
            .map(|l| (l.cell.language.as_str(), l.cell.framework.as_str()))
            .collect();
        assert_eq!(
            ids,
            [
                ("Go", "gin"),
                ("Ruby", "core"),
                ("Ruby", "rails"),
                ("Ruby", "sinatra")
            ]
        );
        let LookupStatus::CoveredBy { sources } = &forward[2].status else {
            panic!("rails is covered");
        };
        // Built-ins sort before plugins; plugins by id.
        assert!(matches!(sources[0], Source::Builtin { .. }));
        assert!(matches!(&sources[1], Source::Plugin { id, .. } if id == "a.plugin"));
        assert!(matches!(&sources[2], Source::Plugin { id, .. } if id == "z.plugin"));
    }

    #[test]
    fn builtin_claims_come_from_the_compiled_in_matrix() {
        let claims = builtin_claims();
        assert!(!claims.is_empty());
        assert!(claims.iter().all(|c| !c.adapter.is_empty()));
        assert!(claims.iter().any(|c| c.adapter == "t0.adapter-ts"));
    }

    #[test]
    fn the_serialized_shape_is_flat_and_tagged() {
        let plugins = [plugin(
            "p",
            true,
            vec![cell("Ruby", "rails", "http_endpoints", Support::Supported)],
        )];
        let found = lookup_coverage(&gaps(), &[], &plugins);
        let rails =
            serde_json::to_value(found.iter().find(|l| l.cell.framework == "rails").unwrap())
                .unwrap();
        assert_eq!(rails["status"], "covered_by");
        assert_eq!(rails["cell"]["capability"], "http_endpoints");
        assert_eq!(
            rails["sources"][0],
            serde_json::json!({"kind": "plugin", "id": "p", "version": "hash-p"})
        );
        let none = serde_json::to_value(
            found
                .iter()
                .find(|l| l.cell.framework == "sinatra")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(none["status"], "none");
        assert!(none.get("sources").is_none());
    }
}
