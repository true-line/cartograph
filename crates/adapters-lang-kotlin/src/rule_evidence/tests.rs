use crate::{SourceId, extract_source};
use core_graph::rules::*;

fn extract(source: &str, path: &str) -> crate::Extraction {
    let id = SourceId {
        repo: "acme/shop",
        commit: "c0ffee",
    };
    extract_source(source.as_bytes(), path, &id).expect("extracts")
}

fn rules(out: &crate::Extraction) -> Vec<GuardedExitEvidence> {
    let mut rules: Vec<_> = out
        .nodes
        .iter()
        .filter(|node| node.label == "BusinessRule")
        .map(|node| {
            assert_eq!(node.props["observation"], "guarded_exit");
            GuardedExitEvidence::from_value(node.props["rule"].clone()).expect("valid rule")
        })
        .collect();
    rules.sort_by_key(|rule| rule.exit_source.byte_start);
    for rule in &rules {
        assert!(
            out.edges.iter().any(|edge| edge.label == "GOVERNS"
                && edge.dst == rule.owner_id
                && out
                    .nodes
                    .iter()
                    .any(|n| n.id == edge.dst && n.label == "Symbol")),
            "every rule is governed by a Symbol"
        );
    }
    rules
}

fn conditions(rule: &GuardedExitEvidence) -> Vec<(String, BranchPolarity)> {
    rule.conditions
        .iter()
        .map(|c| (c.expression.display.as_str().to_owned(), c.polarity))
        .collect()
}

fn effect(rule: &GuardedExitEvidence) -> (&'static str, String) {
    match &rule.effect {
        LocalExit::Throw { value } => ("throw", value.display.as_str().to_owned()),
        LocalExit::Return { value } => (
            "return",
            value
                .as_ref()
                .map(|v| v.display.as_str().to_owned())
                .unwrap_or_default(),
        ),
        other => panic!("unexpected effect {other:?}"),
    }
}

fn gaps(rule: &GuardedExitEvidence) -> Vec<String> {
    rule.interpretation
        .gap_ids
        .iter()
        .map(|id| id.rsplit(':').next().unwrap().to_owned())
        .collect()
}

const SERVICE: &str = r#"package com.acme.service

class UserService(private val repo: UserRepository) {
    fun register(user: User, password: String): User {
        if (user.id != null) {
            throw BadRequestException("A new user cannot already have an ID")
        }
        if (repo.findOneByLogin(user.login) != null) {
            throw LoginAlreadyUsedException()
        } else if (password.length < 4) {
            // Short passwords are refused.
            throw InvalidPasswordException()
        }
        return repo.save(user)
    }

    fun label(count: Int): String {
        if (count == 0) return "none"
        val later = {
            if (count > 9) throw IllegalStateException("inside a lambda")
        }
        val task = object : Runnable {
            override fun run() {
                if (count < 0) throw IllegalStateException("inside an object")
            }
        }
        fun local(): Int {
            if (count > 1) return 2
            return 0
        }
        list.forEach { if (it) return@forEach }
        return "some"
    }

    fun update(slug: String): Any {
        repo.find(slug)?.let {
            if (it.owner != current) throw ForbiddenRequestException()
            return it
        }
        throw NotFoundException()
    }

    val size: Int
        get() {
            if (repo.empty) return 0
            return 1
        }

    init {
        if (repo.closed) throw IllegalArgumentException("repo is closed")
    }

    companion object {
        fun of(name: String?): UserService {
            if (name == null) throw IllegalArgumentException("name is required")
            return UserService(Repo(name))
        }
    }
}
"#;

// AC-1106: a valued return or a throw under same-function `if`/`else`
// branches is a guarded exit owned by its function (companion members
// included), a scope function's lambda (`?.let { … }`) belonging to the
// function; an unguarded exit, a labelled return and exits inside another
// lambda, an object expression, a local function, an accessor or an `init`
// block are not rules.
#[test]
fn kotlin_guarded_returns_and_throws_are_rules_owned_by_their_functions() {
    let out = extract(SERVICE, "src/main/kotlin/com/acme/service/UserService.kt");
    let rules = rules(&out);
    let read: Vec<_> = rules
        .iter()
        .map(|rule| {
            (
                rule.owner_id.rsplit('#').next().unwrap().to_owned(),
                conditions(rule),
                effect(rule),
            )
        })
        .collect();
    use BranchPolarity::*;
    assert_eq!(
        read,
        vec![
            (
                "UserService.register".into(),
                vec![("user.id != null".into(), TruthyBranch)],
                (
                    "throw",
                    "BadRequestException(\"A new user cannot already have an ID\")".into()
                )
            ),
            (
                "UserService.register".into(),
                vec![(
                    "repo.findOneByLogin(user.login) != null".into(),
                    TruthyBranch
                )],
                ("throw", "LoginAlreadyUsedException()".into())
            ),
            (
                "UserService.register".into(),
                vec![
                    (
                        "repo.findOneByLogin(user.login) != null".into(),
                        FalsyBranch
                    ),
                    ("password.length < 4".into(), TruthyBranch)
                ],
                ("throw", "InvalidPasswordException()".into())
            ),
            (
                "UserService.label".into(),
                vec![("count == 0".into(), TruthyBranch)],
                ("return", "\"none\"".into())
            ),
            (
                "UserService.update".into(),
                vec![("it.owner != current".into(), TruthyBranch)],
                ("throw", "ForbiddenRequestException()".into())
            ),
            (
                "UserService.of".into(),
                vec![("name == null".into(), TruthyBranch)],
                (
                    "throw",
                    "IllegalArgumentException(\"name is required\")".into()
                )
            ),
        ],
        "{read:#?}"
    );
    let first = &rules[0];
    assert_eq!(first.kind, RuleKind::GuardedExit);
    assert_eq!(
        first.interpretation.execution_predicate,
        InterpretationStatus::NotEstablished
    );
    assert!(!gaps(&rules[0]).contains(&"preceding_exit".to_owned()));
    assert!(gaps(&rules[1]).contains(&"preceding_exit".to_owned()));
    let none = match &rules[3].effect {
        LocalExit::Return { value: Some(value) } => value.literal.clone(),
        _ => None,
    };
    assert_eq!(
        none,
        Some(LiteralEvidence::Known {
            value: KnownLiteral::String("none".into())
        })
    );
    assert!(
        out.nodes
            .iter()
            .any(|n| n.label == "Gap" && n.props["rule_evidence_gap"] == true)
    );
}

// AC-1106: a secret compared in a condition is withheld from its display; a
// string template is withheld whole; a loop, `when`, `try` and a property
// write in the function are control Gaps; rebinding a local is no
// mutation; test sources yield no rules.
#[test]
fn kotlin_rule_displays_withhold_secrets_and_control_flow_is_a_gap() {
    let source = r#"package com.acme

class Gate {
    var opened = 0

    fun open(password: String, keys: List<String>): Boolean {
        for (key in keys) {
            if (key.isEmpty()) return false
        }
        try {
            if (password == "hunter2-very-secret") {
                return true
            }
        } finally {
            keys.size
        }
        when (keys.size) { 0 -> opened++ }
        if (keys.size > 9) throw TooMany("over $password")
        return false
    }

    fun rebind(n: Int): Int {
        var count = 0
        if (n < 0) return -1
        count = n
        return count
    }

    fun write(n: Int): Int {
        if (n < 0) return -1
        opened = n
        return 1
    }
}
"#;
    let out = extract(source, "src/main/kotlin/com/acme/Gate.kt");
    let rules = rules(&out);
    let owners: Vec<&str> = rules
        .iter()
        .map(|rule| rule.owner_id.rsplit('#').next().unwrap())
        .collect();
    assert_eq!(
        owners,
        [
            "Gate.open",
            "Gate.open",
            "Gate.open",
            "Gate.rebind",
            "Gate.write"
        ],
        "{rules:#?}"
    );
    let secret = &rules[1];
    assert_eq!(
        secret.conditions[0].expression.display.as_str(),
        "password == [REDACTED]"
    );
    assert_eq!(
        secret.conditions[0].expression.capture,
        ExpressionCapture::Redacted
    );
    assert!(
        secret
            .redactions
            .iter()
            .any(|r| r.reason == RedactionReason::CredentialValue)
    );
    for reason in [
        "loop_dependency",
        "exception_control",
        "switch_control",
        "mutation",
        "redacted_expression",
    ] {
        assert!(
            gaps(secret).contains(&reason.to_owned()),
            "{:?}",
            gaps(secret)
        );
    }
    let templated = match &rules[2].effect {
        LocalExit::Throw { value } => value.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(templated.capture, ExpressionCapture::Unsupported);
    assert!(!gaps(&rules[3]).contains(&"mutation".to_owned()));
    assert!(gaps(&rules[4]).contains(&"mutation".to_owned()));
    let test = extract(source, "src/test/kotlin/com/acme/GateTest.kt");
    assert!(!test.nodes.iter().any(|n| n.label == "BusinessRule"));
}

// AC-1106: a reused file's rules carry the new commit.
#[test]
fn a_reused_kotlin_files_rules_carry_the_new_commit() {
    let mut out = extract(SERVICE, "src/main/kotlin/com/acme/service/UserService.kt");
    crate::retarget_commit(&mut out, "def456");
    let rule = out
        .nodes
        .iter()
        .find(|n| n.label == "BusinessRule")
        .unwrap();
    let text = rule.props["rule"].to_string();
    assert!(
        text.contains("def456") && !text.contains("c0ffee"),
        "{text}"
    );
}
