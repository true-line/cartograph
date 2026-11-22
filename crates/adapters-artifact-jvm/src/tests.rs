use super::*;

const LEDGER: &[u8] = include_bytes!("../tests/fixtures/classes/com/acme/pay/Ledger.class");
const STORE: &[u8] = include_bytes!("../tests/fixtures/classes/com/acme/pay/Ledger$Store.class");
const AUDIT: &[u8] = include_bytes!("../tests/fixtures/classes/com/acme/pay/Ledger$Audit.class");
const ENTRY: &[u8] = include_bytes!("../tests/fixtures/classes/com/acme/pay/Ledger$Entry.class");
const KIND: &[u8] = include_bytes!("../tests/fixtures/classes/com/acme/pay/Ledger$Kind.class");

const REPO: &str = "acme/pay";
const DIR: &str = "lib/classes/com/acme/pay";

fn id() -> SourceId<'static> {
    SourceId {
        repo: REPO,
        commit: "workdir",
    }
}

fn files() -> Vec<(String, Vec<u8>)> {
    vec![
        (format!("{DIR}/Ledger$Store.class"), STORE.to_vec()),
        (format!("{DIR}/Ledger.class"), LEDGER.to_vec()),
        (format!("{DIR}/Ledger$Audit.class"), AUDIT.to_vec()),
        (format!("{DIR}/Ledger$Entry.class"), ENTRY.to_vec()),
        (format!("{DIR}/Ledger$Kind.class"), KIND.to_vec()),
    ]
}

fn sym(file: &str, name: &str) -> String {
    format!("sym:{REPO}@{DIR}/{file}#{name}")
}

fn node<'a>(out: &'a Extraction, id: &str) -> &'a Node {
    out.nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| {
            panic!(
                "no node {id}: {:#?}",
                out.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
            )
        })
}

fn calls(out: &Extraction, src: &str) -> Vec<(String, String, Option<String>)> {
    out.edges
        .iter()
        .filter(|edge| edge.label == "CALLS" && edge.src == src)
        .map(|edge| {
            (
                edge.dst.clone(),
                edge.props["opcode"].as_str().unwrap().to_string(),
                edge.props["via"].as_str().map(str::to_string),
            )
        })
        .collect()
}

/// AC-1215: classes, methods and constructors of compiled class files are
/// T0 Confirmed Symbols with artifact evidence, named as the Java source
/// adapter names them.
#[test]
fn class_files_are_symbols_with_artifact_evidence() {
    let out = extract(&id(), &files());
    let ledger = node(&out, &sym("Ledger.class", "Ledger"));
    assert_eq!(ledger.label, "Symbol");
    assert_eq!(ledger.props["kind"], "class");
    assert_eq!(ledger.props["binary_name"], "com.acme.pay.Ledger");
    let store = node(&out, &sym("Ledger$Store.class", "Ledger.Store"));
    assert_eq!(store.props["kind"], "class");
    assert_eq!(store.props["binary_name"], "com.acme.pay.Ledger$Store");
    assert_eq!(
        node(&out, &sym("Ledger$Kind.class", "Ledger.Kind")).props["kind"],
        "enum"
    );
    // Constructors list the parameters the source declares: no enclosing
    // instance for an inner class, no name and ordinal for an enum.
    node(&out, &sym("Ledger$Entry.class", "Ledger.Entry.Entry(int)"));
    node(&out, &sym("Ledger$Kind.class", "Ledger.Kind.Kind(int)"));
    node(&out, &sym("Ledger$Store.class", "Ledger.Store.Store()"));
    let audit = node(&out, &sym("Ledger$Audit.class", "Ledger.Audit"));
    assert_eq!(audit.props["kind"], "interface");
    for name in [
        "Ledger.Ledger()",
        "Ledger.total(List,int)",
        "Ledger.open()",
        "Ledger.post(Ledger.Audit,String)",
    ] {
        let method = node(&out, &sym("Ledger.class", name));
        assert_eq!(method.label, "Symbol");
    }
    assert_eq!(
        node(&out, &sym("Ledger.class", "Ledger.Ledger()")).props["kind"],
        "constructor"
    );
    node(
        &out,
        &sym("Ledger$Store.class", "Ledger.Store.amount(String)"),
    );
    node(
        &out,
        &sym("Ledger$Audit.class", "Ledger.Audit.record(String)"),
    );
    // No static initializer, lambda body or other synthetic method.
    assert!(
        out.nodes
            .iter()
            .all(|n| !n.id.contains("lambda$") && !n.id.contains("<clinit>")),
        "{:#?}",
        out.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
    );

    let prov: Provenance = serde_json::from_value(
        node(&out, &sym("Ledger.class", "Ledger.open()")).props["prov"].clone(),
    )
    .unwrap();
    assert!(prov.validate().is_ok());
    assert_eq!(prov.tier, Tier::Deterministic);
    assert_eq!(prov.confidence_tier, ConfidenceTier::Confirmed);
    assert!(prov.evidence.is_empty());
    let EvidenceBasis::Artifact { refs } = prov.basis() else {
        panic!("artifact basis");
    };
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].digest, digest(LEDGER));
    assert_eq!(refs[0].kind, "jvm-class");
    let ArtifactLocator::Offset { start, end } = refs[0].at else {
        panic!("offset");
    };
    assert!(start < end && end as usize <= LEDGER.len());

    let file = node(&out, &format!("file:{REPO}@{DIR}/Ledger.class"));
    assert_eq!(file.props["language"], "jvm-class");
    assert_eq!(file.props["artifact_digest"], digest(LEDGER));
}

/// AC-1215: call instructions between read methods are CALLS edges citing
/// the instruction's bytes; a lambda's calls belong to its enclosing method;
/// calls into classes not read (the JDK) are not edges.
#[test]
fn call_instructions_between_read_methods_are_calls() {
    let out = extract(&id(), &files());
    let total = calls(&out, &sym("Ledger.class", "Ledger.total(List,int)"));
    assert_eq!(
        total,
        vec![(
            sym("Ledger$Store.class", "Ledger.Store.amount(String)"),
            "invokevirtual".into(),
            None
        )]
    );
    let open = calls(&out, &sym("Ledger.class", "Ledger.open()"));
    assert_eq!(
        open,
        vec![(
            sym("Ledger.class", "Ledger.Ledger()"),
            "invokespecial".into(),
            None
        )]
    );
    let post = calls(
        &out,
        &sym("Ledger.class", "Ledger.post(Ledger.Audit,String)"),
    );
    let record = sym("Ledger$Audit.class", "Ledger.Audit.record(String)");
    assert_eq!(post.iter().filter(|c| c.0 == record).count(), 3, "{post:?}");
    assert!(
        post.iter()
            .all(|c| c.0 != record || c.1 == "invokeinterface")
    );
    assert!(post.contains(&(
        sym("Ledger$Store.class", "Ledger.Store.amount(String)"),
        "invokevirtual".into(),
        Some("lambda".into())
    )));
    assert!(post.contains(&(
        sym("Ledger$Store.class", "Ledger.Store.amount(String)"),
        "invokedynamic".into(),
        Some("method_reference".into())
    )));
    // The constructor calls Store's constructor; Object's is not read.
    assert_eq!(
        calls(&out, &sym("Ledger.class", "Ledger.Ledger()")),
        vec![(
            sym("Ledger$Store.class", "Ledger.Store.Store()"),
            "invokespecial".into(),
            None
        )]
    );
    for edge in out.edges.iter().filter(|e| e.label == "CALLS") {
        let prov: Provenance = serde_json::from_value(edge.props["prov"].clone()).unwrap();
        let EvidenceBasis::Artifact { refs } = prov.basis() else {
            panic!("artifact basis");
        };
        let ArtifactLocator::Offset { start, end } = refs[0].at else {
            panic!("offset");
        };
        let bytes = if edge.src.contains("Ledger$Store") {
            STORE
        } else {
            LEDGER
        };
        let opcode = bytes[start as usize];
        assert!((0xb6..=0xba).contains(&opcode), "{edge:?}");
        assert_eq!(end - start, if opcode >= 0xb9 { 5 } else { 3 });
    }
}

/// AC-1215: referenced classes outside the class's own package and
/// `java.lang` are IMPORTS edges from the class file.
#[test]
fn referenced_classes_are_imports() {
    let out = extract(&id(), &files());
    let imports: BTreeSet<&str> = out
        .edges
        .iter()
        .filter(|e| e.label == "IMPORTS" && e.src == format!("file:{REPO}@{DIR}/Ledger.class"))
        .map(|e| e.dst.as_str())
        .collect();
    assert!(imports.contains("mod:java.util.List"), "{imports:?}");
    // Named only in a method descriptor, never as a class constant.
    assert!(imports.contains("mod:java.time.Clock"), "{imports:?}");
    assert!(
        imports.contains("mod:java.util.function.Supplier"),
        "{imports:?}"
    );
    assert!(
        imports
            .iter()
            .all(|m| !m.starts_with("mod:java.lang.") || m.matches('.').count() > 2)
    );
    assert!(
        imports.iter().all(|m| !m.starts_with("mod:com.acme.pay")),
        "{imports:?}"
    );
}

/// AC-1215: the same files in another order give the same facts.
#[test]
fn extraction_is_order_independent() {
    let mut reversed = files();
    reversed.reverse();
    assert_eq!(extract(&id(), &files()), extract(&id(), &reversed));
}

/// AC-1215: a file the reader cannot walk whole is one Gap naming why, and
/// states no other fact.
#[test]
fn unreadable_class_files_are_gaps() {
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("Bad.class", b"not a class".to_vec(), "magic"),
        (
            "Cut.class",
            LEDGER[..LEDGER.len() / 2].to_vec(),
            "truncated",
        ),
        ("Tail.class", [LEDGER, b"x"].concat(), "trailing_bytes"),
        (
            // An empty constant pool, then a class index into it.
            "Index.class",
            vec![
                0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 61, 0, 1, 0, 0x21, 0, 5, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0,
            ],
            "constant_index",
        ),
    ];
    for (name, bytes, code) in cases {
        let path = format!("{DIR}/{name}");
        let out = extract(&id(), &[(path.clone(), bytes.clone())]);
        assert_eq!(out.nodes.len(), 1, "{name}: {:#?}", out.nodes);
        assert!(out.edges.is_empty());
        let gap = &out.nodes[0];
        assert_eq!(gap.label, "Gap");
        assert_eq!(gap.props["reason_code"], code, "{name}");
        assert_eq!(gap.props["path"], path.as_str());
        let prov: Provenance = serde_json::from_value(gap.props["prov"].clone()).unwrap();
        assert_eq!(prov.confidence_tier, ConfidenceTier::Gap);
        assert!(prov.validate().is_ok());
    }
}

#[test]
fn descriptors_read_as_source_spellings() {
    assert_eq!(
        class_file::parameter_types("(Ljava/util/List;I[J[[Lcom/a/Outer$Inner;Z)V").unwrap(),
        vec!["List", "int", "long[]", "Outer.Inner[][]", "boolean"]
    );
    assert_eq!(
        class_file::parameter_types("()V").unwrap(),
        Vec::<String>::new()
    );
    assert!(class_file::parameter_types("(Lunterminated").is_none());
}

fn write(root: &std::path::Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn class_files(out: &Extraction) -> Vec<&str> {
    out.nodes
        .iter()
        .filter(|node| node.label == "File")
        .map(|node| node.props["path"].as_str().unwrap())
        .collect()
}

/// AC-1215: a checkout's shipped classes are read; classes in build output
/// directories or with their own Java or Kotlin source are not.
#[test]
fn a_checkout_reads_the_classes_it_ships_without_source() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "lib/com/acme/pay/Ledger.class", LEDGER);
    write(root, "lib/com/acme/pay/Ledger$Store.class", STORE);
    write(root, "build/classes/com/acme/pay/Ledger$Audit.class", AUDIT);
    write(root, "lib/Broken.class", b"not a class");
    let id = id();
    let mut seen = Vec::new();
    let out = extract_dir(root, &id, &mut |path| seen.push(path.to_string())).unwrap();
    assert_eq!(
        class_files(&out),
        vec![
            "lib/com/acme/pay/Ledger$Store.class",
            "lib/com/acme/pay/Ledger.class"
        ]
    );
    assert!(
        out.nodes
            .iter()
            .any(|node| node.label == "Gap" && node.props["path"] == "lib/Broken.class")
    );
    assert_eq!(seen.len(), 3, "{seen:?}");

    // With Ledger's Java source in the checkout, Ledger and its nested
    // classes are left to the Java adapter.
    write(
        root,
        "src/main/java/com/acme/pay/Ledger.java",
        b"package com.acme.pay;",
    );
    let out = extract_dir(root, &id, &mut |_| {}).unwrap();
    assert!(class_files(&out).is_empty(), "{:?}", class_files(&out));

    // A Kotlin file facade (`LedgerKt`) is left to the Kotlin adapter too.
    let kotlin = Sources {
        stems: source_stems(&["app/src/com/acme/Ledger.kt".to_string()]),
        ..Sources::default()
    };
    assert!(has_source(&kotlin, &compiled("com/acme/LedgerKt", None)));
    assert!(has_source(
        &kotlin,
        &compiled("com/acme/Ledger$Inner", None)
    ));
    assert!(!has_source(&kotlin, &compiled("com/other/Ledger", None)));
}

fn compiled(internal: &str, source_file: Option<&str>) -> ClassFile {
    ClassFile {
        access: 0,
        this_class: internal.into(),
        super_class: None,
        interfaces: Vec::new(),
        header_span: (0, 0),
        class_refs: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        nesting: None,
        source_file: source_file.map(str::to_string),
    }
}

/// AC-1215: a class declared in a source file named otherwise (a Kotlin
/// file of several classes, a non-public Java class) is left to its source
/// adapter, found through the file its `SourceFile` attribute names.
#[test]
fn a_class_declared_in_another_named_file_has_source() {
    let paths = [
        "src/main/java/com/acme/pay/Payments.java".to_string(),
        "app/Models.kt".to_string(),
    ];
    let read = |path: &str| match path {
        "app/Models.kt" => Ok(
            b"// models\n@file:JvmName(\"M\")\npackage com.acme.model\n\nclass Ledger\n".to_vec(),
        ),
        _ => Err(std::io::Error::other("not read")),
    };
    let sources = sources(&paths, &read);
    assert!(has_source(
        &sources,
        &compiled("com/acme/pay/Refund", Some("Payments.java"))
    ));
    assert!(!has_source(
        &sources,
        &compiled("com/other/Refund", Some("Payments.java"))
    ));
    assert!(has_source(
        &sources,
        &compiled("com/acme/model/Ledger", Some("Models.kt"))
    ));
    assert!(!has_source(
        &sources,
        &compiled("com/acme/other/Ledger", Some("Models.kt"))
    ));
    assert_eq!(kotlin_package("package a.b.c;\n"), "a/b/c");
    assert_eq!(kotlin_package("class Top\n"), "");
}

/// AC-1215: a binary class shipped at two paths keeps each copy's calls to
/// its own methods, and a call into a class shipped twice names no copy.
#[test]
fn a_class_shipped_twice_keeps_its_own_calls() {
    let files = vec![
        ("a/Ledger.class".to_string(), LEDGER.to_vec()),
        ("b/Ledger.class".to_string(), LEDGER.to_vec()),
        ("a/Ledger$Store.class".to_string(), STORE.to_vec()),
    ];
    let out = extract(&id(), &files);
    for copy in ["a", "b"] {
        let open = format!("sym:{REPO}@{copy}/Ledger.class#Ledger.open()");
        let own = format!("sym:{REPO}@{copy}/Ledger.class#Ledger.Ledger()");
        let dsts: Vec<&str> = out
            .edges
            .iter()
            .filter(|edge| edge.label == "CALLS" && edge.src == open)
            .map(|edge| edge.dst.as_str())
            .collect();
        assert_eq!(dsts, [own.as_str()]);
    }
    // Store is shipped once, so both copies' constructors call it.
    let store = format!("sym:{REPO}@a/Ledger$Store.class#Ledger.Store.Store()");
    assert_eq!(
        out.edges
            .iter()
            .filter(|edge| edge.label == "CALLS" && edge.dst == store)
            .count(),
        2
    );
    // With Store shipped twice, neither copy is named.
    let mut twice = files.clone();
    twice.push(("b/Ledger$Store.class".to_string(), STORE.to_vec()));
    let out = extract(&id(), &twice);
    assert!(
        out.edges
            .iter()
            .filter(|edge| edge.label == "CALLS")
            .all(|edge| !edge.dst.contains("Ledger.Store.Store()")),
    );
}
