use super::*;

fn site(provider: &str, identity: Option<IdentityExpr>, start: u64) -> ObservedSite {
    ObservedSite {
        family: ObservedFamily::HttpApi,
        provider: provider.into(),
        module: Some(provider.into()),
        identity,
        identity_source: None,
        identity_assumptions: Vec::new(),
        registry: None,
        symbol: None,
        path: "src/api.ts".into(),
        byte_start: start,
        byte_end: start + 10,
    }
}

// #734: the HTTP shape both producers share. Every client of one absolute
// origin uses one `http-api` resource and the edge names the client; a
// relative or missing URL names no resource; a computed URL is a Gap.
#[test]
fn http_clients_of_one_origin_share_one_resource() {
    let literal = |url: &str| Some(IdentityExpr::Literal(url.into()));
    let sites = [
        site("fetch", literal("https://api.example.com/v1/items?x=1"), 0),
        site("axios", literal("https://api.example.com/other"), 20),
        site("fetch", literal("/relative"), 40),
        site("ky", None, 60),
        site(
            "axios",
            Some(IdentityExpr::Computed("base + path".into())),
            80,
        ),
    ];
    let source = ObservedSource {
        repo: "local/p",
        commit: "abc",
        extractor: "t0.test",
    };
    let (nodes, edges) = assemble(&source, &sites);
    let resources: Vec<&Node> = nodes.iter().filter(|n| n.label == "Resource").collect();
    assert_eq!(resources.len(), 1);
    assert_eq!(
        resources[0].id,
        "res:local/p@observed:http-api:https://api.example.com"
    );
    assert_eq!(resources[0].props["provider"], "http-api");
    assert!(resources[0].props.get("module").is_none());
    let clients: Vec<&serde_json::Value> = edges.iter().map(|e| &e.props["client"]).collect();
    assert_eq!(clients, ["fetch", "axios", "axios"]);
    let gap = nodes.iter().find(|n| n.label == "Gap").unwrap();
    assert_eq!(gap.props["reason"], "runtime-computed http-api identity");
    assert_eq!(gap.props["provider"], "http-api");
}

// AC-0316 (#789 review): sites that merge into one resource keep the module
// any of them proves, whichever site created the resource and even when a
// Confirmed site replaces one that rested on an assumption.
#[test]
fn merged_resources_keep_a_proven_module() {
    let storage =
        |module: Option<&str>, assumption: Option<&'static str>, start: u64| ObservedSite {
            family: ObservedFamily::BrowserStorage,
            module: module.map(str::to_string),
            identity: Some(IdentityExpr::Literal("sync".into())),
            identity_assumptions: assumption.into_iter().collect(),
            ..site("webextension-storage", None, start)
        };
    let source = ObservedSource {
        repo: "local/p",
        commit: "abc",
        extractor: "t0.test",
    };
    for sites in [
        [storage(None, None, 0), storage(Some("polyfill"), None, 20)],
        [
            storage(
                Some("polyfill"),
                Some("identity_assumes_method_receivers"),
                0,
            ),
            storage(None, None, 20),
        ],
    ] {
        let (nodes, _) = assemble(&source, &sites);
        let resource = nodes.iter().find(|n| n.label == "Resource").unwrap();
        assert_eq!(resource.props["module"], "polyfill");
        assert_eq!(resource.props["prov"]["confidence_tier"], "Confirmed");
    }
}

// AC-0376 (#1116): a non-HTTP site whose instance is unresolved keeps its
// Gap and still proves its provider. Its second `USES` edge names the Gap,
// and the resource is marked `instance_unresolved` unless an identity-less
// site also proves it, in either order. A computed HTTP URL proves nothing
// more than its Gap.
#[test]
fn an_unresolved_instance_still_proves_its_provider() {
    let smtp = |identity: Option<IdentityExpr>, start: u64| ObservedSite {
        family: ObservedFamily::SdkClient,
        ..site("smtp", identity, start)
    };
    let computed = || Some(IdentityExpr::Computed("random.choice(servers)".into()));
    let withheld = || Some(IdentityExpr::Literal(String::new()));
    let source = ObservedSource {
        repo: "local/p",
        commit: "abc",
        extractor: "t0.test",
    };
    let provider = "res:local/p@observed:smtp";

    let (nodes, edges) = assemble(
        &source,
        &[
            smtp(computed(), 0),
            smtp(withheld(), 20),
            site("fetch", Some(IdentityExpr::Computed("base + p".into())), 40),
        ],
    );
    let gaps: Vec<&Node> = nodes.iter().filter(|n| n.label == "Gap").collect();
    assert_eq!(gaps.len(), 3);
    let resources: Vec<&Node> = nodes.iter().filter(|n| n.label == "Resource").collect();
    assert_eq!(resources.len(), 1, "{resources:?}");
    assert_eq!(resources[0].id, provider);
    assert_eq!(resources[0].props["instance_unresolved"], true);
    assert_eq!(resources[0].props["kind"], "sdk-client");
    assert!(!resources[0].props.to_string().contains("random.choice"));
    let to_provider: Vec<&Edge> = edges.iter().filter(|e| e.dst == provider).collect();
    assert_eq!(to_provider.len(), 2);
    for edge in &to_provider {
        assert_eq!(edge.props["instance_unresolved"], true);
        assert!(gaps.iter().any(|g| g.id == edge.props["gap"]), "{edge:?}");
    }
    // Each Gap keeps its own edge; HTTP adds no resource.
    assert_eq!(edges.len(), 5);

    for sites in [
        [smtp(None, 0), smtp(computed(), 20)],
        [smtp(computed(), 0), smtp(None, 20)],
    ] {
        let (nodes, _) = assemble(&source, &sites);
        let resource = nodes.iter().find(|n| n.id == provider).unwrap();
        assert!(
            resource.props.get("instance_unresolved").is_none(),
            "{resource:?}"
        );
    }
}
