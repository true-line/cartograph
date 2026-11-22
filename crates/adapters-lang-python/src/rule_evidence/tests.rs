use super::*;
use crate::{IncrementalCache, SourceId, extract_dir_incremental, extract_source};

fn recover(source: &str) -> Extraction {
    extract_source(
        source.as_bytes(),
        "app/views.py",
        &SourceId {
            repo: "fixture",
            commit: "revision-a",
        },
    )
    .unwrap()
}

fn rules(out: &Extraction) -> Vec<(&Node, GuardedExitEvidence)> {
    out.nodes
        .iter()
        .filter(|node| node.label == "BusinessRule")
        .map(|node| {
            (
                node,
                GuardedExitEvidence::from_value(node.props["rule"].clone()).unwrap(),
            )
        })
        .collect()
}

fn shown(rule: &GuardedExitEvidence) -> Vec<(String, BranchPolarity)> {
    rule.conditions
        .iter()
        .map(|c| (c.expression.display.as_str().to_owned(), c.polarity))
        .collect()
}

fn value(rule: &GuardedExitEvidence) -> (&'static str, String) {
    match &rule.effect {
        LocalExit::Throw { value } => ("raise", value.display.as_str().to_owned()),
        LocalExit::Return { value } => (
            "return",
            value
                .as_ref()
                .map(|v| v.display.as_str().to_owned())
                .unwrap_or_default(),
        ),
        LocalExit::Assign { target, value } => (
            "assign",
            format!("{} = {}", target.display.as_str(), value.display.as_str()),
        ),
        LocalExit::Call { value } => ("call", value.display.as_str().to_owned()),
    }
}

#[test]
fn state_changes_and_calls_are_guarded_effects() {
    // AC-0910 (T-0910): an attribute assignment and a call statement to a
    // function, a `self` method or an imported lowercase module's function
    // are guarded effects with the branches around them (possibly none);
    // logging (also a verb imported directly), builtins, a constructor call,
    // an upper-case receiver, a local's method, a module name a parameter
    // shadows, a plain name and a constructor body are not; a module
    // imported inside the function counts.
    let code = r#"
from flask import g, current_user
from app import alias_utils
from app.log import LOG
from app.db import Session
from logging import info
import service

def authorize(api_key):
    if not api_key:
        if current_user.is_authenticated:
            g.user = current_user
    else:
        api_key.times += 1
        g.user = api_key.user
    if g.user.disabled:
        return "disabled", 403

def delete_alias(alias, user):
    if alias.delete_on is not None:
        perform_alias_deletion(alias, user)
    else:
        move_alias_to_trash(alias, user)
    LOG.i("deleted")
    print("x")
    Session.commit()
    alias.save()
    alias_utils.audit(alias)
    total = 1

def more(alias, service):
    hasattr(alias, "x")
    max([1])
    User()
    info("x")
    service.save()
    import mailer
    mailer.send(alias)

def move_alias_to_trash(alias, user):
    alias.enabled = False

def perform_alias_deletion(alias, user):
    pass

class Box:
    def __init__(self):
        self.size = 1

    def reset(self):
        self.clear()
"#;
    let out = recover(code);
    type Found<'a> = (String, (&'a str, String), Vec<(String, BranchPolarity)>);
    let found: Vec<Found<'_>> = rules(&out)
        .into_iter()
        .filter(|(_, r)| r.kind == RuleKind::GuardedEffect)
        .map(|(node, r)| {
            assert_eq!(node.props["observation"], "guarded_effect");
            assert!(node.id.contains("#effect@"), "{}", node.id);
            (
                r.owner_id.rsplit('#').next().unwrap().to_owned(),
                value(&r),
                shown(&r),
            )
        })
        .collect();
    let t = BranchPolarity::TruthyBranch;
    let f = BranchPolarity::FalsyBranch;
    let names: Vec<(&str, &str, &str)> = found
        .iter()
        .map(|(owner, (kind, shown), _)| (owner.as_str(), *kind, shown.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("Box.reset", "call", "self.clear()"),
            ("authorize", "assign", "g.user = current_user"),
            ("authorize", "assign", "g.user = api_key.user"),
            (
                "delete_alias",
                "call",
                "perform_alias_deletion(alias, user)"
            ),
            ("delete_alias", "call", "move_alias_to_trash(alias, user)"),
            ("delete_alias", "call", "alias_utils.audit(alias)"),
            ("more", "call", "mailer.send(alias)"),
            ("move_alias_to_trash", "assign", "alias.enabled = False"),
        ],
        "{found:?}"
    );
    let conditions = |i: usize| found[i].2.clone();
    assert_eq!(
        conditions(1),
        [
            ("not api_key".to_owned(), t),
            ("current_user.is_authenticated".to_owned(), t)
        ]
    );
    assert_eq!(conditions(2), [("not api_key".to_owned(), f)]);
    assert_eq!(
        conditions(4),
        [("alias.delete_on is not None".to_owned(), f)]
    );
    assert!(conditions(5).is_empty());
    // The exit after the effects is still a guarded exit.
    assert!(
        rules(&out)
            .iter()
            .any(|(_, r)| r.kind == RuleKind::GuardedExit && r.owner_id.ends_with("#authorize"))
    );
}

#[test]
fn record_creates_and_deletes_on_imported_classes_are_guarded_effects() {
    // AC-0910 (T-0910, #1998): `create`/`delete` on an imported upper-case
    // class is an effect with its branches; another verb, the SQLAlchemy
    // `Session`, a class that is not imported, or a name rebound in the
    // function is not.
    let code = r#"
from app.models import AliasUsedOn, DeletedAlias, Alias
from app.db import Session

def new_random_alias(alias, hostname):
    if hostname and not AliasUsedOn.get_by(alias_id=alias.id, hostname=hostname):
        AliasUsedOn.create(alias_id=alias.id, hostname=hostname, commit=True)
    DeletedAlias.delete(alias.id)
    Alias.filter_by(id=1)
    Session.delete(alias)
    Local.create(x=1)

def rebound(Alias):
    Alias.create(x=1)

def local_import(src: Alias, n: int = Alias):
    from app.models import Contact
    Contact.create(x=1)
    Alias.delete(src.id)
"#;
    let out = recover(code);
    type Found = (String, String, Vec<(String, BranchPolarity)>);
    let found: Vec<Found> = rules(&out)
        .into_iter()
        .filter(|(_, r)| r.kind == RuleKind::GuardedEffect)
        .map(|(_, r)| {
            (
                r.owner_id.rsplit('#').next().unwrap().to_owned(),
                value(&r).1,
                shown(&r),
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            (
                "local_import".to_owned(),
                "Contact.create(x=1)".to_owned(),
                vec![],
            ),
            (
                "local_import".to_owned(),
                "Alias.delete(src.id)".to_owned(),
                vec![],
            ),
            (
                "new_random_alias".to_owned(),
                "AliasUsedOn.create(alias_id=alias.id, hostname=hostname, commit=True)".to_owned(),
                vec![(
                    "hostname and not AliasUsedOn.get_by(alias_id=alias.id, hostname=hostname)"
                        .to_owned(),
                    BranchPolarity::TruthyBranch
                )],
            ),
            (
                "new_random_alias".to_owned(),
                "DeletedAlias.delete(alias.id)".to_owned(),
                vec![],
            ),
        ],
        "{found:?}"
    );
}

#[test]
fn records_added_to_the_session_and_query_deletes_are_guarded_effects() {
    // AC-0910 (T-0910, #1998): `Session.add` of an imported class's
    // constructor, directly or through a local assigned before it, is an
    // effect whose value is the constructor; `Model.filter(…).delete()` is
    // one too, as is `x = Model.create(…)`. A local not built from an
    // imported class, reassigned, or assigned in another block is not.
    let code = r#"
from app.models import DeletedAlias, DomainDeletedAlias, Alias
from app.db import Session

def perform(alias, reason):
    if not DeletedAlias.get_by(email=alias.email):
        deleted = DeletedAlias(email=alias.email, reason=reason)
        Session.add(deleted)
    if alias.custom_domain_id:
        Session.add(DomainDeletedAlias(email=alias.email))
    other = make(alias)
    Session.add(other)
    item = alias
    if reason:
        item = DeletedAlias(email=alias.email)
    Session.add(item)
    if reason:
        late = DeletedAlias(email=alias.email)
    Session.add(late)
    Alias.filter(Alias.id == alias.id).delete()
    Local.filter(x=1).delete()

def suggest(user, hostname):
    if hostname and user.include_website_in_one_click_alias:
        alias = Alias.create(user_id=user.id, email=hostname)
    found = Alias.get_by(email=hostname)
    local = Local.create(x=1)
"#;
    let out = recover(code);
    let found: Vec<(String, usize)> = rules(&out)
        .into_iter()
        .filter(|(_, r)| r.kind == RuleKind::GuardedEffect)
        .map(|(_, r)| (value(&r).1, r.conditions.len()))
        .collect();
    assert_eq!(
        found,
        [
            (
                "DeletedAlias(email=alias.email, reason=reason)".to_owned(),
                1
            ),
            ("DomainDeletedAlias(email=alias.email)".to_owned(), 1),
            ("Alias.filter(Alias.id == alias.id).delete()".to_owned(), 0),
            (
                "Alias.create(user_id=user.id, email=hostname)".to_owned(),
                1
            ),
        ],
        "{found:?}"
    );
}

#[test]
fn a_local_discarded_or_cleaned_under_a_branch_is_a_guarded_effect() {
    // AC-0910 (T-0910, #1998): under an `if` branch, a local bound earlier
    // (also by unpacking) and set to `None` or to its own
    // `.replace("…", "…")` is an effect. A first binding, an unbranched
    // rebinding, another value and a `.replace` of another name are not.
    let code = r#"
def suggest(user, hostname, data):
    alias = Alias.get_by(email=hostname)
    if alias and not alias.user_id == user.id:
        alias = None
    new_name = data.get("name")
    if new_name:
        new_name = new_name.replace("\n", "")
        other = None
        new_name = other.replace("\n", "")
        alias = make(alias)
    alias = None
    if hostname:
        hostname = hostname.replace(old, "")

def unpacked(user):
    found, meta = load(user)
    if found and found.user_id != user.id:
        found = None
"#;
    let out = recover(code);
    let found: Vec<(String, usize)> = rules(&out)
        .into_iter()
        .filter(|(_, r)| r.kind == RuleKind::GuardedEffect)
        .map(|(_, r)| (value(&r).1, r.conditions.len()))
        .collect();
    assert_eq!(
        found,
        [
            ("alias = None".to_owned(), 1),
            (r#"new_name = new_name.replace("\n", "")"#.to_owned(), 1),
            ("found = None".to_owned(), 1),
        ],
        "{found:?}"
    );
}

#[test]
fn guarded_returns_and_raises_are_owned_by_their_function() {
    // AC-0852: if / elif / else branches, outermost first; a nested function
    // and a lambda own their own exits; an unguarded or bare exit is none.
    let code = r#"
from flask import jsonify

def check(alias):
    return alias is not None

def update_alias(alias_id, user):
    alias = Alias.get(alias_id)
    if not alias or alias.user_id != user.id:
        return jsonify(error="Forbidden"), 403
    if alias.disabled:
        raise AliasDisabled(f"alias {alias_id} is disabled")
    elif not check(alias):
        return jsonify(error="invalid"), 400
    else:
        if user.trial:
            raise ValueError("trial")
    try:
        alias.save()
    except Exception:
        if alias.pinned:
            raise
    def inner(x):
        if x:
            return 1
    handler = lambda y: y
    return jsonify(alias=alias.email), 200

class Box:
    def open(self, key):
        if key is None:
            raise KeyError("key")
"#;
    let out = recover(code);
    let rules = rules(&out);
    let owners: Vec<_> = rules.iter().map(|(_, r)| r.owner_id.as_str()).collect();
    assert_eq!(
        owners,
        [
            "sym:fixture@app/views.py#Box.open",
            "sym:fixture@app/views.py#update_alias",
            "sym:fixture@app/views.py#update_alias",
            "sym:fixture@app/views.py#update_alias",
            "sym:fixture@app/views.py#update_alias",
            "sym:fixture@app/views.py#update_alias.inner",
        ]
    );
    let update: Vec<_> = rules
        .iter()
        .filter(|(_, r)| r.owner_id.ends_with("#update_alias"))
        .map(|(_, r)| r)
        .collect();
    use BranchPolarity::{FalsyBranch as F, TruthyBranch as T};
    assert_eq!(
        shown(update[0]),
        [("not alias or alias.user_id != user.id".into(), T)]
    );
    assert_eq!(
        value(update[0]),
        ("return", r#"jsonify(error="Forbidden"), 403"#.into())
    );
    assert_eq!(shown(update[1]), [("alias.disabled".into(), T)]);
    assert_eq!(
        value(update[1]),
        (
            "raise",
            r#"AliasDisabled(f"alias {alias_id} is disabled")"#.into()
        )
    );
    assert_eq!(
        shown(update[2]),
        [("alias.disabled".into(), F), ("not check(alias)".into(), T)]
    );
    assert_eq!(
        shown(update[3]),
        [
            ("alias.disabled".into(), F),
            ("not check(alias)".into(), F),
            ("user.trial".into(), T)
        ]
    );
    assert_eq!(value(update[3]), ("raise", r#"ValueError("trial")"#.into()));
    // The call to a function this file defines is a Target; the parameter
    // never rebound is a Binding; `alias` is rebound, so Unresolved.
    let deps = &update[2].dependencies;
    assert!(deps.iter().any(|d| d.resolution
        == DependencyResolution::Target {
            node_id: "sym:fixture@app/views.py#check".into()
        }));
    assert!(deps.iter().any(|d| matches!(
        &d.resolution,
        DependencyResolution::Unresolved { gap_id } if gap_id.ends_with(":unresolved_binding")
    )));
    assert!(update[1].dependencies.iter().any(|d| matches!(
        &d.resolution,
        DependencyResolution::Binding { .. }
    ) && d.role == DependencyRole::ExitValue));
    // Interpretation stays open, with each reason a Gap the rule depends on.
    let (node, first) = &rules[1];
    let reasons: BTreeSet<_> = first
        .interpretation
        .gap_ids
        .iter()
        .map(|id| id.rsplit(':').next().unwrap())
        .collect();
    for reason in [
        "execution_predicate_unknown",
        "consumer_semantics_unknown",
        "exception_control",
    ] {
        assert!(reasons.contains(reason), "{reason}");
    }
    let depends: Vec<_> = out
        .edges
        .iter()
        .filter(|e| e.src == node.id && e.label == "DEPENDS_ON")
        .collect();
    assert_eq!(depends.len(), first.interpretation.gap_ids.len());
    assert!(out.edges.iter().any(|e| e.label == "GOVERNS"
        && e.src == node.id
        && e.dst == "sym:fixture@app/views.py#update_alias"));
}

#[test]
fn stored_displays_are_sanitized() {
    // AC-0852 (SPEC-02): a credential-shaped literal, a sensitive keyword's
    // value and comments never reach a stored display; a byte string is
    // withheld whole.
    let code = r#"
def login(password, token):
    if password == "hunter2-very-secret":
        return jsonify(error="bad"), 401
    if token:  # check the token
        raise AuthError(api_key="AKIAABCDEFGHIJKLMNOP")
    if b"raw" in token:
        raise ValueError(b"raw")
"#;
    let out = recover(code);
    let rules = rules(&out);
    assert_eq!(rules.len(), 3);
    assert_eq!(shown(&rules[0].1)[0].0, format!("password == {WITHHELD}"));
    assert_eq!(
        rules[0].1.conditions[0].expression.capture,
        ExpressionCapture::Redacted
    );
    assert_eq!(
        value(&rules[1].1).1,
        format!("AuthError(api_key={WITHHELD})")
    );
    assert_eq!(shown(&rules[2].1)[0].0, UNSUPPORTED);
    assert_eq!(value(&rules[2].1).1, UNSUPPORTED);
    let stored = serde_json::to_string(&out.nodes).unwrap();
    for secret in ["hunter2", "AKIAABCDEFGHIJKLMNOP", "check the token"] {
        assert!(!stored.contains(secret), "{secret}");
    }
}

#[test]
fn recovered_syntax_yields_one_omission_gap_and_no_rules() {
    // AC-0852: a file that does not parse cleanly states what was omitted.
    let out = recover("def f(x):\n    if x:\n        return (\n");
    assert!(rules(&out).is_empty());
    let gaps: Vec<_> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Gap" && n.props["rule_evidence_gap"] == true)
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].props["reason_code"], "unsupported_rule_syntax");
}

#[test]
fn cached_rules_are_retargeted_to_the_ingested_commit() {
    // AC-0852: unchanged files reused from the cache cite the new revision.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("views.py"),
        "def f(x):\n    if x:\n        raise ValueError('x')\n",
    )
    .unwrap();
    let mut cache = IncrementalCache::default();
    let first = SourceId {
        repo: "fixture",
        commit: "revision-a",
    };
    extract_dir_incremental(dir.path(), &first, &mut cache).unwrap();
    let second = SourceId {
        repo: "fixture",
        commit: "revision-b",
    };
    let (out, _) = extract_dir_incremental(dir.path(), &second, &mut cache).unwrap();
    let (node, rule) = rules(&out).pop().unwrap();
    assert_eq!(rule.exit_source.commit_sha, "revision-b");
    assert_eq!(
        rule.conditions[0].expression.source.commit_sha,
        "revision-b"
    );
    assert_eq!(
        node.props["prov"]["content_hash"],
        serde_json::json!(core_prov::content_hash(
            &serde_json::to_vec(&node.props["rule"]).unwrap()
        ))
    );
}

/// Whether the guarded exit in `source` resolves the parameter its
/// condition reads to a Binding (true) or leaves it Unresolved (false).
fn binds(source: &str) -> bool {
    let out = recover(source);
    let rules = rules(&out);
    assert_eq!(rules.len(), 1, "{source}");
    let deps: Vec<_> = rules[0]
        .1
        .dependencies
        .iter()
        .filter(|d| d.role == DependencyRole::Condition)
        .collect();
    let bound = deps
        .iter()
        .any(|d| matches!(d.resolution, DependencyResolution::Binding { .. }));
    let unresolved = deps.iter().any(|d| {
        matches!(
            &d.resolution,
            DependencyResolution::Unresolved { gap_id } if gap_id.ends_with(":unresolved_binding")
        )
    });
    assert_ne!(bound, unresolved, "{source}: {deps:?}");
    bound
}

// AC-0852: a parameter is a stable Binding only when the function never
// rebinds it, counting imports and nested definitions, and not counting
// bindings that belong to nested scopes.
#[test]
fn parameter_bindings_follow_python_scopes() {
    // `*args` / `**kwargs`, typed or not, are parameters.
    assert!(binds(
        "def f(*args):\n    if args:\n        raise ValueError()\n"
    ));
    assert!(binds(
        "def f(**kw: int):\n    if kw:\n        raise ValueError()\n"
    ));
    // An import or a nested def rebinds the parameter.
    for rebinding in [
        "import config",
        "import config.sub",
        "import other as config",
        "from pkg import config",
        "from pkg import other as config",
        "def config(): pass",
        "class config: pass",
    ] {
        let source = format!(
            "def f(config):\n    {rebinding}\n    if config.disabled:\n        raise ValueError()\n"
        );
        assert!(!binds(&source), "{rebinding}");
    }
    // Bindings inside nested scopes are their own.
    for nested in [
        "def inner():\n        config = 1",
        "g = lambda config: config",
        "xs = [config for config in range(3)]",
        "class C:\n        config = 1",
    ] {
        let source = format!(
            "def f(config):\n    {nested}\n    if config.disabled:\n        raise ValueError()\n"
        );
        assert!(binds(&source), "{nested}");
    }
    // ... except `nonlocal`, and a walrus in a comprehension.
    for rebinding in [
        "def inner():\n        nonlocal config\n        config = 1",
        "xs = [(config := y) for y in range(3)]",
    ] {
        let source = format!(
            "def f(config):\n    {rebinding}\n    if config.disabled:\n        raise ValueError()\n"
        );
        assert!(!binds(&source), "{rebinding}");
    }
}

#[test]
fn test_code_yields_no_business_rules() {
    // AC-0922 (#2009): guards in test code are not business rules.
    let source = "def check(ready):\n    if not ready:\n        raise ValueError('not ready')\n    return 1\n";
    let at = |path: &str| {
        extract_source(
            source.as_bytes(),
            path,
            &SourceId {
                repo: "fixture",
                commit: "revision-a",
            },
        )
        .unwrap()
    };
    assert_eq!(rules(&at("app/views.py")).len(), 1);
    for path in ["tests/helpers.py", "app/test_views.py", "conftest.py"] {
        assert!(rules(&at(path)).is_empty(), "{path}");
    }
}

#[test]
fn a_predicates_unguarded_answer_is_a_rule() {
    // AC-0915 (T-0915): `return <expr>` with no branch is a rule only in a
    // predicate (`is_`, `has_`, `can_`, `should_`, `must_`), and a constant
    // answer only after an earlier exit (its "otherwise").
    let out = recover(
        r#"
class User:
    def is_active(self):
        return self.delete_on is None

    def name(self):
        return self.first

def has_quota(user):
    return user.count < 5

def can_always(user):
    return True

def must_pay(user):
    if user.premium:
        return False
    return True
"#,
    );
    let found: Vec<(String, String)> = rules(&out)
        .into_iter()
        .map(|(_, r)| {
            (
                r.owner_id.rsplit('#').next().unwrap().to_owned(),
                value(&r).1,
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            (
                "User.is_active".to_owned(),
                "self.delete_on is None".to_owned()
            ),
            ("has_quota".to_owned(), "user.count < 5".to_owned()),
            ("must_pay".to_owned(), "False".to_owned()),
            ("must_pay".to_owned(), "True".to_owned()),
        ]
    );
}

#[test]
fn a_condition_local_assigned_once_carries_its_initializer() {
    // AC-1034 (T-1034): a condition's name bound once by `name = value`
    // before the condition is a Binding with its initializer recorded, and
    // a name that initializer reads is followed the same way. A name
    // assigned only after the condition stays unresolved.
    let code = r#"
from flask import request, g

def authorize_request():
    api_code = request.headers.get("Authentication")
    api_key = ApiKey.get_by(code=api_code)
    if not api_key:
        return "Wrong api key", 401
    g.user = api_key.user

def several():
    try:
        return load()
    except (ValueError, TypeError):
        return "bad", 400

def late():
    if not suffix:
        return "no suffix", 400
    suffix = 1
    return None
"#;
    let out = recover(code);
    let all = rules(&out);
    let (_, refusal) = all
        .iter()
        .find(|(_, r)| r.owner_id.ends_with("authorize_request") && value(r).0 == "return")
        .unwrap();
    refusal.validate().unwrap();
    let definitions = refusal.local_definitions.as_ref().unwrap();
    let shown: Vec<&str> = definitions
        .iter()
        .map(|d| d.initializer.display.as_str())
        .collect();
    assert_eq!(
        shown,
        [
            "ApiKey.get_by(code=api_code)",
            "request.headers.get(\"Authentication\")"
        ]
    );
    assert!(matches!(
        definitions[0].expression.nodes[0].kind,
        DefinitionExpressionKind::Call { .. }
    ));
    assert!(
        definitions[0]
            .expression
            .nodes
            .iter()
            .any(|n| matches!(n.kind, DefinitionExpressionKind::KeywordArgument { .. }))
    );
    assert!(
        refusal
            .dependencies
            .iter()
            .any(|d| d.role == DependencyRole::Condition
                && matches!(d.resolution, DependencyResolution::Binding { .. }))
    );
    let (_, late) = all
        .iter()
        .find(|(_, r)| r.owner_id.ends_with("late"))
        .unwrap();
    late.validate().unwrap();
    assert!(late.local_definitions.as_ref().unwrap().is_empty());
    assert!(
        late.dependencies
            .iter()
            .all(|d| d.role != DependencyRole::Condition
                || matches!(d.resolution, DependencyResolution::Unresolved { .. }))
    );
    // A handler naming several types states no branch (AC-1036).
    assert!(all.iter().all(|(_, r)| !r.owner_id.ends_with("several")
        || r.conditions.iter().all(|c| !matches!(
            c.polarity,
            BranchPolarity::RaisedBranch | BranchPolarity::UnraisedBranch
        ))));
    for (_, rule) in &all {
        rule.validate().unwrap();
    }
}

#[test]
fn a_condition_local_takes_the_assignment_that_reaches_it() {
    // AC-1040 (T-1040): a name bound more than once takes the last plain
    // assignment before the condition in an enclosing block. A binding in
    // an earlier `if` clause cannot run before a later clause's condition,
    // and `x = f(x)` reads the earlier value; a binding that could run in
    // between, or come round again in a loop, leaves it unresolved. An
    // index and a sum are kept as written.
    let code = r#"
def suggest(user, hostname):
    alias = None
    if hostname:
        prefix = hostname.split(".")
        prefix = convert_to_id(prefix)
        suggested = prefix + suffixes[0].suffix
        alias = Alias.get_by(email=suggested)
        if alias and not alias.user_id == user.id:
            alias = None
        elif alias and alias.user_id == user.id:
            return alias
        elif not alias:
            return create(suggested)
    return None

def branch(flag):
    value = load()
    if flag:
        value = None
    if not value:
        return "missing", 404

def looped(items):
    value = load()
    for item in items:
        if not value:
            return "missing", 404
        value = item

def steady(items):
    value = None
    value = load()
    while items:
        if not value:
            return "absent", 404
"#;
    let out = recover(code);
    let all = rules(&out);
    for (_, rule) in &all {
        rule.validate().unwrap();
    }
    let initializers = |owner: &str, effect: &str| -> Vec<String> {
        let (_, rule) = all
            .iter()
            .find(|(_, r)| r.owner_id.ends_with(owner) && value(r).1.contains(effect))
            .unwrap_or_else(|| panic!("{owner} {effect}"));
        rule.local_definitions
            .iter()
            .flatten()
            .map(|d| d.initializer.display.as_str().to_owned())
            .collect()
    };
    for effect in ["alias", "create(suggested)"] {
        let shown = initializers("suggest", effect);
        assert_eq!(
            shown,
            [
                "Alias.get_by(email=suggested)",
                "prefix + suffixes[0].suffix",
                "convert_to_id(prefix)",
                "hostname.split(\".\")",
            ],
            "{effect}"
        );
    }
    assert!(initializers("branch", "missing").is_empty());
    assert!(initializers("looped", "missing").is_empty());
    // A loop with no binding of the name inside it carries nothing back.
    assert_eq!(initializers("steady", "absent"), ["load()"]);
    let (_, created) = all
        .iter()
        .find(|(_, r)| value(r).1.contains("create(suggested)"))
        .unwrap();
    let kinds: Vec<_> = created
        .local_definitions
        .iter()
        .flatten()
        .flat_map(|d| d.expression.nodes.iter().map(|n| &n.kind))
        .collect();
    assert!(kinds.iter().any(|k| matches!(
        k,
        DefinitionExpressionKind::Binary {
            operator: BinaryOperator::Add,
            ..
        }
    )));
    assert!(
        kinds
            .iter()
            .any(|k| matches!(k, DefinitionExpressionKind::Member { computed: true, .. }))
    );
}
