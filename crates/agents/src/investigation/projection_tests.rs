use super::*;

pub(super) fn page(basis: &InvestigationInputLedger, note: &str) -> String {
    json!({
        "context": {
            "snapshot_id": basis.scope_snapshot_id,
            "view": "recovered_context",
            "total_selected": 1,
            "facts": [{
                "reference": basis.selected_facts[0].fact,
                "label": "Function",
                "properties": {"note": note, "producing_method": {"id": "fixture@1"}},
                "provenance": null,
                "provenance_problem": "missing",
                "confidence_tier": "Gap"
            }],
            "next_cursor": null
        },
        "evidence_options": [{
            "fact": basis.selected_facts[0].fact,
            "origin": "working_tree_unverified",
            "ranges": []
        }]
    })
    .to_string()
}

pub(super) fn bind_pages(basis: &mut InvestigationInputLedger, pages: &[String]) {
    let template = basis.queries[0].clone();
    basis.queries = pages
        .iter()
        .map(|raw| InvestigationQueryManifest {
            response_hash: content_hash(raw.as_bytes()),
            response_bytes: raw.len(),
            ..template.clone()
        })
        .collect();
}

// AC-0234: identical representations are shared without losing query coverage.
#[test]
fn compact_projection_deduplicates_complete_objects_and_preserves_query_metadata() {
    let mut basis = ledger();
    let raw = page(&basis, &"business behavior ".repeat(80));
    let pages = vec![raw.clone(), raw.clone(), raw];
    bind_pages(&mut basis, &pages);
    let projected = CompactInvestigationContext::prepare(&basis, &pages).unwrap();
    assert_eq!(projected.facts.len(), 1);
    assert_eq!(projected.evidence_options.len(), 1);
    assert_eq!(projected.queries.len(), 3);
    for query in &projected.queries {
        assert_eq!(query["context"]["fact_indexes"], json!([0]));
        assert_eq!(query["context"]["total_selected"], 1);
        assert!(query["context"]["next_cursor"].is_null());
        assert_eq!(query["evidence_option_indexes"], json!([0]));
    }
    assert_eq!(
        projected.facts[0]["properties"]["producing_method"]["id"],
        "fixture@1"
    );
    assert!(serde_json::to_vec(&projected).unwrap().len() < pages.iter().map(String::len).sum());
}

#[test]
fn compact_projection_preserves_conflicting_representations_of_same_typed_fact() {
    let mut basis = ledger();
    let pages = vec![
        page(&basis, "first observation"),
        page(&basis, "different observation"),
    ];
    bind_pages(&mut basis, &pages);
    let projected = CompactInvestigationContext::prepare(&basis, &pages).unwrap();
    assert_eq!(projected.facts.len(), 2);
    assert_eq!(projected.evidence_options.len(), 1);
    assert_eq!(projected.queries[0]["context"]["fact_indexes"], json!([0]));
    assert_eq!(projected.queries[1]["context"]["fact_indexes"], json!([1]));
}

#[test]
fn compact_projection_rejects_replaced_pages_and_mismatched_inventory_identity() {
    let mut basis = ledger();
    let pages = vec![page(&basis, "original")];
    bind_pages(&mut basis, &pages);
    assert!(CompactInvestigationContext::prepare(&basis, &[page(&basis, "replacement")]).is_err());
    let mut changed: serde_json::Value = serde_json::from_str(&pages[0]).unwrap();
    changed["evidence_options"][0]["fact"]["id"] = json!("node:other");
    let pages = vec![changed.to_string()];
    bind_pages(&mut basis, &pages);
    assert!(CompactInvestigationContext::prepare(&basis, &pages).is_err());
}
