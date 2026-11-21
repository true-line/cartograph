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

const SERVICE: &str = r#"package com.acme.service;

public class UserService {
    private final UserRepository repo;

    public User register(User user, String password) {
        if (user.getId() != null) {
            throw new BadRequestException("A new user cannot already have an ID");
        }
        if (repo.findOneByLogin(user.getLogin()).isPresent()) {
            throw new LoginAlreadyUsedException();
        } else if (password.length() < 4) {
            // Short passwords are refused.
            throw new InvalidPasswordException();
        }
        return repo.save(user);
    }

    public String label(int count) {
        if (count == 0) {
            return "none";
        }
        Runnable later = () -> {
            if (count > 9) {
                throw new IllegalStateException("inside a lambda");
            }
        };
        return "some";
    }

    public UserService(UserRepository repo) {
        if (repo == null) throw new IllegalArgumentException("repo is required");
        this.repo = repo;
    }
}
"#;

// AC-0967: a valued return or a throw under same-method `if`/`else` branches
// is a guarded exit owned by its method or constructor; an unguarded exit
// and exits inside a lambda are not rules.
#[test]
fn java_guarded_returns_and_throws_are_rules_owned_by_their_methods() {
    let out = extract(SERVICE, "src/main/java/com/acme/service/UserService.java");
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
                "UserService.register(User,String)".into(),
                vec![("user.getId() != null".into(), TruthyBranch)],
                (
                    "throw",
                    "new BadRequestException(\"A new user cannot already have an ID\")".into()
                )
            ),
            (
                "UserService.register(User,String)".into(),
                vec![(
                    "repo.findOneByLogin(user.getLogin()).isPresent()".into(),
                    TruthyBranch
                )],
                ("throw", "new LoginAlreadyUsedException()".into())
            ),
            (
                "UserService.register(User,String)".into(),
                vec![
                    (
                        "repo.findOneByLogin(user.getLogin()).isPresent()".into(),
                        FalsyBranch
                    ),
                    ("password.length() < 4".into(), TruthyBranch)
                ],
                ("throw", "new InvalidPasswordException()".into())
            ),
            (
                "UserService.label(int)".into(),
                vec![("count == 0".into(), TruthyBranch)],
                ("return", "\"none\"".into())
            ),
            (
                "UserService.UserService(UserRepository)".into(),
                vec![("repo == null".into(), TruthyBranch)],
                (
                    "throw",
                    "new IllegalArgumentException(\"repo is required\")".into()
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
    let literal = match &first.effect {
        LocalExit::Throw { value } => value.literal.clone(),
        _ => None,
    };
    assert!(literal.is_none(), "a constructor call is no literal");
    // A later exit depends on the earlier one; the constructor's field write
    // is a mutation.
    let gaps = |rule: &GuardedExitEvidence| -> Vec<String> {
        rule.interpretation
            .gap_ids
            .iter()
            .map(|id| id.rsplit(':').next().unwrap().to_owned())
            .collect()
    };
    assert!(!gaps(&rules[0]).contains(&"preceding_exit".to_owned()));
    assert!(gaps(&rules[1]).contains(&"preceding_exit".to_owned()));
    assert!(
        gaps(&rules[4]).contains(&"mutation".to_owned()),
        "{:?}",
        gaps(&rules[4])
    );
    let gap_nodes = out
        .nodes
        .iter()
        .filter(|n| n.label == "Gap" && n.props["rule_evidence_gap"] == true)
        .count();
    assert!(gap_nodes > 0);
}

// AC-0967: a secret compared in a condition is withheld from its display,
// a loop or try in the method is a control Gap, and test sources yield no
// rules.
#[test]
fn java_rule_displays_withhold_secrets_and_test_code_is_no_rule_source() {
    let source = r#"package com.acme;
public class Gate {
    public boolean open(String password, java.util.List<String> keys) {
        for (String key : keys) {
            if (key.isEmpty()) return false;
        }
        try {
            if (password.equals("hunter2-very-secret")) {
                return true;
            }
        } finally {
            keys.clear();
        }
        return false;
    }
}
"#;
    let out = extract(source, "src/main/java/com/acme/Gate.java");
    let rules = rules(&out);
    assert_eq!(rules.len(), 2, "{rules:#?}");
    let secret = &rules[1];
    assert_eq!(
        secret.conditions[0].expression.display.as_str(),
        "password.equals([REDACTED])"
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
    let reasons: Vec<String> = secret
        .interpretation
        .gap_ids
        .iter()
        .map(|id| id.rsplit(':').next().unwrap().to_owned())
        .collect();
    for reason in [
        "loop_dependency",
        "exception_control",
        "redacted_expression",
    ] {
        assert!(reasons.contains(&reason.to_owned()), "{reasons:?}");
    }
    let test = extract(source, "src/test/java/com/acme/GateTest.java");
    assert!(!test.nodes.iter().any(|n| n.label == "BusinessRule"));
}

// AC-0967: a reused file's rules carry the new commit.
#[test]
fn a_reused_java_files_rules_carry_the_new_commit() {
    let mut out = extract(SERVICE, "src/main/java/com/acme/service/UserService.java");
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
