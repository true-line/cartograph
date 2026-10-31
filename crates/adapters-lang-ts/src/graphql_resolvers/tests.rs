use crate::{SourceId, extract_source};
use core_graph::{Edge, Node};
use serde_json::json;

const IMPORTS: &str = "import { Permission } from '@vendure/common/lib/generated-shop-types';\nimport { Allow } from '../../decorators/allow.decorator';";
fn source() -> String {
    // Reduced fixture with exact import/decorator shapes from pinned Vendure
    // 2cc309de shop-order.resolver.ts; bodies are not the full target source.
    format!(
        "import {{ Query, Resolver }} from '@nestjs/graphql';\n{IMPORTS}\n@Resolver() class R {{ @Query()\n@Allow(Permission.Owner)\norder(Permission: unknown) {{}} }}"
    )
}
fn operation(source: &str) -> (Node, Edge) {
    let ex = extract_source(
        source.as_bytes(),
        "src/api/resolvers/shop/order.ts",
        &SourceId {
            repo: "fixture",
            commit: "fixed",
        },
    )
    .unwrap();
    (
        ex.nodes
            .into_iter()
            .find(|n| n.label == "Endpoint")
            .unwrap(),
        ex.edges.into_iter().find(|e| e.label == "HANDLES").unwrap(),
    )
}

// AC-0517: exact Allow/import evidence is separate from the unchanged operation proof.
#[test]
fn permission_site_cites_allow_and_named_imports_without_changing_operation_provenance() {
    let source = source();
    let (endpoint, handles) = operation(&source);
    let (before, before_handles) = operation(&source.replace("@Allow(Permission.Owner)", ""));
    assert_eq!(endpoint.id, before.id);
    assert_eq!(endpoint.props["prov"], before.props["prov"]);
    assert_eq!(handles, before_handles);
    assert_eq!(endpoint.props["permissions"], json!(["Owner"]));
    let site = &endpoint.props["permission_site"];
    assert_eq!(site["basis"], "import-bound-syntax");
    assert_eq!(site["prov"]["confidence_tier"], "Confirmed");
    assert_eq!(site["prov"]["tier"], "Deterministic");
    let slices: Vec<_> = site["prov"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            assert_eq!(e["repo"], "fixture");
            assert_eq!(e["commit_sha"], "fixed");
            &source[e["byte_start"].as_u64().unwrap() as usize
                ..e["byte_end"].as_u64().unwrap() as usize]
        })
        .collect();
    assert_eq!(
        slices,
        [
            "Permission",
            "'@vendure/common/lib/generated-shop-types'",
            "Allow",
            "'../../decorators/allow.decorator'",
            "@Allow(Permission.Owner)"
        ]
    );
    let changed = operation(&source.replace(
        "Permission.Owner)",
        "Permission.UpdateOrder, Permission.Owner)",
    ))
    .0;
    assert_eq!(changed.props["prov"], endpoint.props["prov"]);
    assert_eq!(
        changed.props["permissions"],
        json!(["UpdateOrder", "Owner"])
    );
    assert_ne!(
        changed.props["permission_site"]["prov"]["content_hash"],
        site["prov"]["content_hash"]
    );
    let plain = operation(&source.replace("@Allow(Permission.Owner)", "")).0;
    assert!(plain.props["permissions"].is_null());
    assert!(plain.props["permission_site"].is_null()); // never means anonymous
}

// AC-0517: aliases and actual module spellings retain argument order/duplicates as syntax.
#[test]
fn permission_site_supports_aliases_and_preserves_arguments() {
    let source = source().replace(IMPORTS, "import { Permission as P } from 'other-types';\nimport { Allow as Gate } from './custom';").replace("@Allow(Permission.Owner)", "@Gate(P.Read, P.Owner, P.Owner)");
    let endpoint = operation(&source).0;
    assert_eq!(
        endpoint.props["permissions"],
        json!(["Read", "Owner", "Owner"])
    );
    let site = &endpoint.props["permission_site"];
    assert_eq!(
        site["decorator"],
        json!({"exported":"Allow", "local":"Gate", "module":"./custom"})
    );
    assert_eq!(
        site["permission_import"],
        json!({"exported":"Permission", "local":"P", "module":"other-types"})
    );
    // A parameter inside the decorated method does not bind its decorator.
    let with_parameter = source.replace("order(Permission: unknown)", "order(P: unknown)");
    assert_eq!(operation(&with_parameter).0.props["permission_site"], *site);
}

// AC-0517: unsupported bindings or argument sites produce neither field, never a subset.
#[test]
fn permission_site_refuses_unproven_or_partial_observations() {
    for (case, before, after) in [
        (
            "unbound",
            "import { Permission } from '@vendure/common/lib/generated-shop-types';",
            "",
        ),
        ("wrong-export", "{ Permission }", "{ Other as Permission }"),
        ("default", "{ Permission }", "Permission"),
        ("namespace", "{ Permission }", "* as Permission"),
        (
            "type-import",
            "import { Permission }",
            "import type { Permission }",
        ),
        ("type-specifier", "{ Permission }", "{ type Permission }"),
        (
            "duplicate",
            "@Resolver()",
            "import { Permission } from 'duplicate'; @Resolver()",
        ),
        (
            "reassigned",
            "@Resolver()",
            "Permission = replacement; @Resolver()",
        ),
        (
            "allow-reassigned",
            "@Resolver()",
            "Allow = replacement; @Resolver()",
        ),
        (
            "shadowed",
            "@Resolver()",
            "function outer(Permission: unknown) { @Resolver()",
        ),
        (
            "allow-shadowed",
            "@Resolver()",
            "function outer(Allow: unknown) { @Resolver()",
        ),
        ("type-allow", "import { Allow }", "import type { Allow }"),
        ("empty", "@Allow(Permission.Owner)", "@Allow()"),
        ("mixed", "Permission.Owner)", "Permission.Owner, dynamic)"),
        ("computed", "Permission.Owner)", "Permission['Owner'])"),
        ("optional", "Permission.Owner)", "Permission?.Owner)"),
        ("spread", "Permission.Owner)", "...Permission.Owner)"),
        ("called", "Permission.Owner)", "Permission.Owner())"),
        (
            "multiple",
            "@Allow(Permission.Owner)",
            "@Allow(Permission.Owner) @Allow(Permission.UpdateOrder)",
        ),
        (
            "mixed-bindings",
            "@Allow(Permission.Owner)",
            "@Allow(Permission.Owner, P.Owner)",
        ),
    ] {
        let mut source = source();
        assert!(source.contains(before));
        source = source.replace(before, after);
        if case.ends_with("shadowed") {
            source.push('}');
        }
        if case == "mixed-bindings" {
            source = format!("import {{ Permission as P }} from 'other';\n{source}");
        }
        let endpoint = operation(&source).0;
        assert!(endpoint.props["permissions"].is_null(), "{case}");
        assert!(endpoint.props["permission_site"].is_null(), "{case}");
    }
}
