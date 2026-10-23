use super::*;

fn site(provider: &str, identity: Option<IdentityExpr>, start: u64) -> ObservedSite {
    ObservedSite {
        family: ObservedFamily::HttpApi,
        provider: provider.into(),
        module: Some(provider.into()),
        identity,
        identity_source: None,
        identity_assumption: None,
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
