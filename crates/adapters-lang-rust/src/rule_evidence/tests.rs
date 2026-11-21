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

fn returned(rule: &GuardedExitEvidence) -> String {
    match &rule.effect {
        LocalExit::Return { value: Some(value) } => value.display.as_str().to_owned(),
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

const SERVICE: &str = r#"use axum::http::StatusCode;

pub struct Accounts {
    store: Store,
}

impl Accounts {
    pub fn register(&self, user: &User, password: &str) -> Result<Id, ApiError> {
        if user.id.is_some() {
            return Err(ApiError::Conflict);
        }
        if self.store.exists(&user.login) {
            return Err(ApiError::LoginTaken);
        } else if password.len() < 4 {
            // Short passwords are refused.
            return Err(anyhow!("password too short"));
        }
        let later = |n: u32| {
            if n > 9 {
                return 1;
            }
            0
        };
        Ok(self.store.save(user))
    }
}

pub async fn delete(id: u64) -> Result<(), StatusCode> {
    if id == 0 {
        return Err(StatusCode::NOT_FOUND);
    }
    if let Some(lock) = locks().get(&id) {
        return Err(StatusCode::CONFLICT);
    }
    let task = async {
        if id > 9 {
            return 2;
        }
        1
    };
    return Ok(());
}

#[cfg(test)]
mod tests {
    fn helper(n: u32) -> u32 {
        if n == 0 {
            return 1;
        }
        n
    }
}
"#;

// AC-1017: a valued `return` under same-function `if`/`else` branches is a
// guarded exit owned by its `fn`; an unguarded return, returns inside a
// closure or `async` block, and test code are not rules.
#[test]
fn rust_guarded_returns_are_rules_owned_by_their_functions() {
    let out = extract(SERVICE, "src/accounts.rs");
    let rules = rules(&out);
    let read: Vec<_> = rules
        .iter()
        .map(|rule| {
            (
                rule.owner_id.rsplit('#').next().unwrap().to_owned(),
                conditions(rule),
                returned(rule),
            )
        })
        .collect();
    use BranchPolarity::*;
    assert_eq!(
        read,
        vec![
            (
                "Accounts.register".into(),
                vec![("user.id.is_some()".into(), TruthyBranch)],
                "Err(ApiError::Conflict)".into()
            ),
            (
                "Accounts.register".into(),
                vec![("self.store.exists(&user.login)".into(), TruthyBranch)],
                "Err(ApiError::LoginTaken)".into()
            ),
            (
                "Accounts.register".into(),
                vec![
                    ("self.store.exists(&user.login)".into(), FalsyBranch),
                    ("password.len() < 4".into(), TruthyBranch)
                ],
                "Err(anyhow!(\"password too short\"))".into()
            ),
            (
                "delete".into(),
                vec![("id == 0".into(), TruthyBranch)],
                "Err(StatusCode::NOT_FOUND)".into()
            ),
            (
                "delete".into(),
                vec![("let Some(lock) = locks().get(&id)".into(), TruthyBranch)],
                "Err(StatusCode::CONFLICT)".into()
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
    assert!(
        out.nodes
            .iter()
            .any(|n| n.label == "Gap" && n.props["rule_evidence_gap"] == true)
    );
}

// AC-1017: a secret compared in a condition is withheld from its display;
// a loop, a `match`, `?` and a field write are control Gaps; a file under
// `tests/` yields no rules.
#[test]
fn rust_rule_displays_withhold_secrets_and_control_flow_is_a_gap() {
    let source = r#"pub fn open(&mut self, password: &str, keys: &[String]) -> Result<bool, Error> {
    for key in keys {
        if key.is_empty() {
            return Ok(false);
        }
    }
    let mode = match self.mode { Mode::A => 1, _ => 2 };
    let user = load()?;
    if password == "hunter2-very-secret" {
        return Ok(true);
    }
    self.count += 1;
    self.last = mode;
    Ok(false)
}
"#;
    let out = extract(source, "src/gate.rs");
    let rules = rules(&out);
    assert_eq!(rules.len(), 2, "{rules:#?}");
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
    let reasons = gaps(secret);
    for reason in [
        "loop_dependency",
        "switch_control",
        "exception_control",
        "mutation",
        "redacted_expression",
    ] {
        assert!(reasons.contains(&reason.to_owned()), "{reasons:?}");
    }
    let test = extract(source, "tests/gate.rs");
    assert!(!test.nodes.iter().any(|n| n.label == "BusinessRule"));
}

// AC-1017: rebinding a local is no mutation; a reused file's rules carry the
// new commit.
#[test]
fn rust_local_rebinds_are_not_mutations_and_reused_rules_carry_the_new_commit() {
    let source = r#"pub fn clamp(n: i64) -> i64 {
    let mut m = n;
    if n < 0 {
        return 0;
    }
    m = m * 2;
    m
}

pub fn named(name: &[u8], parts: &[&str]) -> bool {
    if name != b"template" && parts.iter().any(|p: &&str| p.is_empty()) {
        return false;
    }
    true
}
"#;
    let mut out = extract(source, "src/clamp.rs");
    let found = rules(&out);
    assert_eq!(found.len(), 2);
    // Byte strings and closures are captured as written.
    assert_eq!(
        conditions(&found[1]),
        vec![(
            "name != b\"template\" && parts.iter().any(|p: &&str| p.is_empty())".to_owned(),
            BranchPolarity::TruthyBranch
        )]
    );
    assert_eq!(
        found[1].conditions[0].expression.capture,
        ExpressionCapture::CompleteSyntax
    );
    assert!(!gaps(&found[0]).contains(&"mutation".to_owned()));
    let literal = match &found[0].effect {
        LocalExit::Return { value: Some(value) } => value.literal.clone(),
        _ => None,
    };
    assert_eq!(
        literal,
        Some(LiteralEvidence::Known {
            value: KnownLiteral::Number(0.into())
        })
    );
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
