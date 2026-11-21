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
    // AC-1108: a template naming a sensitive value is withheld.
    assert_eq!(templated.capture, ExpressionCapture::Redacted);
    assert_eq!(templated.display.as_str(), "TooMany([REDACTED])");
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

// AC-1106: a scope-function name the file rebinds (a declared function or a
// named import) is no proven inline call, so exits in its lambda are not the
// enclosing function's; the standard-library `let` still is.
#[test]
fn rebound_scope_function_names_keep_their_lambdas_apart() {
    let source = r#"package com.acme

import com.acme.jobs.later as also

class Jobs {
    fun <T> T.run(block: () -> Unit) { queue.add(block) }

    fun plan(n: Int): Int {
        run { if (n < 0) throw IllegalStateException("stored, not run") }
        also { if (n > 9) throw IllegalStateException("aliased") }
        n.let { if (it == 1) throw IllegalStateException("inline") }
        return n
    }
}
"#;
    let out = extract(source, "src/main/kotlin/com/acme/Jobs.kt");
    let read: Vec<_> = rules(&out).iter().map(|rule| effect(rule).1).collect();
    assert_eq!(read, ["IllegalStateException(\"inline\")"], "{read:?}");
}

// AC-1108: an elvis right side takes its left side falsy (absent), a
// subject-less `when` entry takes its condition truthy and earlier entries'
// falsy, and `require`/`check`/`requireNotNull`/`checkNotNull` throw with
// their argument falsy, carrying their message literal; an unguarded
// `error(…)`, a `when (subject)` entry and a several-condition entry are no
// rules, and only the `when (subject)` is a control Gap.
#[test]
fn elvis_when_and_precondition_exits_are_guarded_rules() {
    let source = r#"package com.acme

class Orders(private val repo: Repo) {
    fun place(id: String?, qty: Int): Order {
        val key = id ?: throw IllegalArgumentException("id is required")
        val order = repo.find(key) ?: return Order.EMPTY
        when {
            qty < 0 -> throw IllegalArgumentException("negative")
            qty == 0 -> return Order.EMPTY
            else -> Unit
        }
        require(qty < 100) { "too many" }
        check(order.open)
        requireNotNull(order.owner) { "owner is required" }
        if (order.locked) error("locked")
        error("unguarded")
    }

    fun kind(n: Int): String {
        when (n) {
            1 -> return "one"
        }
        when {
            n > 1, n < -1 -> return "many"
        }
        if (n == 0) return "zero"
        return "other"
    }
}
"#;
    let out = extract(source, "src/main/kotlin/com/acme/Orders.kt");
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
    let s = |text: &str| text.to_owned();
    assert_eq!(
        read,
        vec![
            (
                s("Orders.place"),
                vec![(s("id"), FalsyBranch)],
                ("throw", s("IllegalArgumentException(\"id is required\")"))
            ),
            (
                s("Orders.place"),
                vec![(s("repo.find(key)"), FalsyBranch)],
                ("return", s("Order.EMPTY"))
            ),
            (
                s("Orders.place"),
                vec![(s("qty < 0"), TruthyBranch)],
                ("throw", s("IllegalArgumentException(\"negative\")"))
            ),
            (
                s("Orders.place"),
                vec![(s("qty < 0"), FalsyBranch), (s("qty == 0"), TruthyBranch)],
                ("return", s("Order.EMPTY"))
            ),
            (
                s("Orders.place"),
                vec![(s("qty < 100"), FalsyBranch)],
                ("throw", s("\"too many\""))
            ),
            (
                s("Orders.place"),
                vec![(s("order.open"), FalsyBranch)],
                ("throw", s("check(order.open)"))
            ),
            (
                s("Orders.place"),
                vec![(s("order.owner"), FalsyBranch)],
                ("throw", s("\"owner is required\""))
            ),
            (
                s("Orders.place"),
                vec![(s("order.locked"), TruthyBranch)],
                ("throw", s("error(\"locked\")"))
            ),
            (
                s("Orders.kind"),
                vec![(s("n == 0"), TruthyBranch)],
                ("return", s("\"zero\""))
            ),
        ],
        "{read:#?}"
    );
    assert!(
        !gaps(&rules[0]).contains(&s("switch_control")),
        "{:?}",
        gaps(&rules[0])
    );
    assert!(
        gaps(&rules[8]).contains(&s("switch_control")),
        "{:?}",
        gaps(&rules[8])
    );
}

// AC-1108: across files, a same-package top-level `require` and a `run`
// member of a repository supertype rebind those names, so the rules that
// relied on them are dropped with their Gaps; `?.let` and `check` stay.
#[test]
fn names_rebound_in_other_files_drop_the_rules_relying_on_them() {
    let dir = tempfile::tempdir().unwrap();
    let write = |path: &str, text: &str| {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    };
    write(
        "src/main/kotlin/com/acme/Checks.kt",
        "package com.acme\n\nfun require(ok: Boolean, message: () -> String) {}\n",
    );
    write(
        "src/main/kotlin/com/acme/Base.kt",
        "package com.acme\n\nopen class Base {\n    fun run(block: () -> Unit) {}\n}\n",
    );
    let svc = "package com.acme\n\nclass Svc : Base() {\n    fun a(n: Int) {\n        require(n > 0) { \"positive\" }\n    }\n\n    fun b(n: Int?) {\n        n?.let { if (it < 0) throw IllegalStateException(\"neg\") }\n    }\n\n    fun c(n: Int) {\n        run { if (n < 0) throw IllegalStateException(\"run\") }\n    }\n\n    fun d(n: Int) {\n        check(n > 0)\n    }\n}\n";
    write("src/main/kotlin/com/acme/Svc.kt", svc);
    let id = SourceId {
        repo: "acme/shop",
        commit: "c0ffee",
    };
    let out = crate::extract_dir(dir.path(), &id).expect("extracts");
    let owners: Vec<String> = rules(&out)
        .iter()
        .map(|rule| rule.owner_id.rsplit('#').next().unwrap().to_owned())
        .collect();
    assert_eq!(owners, ["Svc.b", "Svc.d"], "{owners:?}");
    let dangling = out.nodes.iter().any(|n| {
        n.label == "Gap"
            && n.id.starts_with("gap:rule:")
            && !out.nodes.iter().any(|rule| {
                rule.label == "BusinessRule" && n.id.starts_with(&format!("gap:{}:", rule.id))
            })
    });
    assert!(!dangling, "a dropped rule's Gaps are dropped with it");
    // Read alone, the file has no rebinding, so all four are rules.
    let alone = extract(svc, "src/main/kotlin/com/acme/Svc.kt");
    assert_eq!(rules(&alone).len(), 4);
}

// AC-1108: a template of plain names keeps its text as written, with no
// literal value; a secret in a template withholds it.
#[test]
fn plain_name_templates_stay_readable_in_rule_values() {
    let source = r#"package com.acme

class Owners(private val owners: Repo) {
    fun show(ownerId: Int): Owner {
        return owners.find(ownerId) ?: throw NotFound("Owner $ownerId of ${owners.name} not found")
    }

    fun token(id: Int): String {
        if (id < 0) throw Bad("key ghp_abcdefghijklmnopqrstuvwxyz0123456789 for $id")
        return ""
    }
}
"#;
    let out = extract(source, "src/main/kotlin/com/acme/Owners.kt");
    let rules = rules(&out);
    let values: Vec<_> = rules
        .iter()
        .map(|rule| match &rule.effect {
            LocalExit::Throw { value } => (value.display.as_str().to_owned(), value.capture),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        values,
        [
            (
                "NotFound(\"Owner $ownerId of ${owners.name} not found\")".to_owned(),
                ExpressionCapture::CompleteSyntax
            ),
            ("Bad([REDACTED])".to_owned(), ExpressionCapture::Redacted),
        ],
        "{values:?}"
    );
}
