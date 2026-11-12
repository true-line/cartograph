use super::*;

const SOURCE: ObservedSource = ObservedSource {
    repo: "local/p",
    commit: "abc",
    extractor: "t0.test",
};

fn site(path: &str, key: Option<&str>, symbol: Option<&str>, start: u64) -> ConfigReadSite {
    ConfigReadSite {
        key: key.map(str::to_string),
        language: "python",
        symbol: symbol.map(str::to_string),
        path: path.into(),
        byte_start: start,
        byte_end: start + 10,
    }
}

fn spans(value: &serde_json::Value) -> Vec<(u64, u64)> {
    value["prov"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["byte_start"].as_u64().unwrap(),
                e["byte_end"].as_u64().unwrap(),
            )
        })
        .collect()
}

// AC-0597 (ADR-0059 decisions 1-3): one node per file and key citing every
// read in byte order, one dynamic node per file with no key, and one
// `READS_CONFIG` edge per enclosing symbol (or the file) and node.
#[test]
fn config_reads_are_one_node_per_file_and_key() {
    let sites = [
        site("app/b.py", Some("DB_URL"), None, 5),
        site(
            "app/a.py",
            Some("DB_URL"),
            Some("sym:local/p@app/a.py#load"),
            90,
        ),
        site("app/a.py", None, Some("sym:local/p@app/a.py#load"), 60),
        site("app/a.py", Some("DB_URL"), None, 10),
        site(
            "app/a.py",
            Some("DB_URL"),
            Some("sym:local/p@app/a.py#load"),
            40,
        ),
        site("app/a.py", Some("API_KEY"), None, 20),
        site("app/a.py", None, None, 120),
    ];
    let (nodes, edges) = assemble(&SOURCE, &sites);
    let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "cfgread:local/p@app/a.py:env:API_KEY",
            "cfgread:local/p@app/a.py:env:DB_URL",
            "cfgread:local/p@app/a.py:env:?",
            "cfgread:local/p@app/b.py:env:DB_URL",
        ]
    );
    assert!(nodes.iter().all(|n| n.label == "ConfigRead"));
    let named = &nodes[1];
    assert_eq!(named.props["source"], "env");
    assert_eq!(named.props["key"], "DB_URL");
    assert_eq!(named.props["language"], "python");
    assert!(named.props.get("dynamic").is_none());
    assert_eq!(named.props["prov"]["tier"], "Deterministic");
    assert_eq!(named.props["prov"]["confidence_tier"], "Confirmed");
    assert_eq!(spans(&named.props), [(10, 20), (40, 50), (90, 100)]);
    let dynamic = &nodes[2];
    assert_eq!(dynamic.props["dynamic"], true);
    assert!(dynamic.props.get("key").is_none());
    assert_eq!(spans(&dynamic.props), [(60, 70), (120, 130)]);

    assert!(edges.iter().all(|e| e.label == "READS_CONFIG"));
    let pairs: Vec<(&str, &str)> = edges
        .iter()
        .map(|e| (e.src.as_str(), e.dst.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            (
                "file:local/p@app/a.py",
                "cfgread:local/p@app/a.py:env:API_KEY"
            ),
            (
                "file:local/p@app/a.py",
                "cfgread:local/p@app/a.py:env:DB_URL"
            ),
            (
                "sym:local/p@app/a.py#load",
                "cfgread:local/p@app/a.py:env:DB_URL"
            ),
            ("file:local/p@app/a.py", "cfgread:local/p@app/a.py:env:?"),
            (
                "sym:local/p@app/a.py#load",
                "cfgread:local/p@app/a.py:env:?"
            ),
            (
                "file:local/p@app/b.py",
                "cfgread:local/p@app/b.py:env:DB_URL"
            ),
        ]
    );
    // The symbol's two reads of one key are one edge citing both.
    assert_eq!(spans(&edges[2].props), [(40, 50), (90, 100)]);
}

// AC-0597 (requirement 6): the facts do not depend on the order the sites
// were collected in, and a site reported twice is one read.
#[test]
fn config_read_assembly_is_order_independent() {
    let mut sites = vec![
        site("b.py", Some("B"), None, 1),
        site("a.py", None, Some("sym:local/p@a.py#f"), 30),
        site("a.py", Some("A"), None, 2),
        site("a.py", Some("A"), Some("sym:local/p@a.py#f"), 50),
    ];
    let forward = assemble(&SOURCE, &sites);
    sites.reverse();
    sites.push(sites[0].clone());
    let backward = assemble(&SOURCE, &sites);
    assert_eq!(
        serde_json::to_string(&forward).unwrap(),
        serde_json::to_string(&backward).unwrap()
    );
}

// AC-0597 (ADR-0059 decision 5): a literal that is not a key name (empty,
// whitespace, over-long or secret-shaped) is never stored; the read is
// counted as a dynamic read.
#[test]
fn unrecordable_literal_key_is_a_dynamic_read() {
    let long = "K".repeat(MAX_IDENTITY + 1);
    let sites = [
        site("a.py", Some(""), None, 1),
        site("a.py", Some("TWO WORDS"), None, 20),
        site("a.py", Some(&long), None, 40),
    ];
    let (nodes, edges) = assemble(&SOURCE, &sites);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].id, "cfgread:local/p@a.py:env:?");
    assert!(nodes[0].props.get("key").is_none());
    assert_eq!(spans(&nodes[0].props), [(1, 11), (20, 30), (40, 50)]);
    assert_eq!(edges.len(), 1);
}
