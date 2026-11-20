use crate::{SourceId, extract_source};
use core_graph::rules::*;

fn rules(source: &str, path: &str) -> Vec<(String, GuardedExitEvidence)> {
    let id = SourceId {
        repo: "example/bot",
        commit: "c0ffee",
    };
    let out = extract_source(source.as_bytes(), path, &id).expect("extracts");
    let mut rules: Vec<_> = out
        .nodes
        .iter()
        .filter(|node| node.label == "BusinessRule")
        .map(|node| {
            (
                node.props["observation"].as_str().unwrap().to_owned(),
                GuardedExitEvidence::from_value(node.props["rule"].clone()).expect("valid rule"),
            )
        })
        .collect();
    rules.sort_by_key(|(_, rule)| rule.exit_source.byte_start);
    for (_, rule) in &rules {
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

fn shown(expression: &SourceExpression) -> &str {
    expression.display.as_str()
}

const HANDLERS: &str = r#"package device

import (
	"errors"
	"net/http"
)

var ErrNotFound = errors.New("not found")

type Server struct{ allow map[string]bool }

func (s *Server) signIn(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	email := r.FormValue("email")
	if !s.allow[email] {
		// Only allowlisted accounts sign in.
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	} else if email == "" {
		w.WriteHeader(400)
		return
	}
	w.WriteHeader(http.StatusOK)
	go func() {
		if email == "x" {
			return
		}
	}()
}

func lookup(id string) (string, error) {
	if id == "" {
		return "", ErrNotFound
	}
	if id == "admin" {
		return "", errors.New("reserved")
	}
	v, err := load(id)
	if err != nil {
		return "", err
	}
	if err := check(v); err != nil {
		return "", fmt.Errorf("check %s: %w", id, err)
	}
	return id, nil
}
"#;

#[test]
fn go_refusals_and_guarded_returns_are_rules() {
    let rules = rules(HANDLERS, "pkg/device/web.go");
    let summary: Vec<_> = rules
        .iter()
        .map(|(observation, rule)| {
            let conditions: Vec<_> = rule
                .conditions
                .iter()
                .map(|c| format!("{:?} {}", c.polarity, shown(&c.expression)))
                .collect();
            let effect = match &rule.effect {
                LocalExit::Call { value } => format!("call {}", shown(value)),
                LocalExit::Return { value } => {
                    format!("return {}", value.as_ref().map_or("", shown))
                }
                other => format!("{other:?}"),
            };
            format!(
                "{observation} {} [{}] {effect}",
                rule.owner_id.rsplit('#').next().unwrap(),
                conditions.join("; ")
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            "guarded_effect Server.signIn [TruthyBranch r.Method != http.MethodPost] call http.Error(w, \"method not allowed\", http.StatusMethodNotAllowed)",
            "guarded_effect Server.signIn [TruthyBranch !s.allow[email]] call http.Error(w, \"forbidden\", http.StatusForbidden)",
            "guarded_effect Server.signIn [FalsyBranch !s.allow[email]; TruthyBranch email == \"\"] call w.WriteHeader(400)",
            "guarded_exit lookup [TruthyBranch id == \"\"] return \"\", ErrNotFound",
            "guarded_exit lookup [TruthyBranch id == \"admin\"] return \"\", errors.New(\"reserved\")",
        ],
        "a refusal is the call before a bare return; the OK write, the unconditional return, the goroutine's return and passing an error on are not rules"
    );
    let (_, refusal) = &rules[1];
    assert_eq!(refusal.kind, RuleKind::GuardedEffect);
    assert!(
        refusal
            .interpretation
            .gap_ids
            .iter()
            .any(|gap| gap.ends_with(":preceding_exit")),
        "the earlier refusal may leave the handler first"
    );
    assert!(
        refusal
            .interpretation
            .gap_ids
            .iter()
            .any(|gap| gap.ends_with(":exception_control")),
        "the goroutine is not interpreted"
    );
}

#[test]
fn go_test_files_and_secrets_state_no_rules() {
    assert!(rules(HANDLERS, "pkg/device/web_test.go").is_empty());
    let secret = r#"package auth

import "net/http"

func check(w http.ResponseWriter, token string) {
	if token != "ghp_abcdefghijklmnopqrstuvwxyz0123456789" {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
}
"#;
    let rules = rules(secret, "auth/check.go");
    let [(_, rule)] = &rules[..] else {
        panic!("one refusal: {rules:?}");
    };
    let condition = &rule.conditions[0].expression;
    assert_eq!(shown(condition), "token != [REDACTED]");
    assert_eq!(condition.capture, ExpressionCapture::Redacted);
}

#[test]
fn an_inline_route_handlers_refusals_are_its_rules() {
    // AC-0923 (#2009): a func literal registered as a route handler is its
    // own Symbol, and its guarded refusals are governed by it.
    let source = r#"package device

import "net/http"

func routes(mux *http.ServeMux) {
	mux.HandleFunc("POST /sign-in", func(w http.ResponseWriter, r *http.Request) {
		if r.FormValue("email") == "" {
			http.Error(w, "email required", http.StatusBadRequest)
			return
		}
	})
}
"#;
    let found = rules(source, "pkg/device/routes.go");
    assert_eq!(found.len(), 1, "{found:?}");
    let (_, rule) = &found[0];
    assert!(rule.owner_id.contains("#routes/func@"), "{}", rule.owner_id);
    assert!(shown(&rule.conditions[0].expression).contains("FormValue"));
}

#[test]
fn a_tested_error_names_the_call_that_failed() {
    // AC-0923 (#2009): `err != nil` names the call whose error it tests,
    // bound in the `if` initializer or the statement just before it.
    let source = r#"package device

import (
	"encoding/json"
	"net/http"
)

func (s *Service) save(w http.ResponseWriter, r *http.Request) {
	var d Device
	if err := json.NewDecoder(r.Body).Decode(&d); err != nil {
		http.Error(w, "bad body", http.StatusBadRequest)
		return
	}
	err := s.Apps.Store.Save(d)
	// stored
	if err != nil {
		http.Error(w, "save failed", http.StatusInternalServerError)
		return
	}
	if d.Name == "" {
		http.Error(w, "name required", http.StatusBadRequest)
		return
	}
}
"#;
    let id = SourceId {
        repo: "example/bot",
        commit: "c0ffee",
    };
    let out = extract_source(source.as_bytes(), "pkg/device/save.go", &id).expect("extracts");
    let mut failed: Vec<(u64, serde_json::Value)> = out
        .nodes
        .iter()
        .filter(|node| node.label == "BusinessRule")
        .map(|node| {
            let rule = GuardedExitEvidence::from_value(node.props["rule"].clone()).unwrap();
            (
                rule.exit_source.byte_start,
                node.props["failed_calls"].clone(),
            )
        })
        .collect();
    failed.sort_by_key(|(at, _)| *at);
    let failed: Vec<_> = failed.into_iter().map(|(_, f)| f).collect();
    assert_eq!(
        failed,
        vec![
            serde_json::json!(["Decode"]),
            serde_json::json!(["Store.Save"]),
            serde_json::json!([null]),
        ]
    );
}
