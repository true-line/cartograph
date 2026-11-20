use super::*;
use core_graph::StoreRevision;

const REPO: &str = "repo";
const COMMIT: &str = "2cc309deb5394af176314552ac7320a7b26a0104";
const NEST_PATH: &str = "src/orders.controller.ts";
const NEST_SOURCE: &[u8] = br#"
import {Controller, Get} from '@nestjs/common';
@Controller('health')
export class HealthController {
  @Get() check() {}
}
"#;

fn capability(fact: &OverlayFact) -> &Node {
    let OverlayItem::Node(node) = &fact.item else {
        panic!("Capability is a node")
    };
    node
}

const SPRING_JAVA_PATH: &str = "src/main/java/com/demo/OrderController.java";
const SPRING_JAVA_SOURCE: &[u8] = br#"package com.demo;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;
@RestController
@RequestMapping("/orders")
public class OrderController {
    @GetMapping
    public String list() { return "x"; }
    @PostMapping("/{id}/cancel")
    public String cancel(String id, int reason) { return "x"; }
    public String helper() { return "x"; }
    @RestController
    static class Audit {
        @GetMapping("/audit")
        public String audit() { return "x"; }
    }
}
interface OrderClient {
    @GetMapping("/remote/orders")
    String remote();
}
"#;
const SPRING_KOTLIN_PATH: &str = "src/main/kotlin/com/demo/StockController.kt";
const SPRING_KOTLIN_SOURCE: &[u8] = br#"package com.demo
import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.PostMapping
import org.springframework.web.bind.annotation.RequestMapping
import org.springframework.web.bind.annotation.RestController
@RestController
@RequestMapping("/stock")
class StockController {
    @GetMapping
    fun list(): String = "x"
    @PostMapping("/{sku}/reserve")
    fun reserve(sku: String): String = "x"
    fun helper(): String = "x"
}
@RestController
object StockProbe {
    @GetMapping("/stock/probe")
    fun probe(): String = "x"
}
"#;

const JAXRS_PATH: &str = "src/main/java/com/demo/InvoiceResource.java";
const JAXRS_SOURCE: &[u8] = br#"package com.demo;
import jakarta.ws.rs.GET;
import jakarta.ws.rs.POST;
import jakarta.ws.rs.Path;
@Path("/invoices")
public class InvoiceResource {
    @GET
    public String list() { return "x"; }
    @POST
    @Path("/{id}/void")
    public String cancel(String id) { return "x"; }
    public String helper() { return "x"; }
    @Path("/lines")
    public Object lines() { return null; }
}
@Path("/remote/invoices")
interface InvoiceClient {
    @GET
    String remote();
}
"#;

fn jaxrs_snapshot() -> RevisionedSnapshot {
    let facts = adapters_lang_java::extract_source(
        JAXRS_SOURCE,
        JAXRS_PATH,
        &adapters_lang_java::SourceId {
            repo: REPO,
            commit: COMMIT,
        },
    )
    .unwrap();
    RevisionedSnapshot {
        revision: StoreRevision {
            graph_instance: "fixture".into(),
            generation: 1,
        },
        graph: (facts.nodes, facts.edges),
    }
}

fn spring_snapshot(java: bool) -> RevisionedSnapshot {
    let (nodes, edges) = if java {
        let facts = adapters_lang_java::extract_source(
            SPRING_JAVA_SOURCE,
            SPRING_JAVA_PATH,
            &adapters_lang_java::SourceId {
                repo: REPO,
                commit: COMMIT,
            },
        )
        .unwrap();
        (facts.nodes, facts.edges)
    } else {
        let facts = adapters_lang_kotlin::extract_source(
            SPRING_KOTLIN_SOURCE,
            SPRING_KOTLIN_PATH,
            &adapters_lang_kotlin::SourceId {
                repo: REPO,
                commit: COMMIT,
            },
        )
        .unwrap();
        (facts.nodes, facts.edges)
    };
    RevisionedSnapshot {
        revision: StoreRevision {
            graph_instance: "fixture".into(),
            generation: 1,
        },
        graph: (nodes, edges),
    }
}

fn controller_titles(snapshot: &RevisionedSnapshot) -> Vec<String> {
    let mut titles: Vec<_> = derive_controller_capabilities(snapshot)
        .facts
        .iter()
        .map(|fact| capability(fact).props["title"].as_str().unwrap().to_owned())
        .collect();
    titles.sort();
    titles
}

// AC-0801: Spring controllers in Java and Kotlin give one Capability per
// class, citing the class Symbol with each endpoint, handler and relation.
#[test]
fn controller_capability_groups_real_spring_routes_by_class() {
    let java = spring_snapshot(true);
    let before = java.clone();
    let report = derive_controller_capabilities(&java);
    assert_eq!(java, before);
    assert_eq!(report.oversized_groups, 0);
    // The nested class is a controller of its own; the interface is not one.
    assert_eq!(
        controller_titles(&java),
        ["Manage orders", "View order audits"]
    );
    let orders = report
        .facts
        .iter()
        .find(|fact| capability(fact).props["title"] == "Manage orders")
        .unwrap();
    let node = capability(orders);
    assert!(node.id.starts_with("capability:http-controller:"));
    assert_eq!(node.props.as_object().unwrap().len(), 2);
    let sym = |name: &str| format!("sym:{REPO}@{SPRING_JAVA_PATH}#{name}");
    let list = format!("ep:{REPO}@GET:/orders");
    let cancel = format!("ep:{REPO}@POST:/orders/{{id}}/cancel");
    let expected: BTreeSet<_> = [
        BaseRef::Node(sym("OrderController")),
        BaseRef::Node(list.clone()),
        BaseRef::Node(cancel.clone()),
        BaseRef::Node(sym("OrderController.list()")),
        BaseRef::Node(sym("OrderController.cancel(String,int)")),
        BaseRef::Edge(list, sym("OrderController.list()"), "HANDLES".into()),
        BaseRef::Edge(
            cancel,
            sym("OrderController.cancel(String,int)"),
            "HANDLES".into(),
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(orders.cites, expected);
    let prov: Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
    assert!(prov.validate().is_ok());
    assert_eq!(prov.tier, Tier::Semantic);
    assert_eq!(prov.confidence_tier, ConfidenceTier::InferredWeak);
    assert_eq!(
        prov.method,
        ProducingMethod::Rule {
            rule: CAPABILITY_CONTROLLER_RULE.into(),
            version: CAPABILITY_CONTROLLER_RULE_VERSION.into()
        }
    );
    assert!(prov.evidence.iter().all(|e| e.path == SPRING_JAVA_PATH));
    let mut reversed = java.clone();
    reversed.graph.0.reverse();
    reversed.graph.1.reverse();
    assert_eq!(derive_controller_capabilities(&reversed), report);

    // Kotlin: the class is a controller; the object is not a plain class.
    let kotlin = spring_snapshot(false);
    let report = derive_controller_capabilities(&kotlin);
    assert_eq!(controller_titles(&kotlin), ["Manage stocks"]);
    let sym = |name: &str| format!("sym:{REPO}@{SPRING_KOTLIN_PATH}#{name}");
    let list = format!("ep:{REPO}@GET:/stock");
    let reserve = format!("ep:{REPO}@POST:/stock/{{sku}}/reserve");
    let expected: BTreeSet<_> = [
        BaseRef::Node(sym("StockController")),
        BaseRef::Node(list.clone()),
        BaseRef::Node(reserve.clone()),
        BaseRef::Node(sym("StockController.list")),
        BaseRef::Node(sym("StockController.reserve")),
        BaseRef::Edge(list, sym("StockController.list"), "HANDLES".into()),
        BaseRef::Edge(reserve, sym("StockController.reserve"), "HANDLES".into()),
    ]
    .into_iter()
    .collect();
    assert_eq!(report.facts[0].cites, expected);

    // Controllers from the three adapters in one snapshot: one group a class.
    let nest = adapters_lang_ts::extract_source(
        NEST_SOURCE,
        NEST_PATH,
        &adapters_lang_ts::SourceId {
            repo: REPO,
            commit: COMMIT,
        },
    )
    .unwrap();
    let mut all = java;
    all.graph
        .0
        .extend(kotlin.graph.0.into_iter().chain(nest.nodes));
    all.graph
        .1
        .extend(kotlin.graph.1.into_iter().chain(nest.edges));
    assert_eq!(
        controller_titles(&all),
        [
            "Manage orders",
            "Manage stocks",
            "View health",
            "View order audits"
        ]
    );
}

// AC-0801: a Spring route whose class, handler or support is not what the
// language's own adapter proves yields no candidate.
#[test]
fn controller_capability_omits_unproven_spring_routes() {
    let java = spring_snapshot(true);
    let audit_only = ["View order audits"];
    let class = format!("sym:{REPO}@{SPRING_JAVA_PATH}#OrderController");
    let list = format!("sym:{REPO}@{SPRING_JAVA_PATH}#OrderController.list()");
    let endpoints = [
        format!("ep:{REPO}@GET:/orders"),
        format!("ep:{REPO}@POST:/orders/{{id}}/cancel"),
    ];
    let edit = |change: &dyn Fn(&mut Node)| {
        let mut changed = java.clone();
        for node in &mut changed.graph.0 {
            change(node);
        }
        changed
    };
    // No Symbol for the class, or one declared twice.
    let mut missing = java.clone();
    missing.graph.0.retain(|node| node.id != class);
    assert_eq!(controller_titles(&missing), audit_only);
    let mut twice = java.clone();
    let copy = twice
        .graph
        .0
        .iter()
        .find(|n| n.id == class)
        .unwrap()
        .clone();
    twice.graph.0.push(copy);
    assert_eq!(controller_titles(&twice), audit_only);
    // The owner is an interface, a record or another language's class.
    for kind in ["interface", "record", "enum"] {
        assert_eq!(
            controller_titles(&edit(&|node| if node.id == class {
                node.props["kind"] = kind.into();
            })),
            audit_only
        );
    }
    assert_eq!(
        controller_titles(&edit(&|node| if node.id == class {
            node.props["language"] = "kotlin".into();
        })),
        audit_only
    );
    // The class Symbol comes from another file or another extractor.
    assert_eq!(
        controller_titles(&edit(&|node| if node.id == class {
            node.props["prov"]["evidence"][0]["path"] = "Other.java".into();
        })),
        audit_only
    );
    assert_eq!(
        controller_titles(&edit(&|node| if node.id == class {
            node.props["prov"]["extractor_id"] = "t0.adapter-ts".into();
        })),
        audit_only
    );
    // One route's handler is not a method, has no parameter list, or is not
    // the handler the endpoint names: that route alone is left out.
    let without_list = |snapshot: &RevisionedSnapshot| {
        let facts = derive_controller_capabilities(snapshot).facts;
        let orders = facts
            .iter()
            .find(|fact| capability(fact).props["title"] == "Manage orders")
            .unwrap();
        !orders.cites.contains(&BaseRef::Node(list.clone()))
            && orders.cites.contains(&BaseRef::Node(endpoints[1].clone()))
    };
    assert!(!without_list(&java));
    assert!(without_list(&edit(&|node| if node.id == list {
        node.props["kind"] = "function".into();
    })));
    assert!(without_list(&edit(&|node| if node.id == list {
        node.props["name"] = "OrderController.list".into();
    })));
    assert!(without_list(&edit(&|node| if node.id == list {
        node.props["receiver"] = "OrderController".into();
    })));
    assert!(without_list(&edit(&|node| if node.id == endpoints[0] {
        node.props["handler_sym"] = "sym:other".into();
    })));
    // A Java endpoint is not read with the TypeScript shape, nor the reverse.
    assert!(without_list(&edit(&|node| if node.id == endpoints[0] {
        node.props["framework"] = "nestjs".into();
    })));
    assert!(without_list(&edit(&|node| if node.id == endpoints[0] {
        node.props["language"] = "kotlin".into();
    })));
    assert!(without_list(&edit(&|node| if node.id == endpoints[0] {
        node.props["prov"]["basis"] = "structural".into();
    })));
}

// AC-0801: a JAX-RS resource class gives one Capability citing the class
// Symbol with each endpoint, handler and relation; a sub-resource locator and
// a resource method on an interface are route Gaps and give nothing.
#[test]
fn controller_capability_groups_real_jaxrs_routes_by_class() {
    let jaxrs = jaxrs_snapshot();
    let before = jaxrs.clone();
    let report = derive_controller_capabilities(&jaxrs);
    assert_eq!(jaxrs, before);
    assert_eq!(controller_titles(&jaxrs), ["Manage invoices"]);
    let sym = |name: &str| format!("sym:{REPO}@{JAXRS_PATH}#{name}");
    let list = format!("ep:{REPO}@GET:/invoices");
    let cancel = format!("ep:{REPO}@POST:/invoices/{{id}}/void");
    let expected: BTreeSet<_> = [
        BaseRef::Node(sym("InvoiceResource")),
        BaseRef::Node(list.clone()),
        BaseRef::Node(cancel.clone()),
        BaseRef::Node(sym("InvoiceResource.list()")),
        BaseRef::Node(sym("InvoiceResource.cancel(String)")),
        BaseRef::Edge(
            list.clone(),
            sym("InvoiceResource.list()"),
            "HANDLES".into(),
        ),
        BaseRef::Edge(
            cancel,
            sym("InvoiceResource.cancel(String)"),
            "HANDLES".into(),
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(report.facts[0].cites, expected);
    let prov: Provenance =
        serde_json::from_value(capability(&report.facts[0]).props["prov"].clone()).unwrap();
    assert!(prov.validate().is_ok());
    assert!(prov.evidence.iter().all(|e| e.path == JAXRS_PATH));
    let mut reversed = jaxrs.clone();
    reversed.graph.0.reverse();
    reversed.graph.1.reverse();
    assert_eq!(derive_controller_capabilities(&reversed), report);

    // The Java shape is not read for another language or a Kotlin adapter,
    // and the endpoint must name the handler its relation reaches.
    let edit = |change: &dyn Fn(&mut Node)| {
        let mut changed = jaxrs.clone();
        for node in &mut changed.graph.0 {
            change(node);
        }
        changed
    };
    let without_list = |snapshot: &RevisionedSnapshot| {
        let facts = derive_controller_capabilities(snapshot).facts;
        facts.len() == 1 && !facts[0].cites.contains(&BaseRef::Node(list.clone()))
    };
    assert!(!without_list(&jaxrs));
    assert!(without_list(&edit(&|node| if node.id == list {
        node.props["language"] = "kotlin".into();
    })));
    assert!(without_list(&edit(&|node| if node.id == list {
        node.props["handler_sym"] = "sym:other".into();
    })));
    assert!(without_list(&edit(&|node| if node.id == list {
        node.props["framework"] = "jax".into();
    })));
    let class = sym("InvoiceResource");
    assert!(
        controller_titles(&edit(&|node| if node.id == class {
            node.props["kind"] = "interface".into();
        }))
        .is_empty()
    );

    // With Spring in the same snapshot each class keeps its own group.
    let mut all = spring_snapshot(true);
    all.graph.0.extend(jaxrs.graph.0.clone());
    all.graph.1.extend(jaxrs.graph.1.clone());
    assert_eq!(
        controller_titles(&all),
        ["Manage invoices", "Manage orders", "View order audits"]
    );
}

// AC-0851: two controllers whose titles would collide each name the route
// prefix their routes share, whatever framework words told their classes
// apart; never their file when the routes tell them apart.
#[test]
fn colliding_controller_titles_name_their_files() {
    let source = |class: &str, prefix: &str| {
        format!(
            "package com.demo;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;
@RestController
@RequestMapping(\"/{prefix}/orders\")
public class {class} {{
    @GetMapping
    public String list() {{ return \"x\"; }}
}}
"
        )
    };
    let (mut nodes, mut edges) = (Vec::new(), Vec::new());
    for (class, prefix) in [
        ("ShopOrderController", "shop"),
        ("AdminOrderController", "admin"),
    ] {
        let facts = adapters_lang_java::extract_source(
            source(class, prefix).as_bytes(),
            &format!("src/main/java/com/demo/{class}.java"),
            &adapters_lang_java::SourceId {
                repo: REPO,
                commit: COMMIT,
            },
        )
        .unwrap();
        nodes.extend(facts.nodes);
        edges.extend(facts.edges);
    }
    let snapshot = RevisionedSnapshot {
        revision: StoreRevision {
            graph_instance: "fixture".into(),
            generation: 1,
        },
        graph: (nodes, edges),
    };
    assert_eq!(
        controller_titles(&snapshot),
        ["View orders (/admin/orders)", "View orders (/shop/orders)"]
    );
}

// AC-0851: colliding titles name their route prefix only when it tells each
// one apart; otherwise they keep naming their files.
#[test]
fn colliding_titles_fall_back_to_files_when_routes_do_not_tell_them_apart() {
    let pending = |file: &str, routes: &[&str]| Pending {
        title: "Manage dashboard".into(),
        places: [file.into(), "dashboard".into()],
        key: file.as_bytes().to_vec(),
        group: Group {
            cites: routes
                .iter()
                .map(|route| BaseRef::Node(format!("ep:repo@GET:{route}")))
                .collect(),
            ..Group::default()
        },
    };
    let titles = |pending: Vec<Pending>| {
        disambiguated(pending)
            .into_iter()
            .map(|(title, _, _)| title)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        titles(vec![
            pending(
                "app/dashboard/views/index.py",
                &["/dashboard/", "/dashboard/<int:id>"]
            ),
            pending(
                "app/dashboard/views/domain.py",
                &["/dashboard/domains", "/dashboard/domains/<int:id>/dns"]
            ),
        ]),
        [
            "Manage dashboard (/dashboard)",
            "Manage dashboard (/dashboard/domains)"
        ]
    );
    assert_eq!(
        titles(vec![
            pending("app/a.py", &["/dashboard/x", "/dashboard/y"]),
            pending("app/b.py", &["/dashboard/z", "/dashboard/w"]),
        ]),
        ["Manage dashboard (app/a.py)", "Manage dashboard (app/b.py)"]
    );
}
