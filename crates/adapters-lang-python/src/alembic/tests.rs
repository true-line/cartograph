use super::super::{SourceId, extract_dir};
use core_graph::{Edge, Node};

const REPO: &str = "local/py";

const MODELS: &str = r#"
import sqlalchemy as sa
from sqlalchemy.orm import declarative_base, deferred

Base = declarative_base()

class User(Base):
    __tablename__ = "users"
    id = sa.Column(sa.Integer, primary_key=True)
    email = sa.Column(EmailType)
    note = deferred(sa.Column(sa.Text))
    display = sa.Column("full_name", sa.String)

class Post(Base):
    __tablename__ = "post"
    id = sa.Column(sa.Integer, primary_key=True)
    user_id = sa.Column(sa.ForeignKey("users.id"))
"#;

fn revision(id: &str, down: &str, upgrade: &str) -> String {
    format!(
        "from alembic import op\nimport sqlalchemy as sa\n\nrevision = '{id}'\ndown_revision = {down}\n\n\ndef upgrade():\n{upgrade}\n\n\ndef downgrade():\n    op.drop_table('anything')\n"
    )
}

/// Five revisions with a branch and a merge, replaying to `MODELS`.
fn history() -> Vec<(String, String)> {
    [
        ("a1_init.py", revision("a1", "None", "    op.create_table('users', sa.Column('id', sa.Integer(), nullable=False), sa.Column('email', sa.String()), sa.PrimaryKeyConstraint('id'))")),
        ("b2_note.py", revision("b2", "'a1'", "    op.add_column('users', sa.Column('note', sa.Text()))\n    op.create_index('ix_users_email', 'users', ['email'])")),
        ("c3_post.py", revision("c3", "'b2'", "    op.create_table('post', sa.Column('id', sa.Integer()), sa.Column('author_id', sa.Integer()))\n    op.alter_column('post', 'author_id', new_column_name='user_id')")),
        ("c4_name.py", revision("c4", "'b2'", "    op.execute('ALTER TABLE users ADD COLUMN full_name varchar(64)')\n    op.execute(sa.text(\"UPDATE users SET full_name = email; ALTER TABLE users ALTER COLUMN full_name SET NOT NULL\"))")),
        ("d5_merge.py", revision("d5", "('c3', 'c4')", "    with op.get_context().autocommit_block():\n        op.create_index('ix_post_user', 'post', ['user_id'])\n        op.alter_column('post', 'user_id', nullable=False)")),
    ]
    .into_iter()
    .map(|(name, body)| (format!("migrations/versions/{name}"), body))
    .collect()
}

fn extract(
    models: &str,
    revisions: &[(String, String)],
    extra: &[(&str, &str)],
) -> (Vec<Node>, Vec<Edge>) {
    let dir = tempfile::tempdir().unwrap();
    let mut files: Vec<(String, String)> = vec![
        (
            "alembic.ini".into(),
            "[alembic]\nscript_location = migrations\n".into(),
        ),
        ("app/models.py".into(), models.into()),
    ];
    files.extend(revisions.iter().cloned());
    files.extend(extra.iter().map(|(p, b)| (p.to_string(), b.to_string())));
    for (path, body) in &files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let out = extract_dir(
        dir.path(),
        &SourceId {
            repo: REPO,
            commit: "workdir",
        },
    )
    .unwrap();
    (out.nodes, out.edges)
}

fn drift(nodes: &[Node]) -> Vec<String> {
    let mut found: Vec<String> = nodes
        .iter()
        .filter(|n| n.props["kind"] == "schema-drift")
        .map(|n| {
            format!(
                "{}:{}",
                n.props["drift_kind"].as_str().unwrap(),
                n.props["key"].as_str().unwrap()
            )
        })
        .collect();
    found.sort();
    found
}

fn gaps(nodes: &[Node]) -> Vec<String> {
    let mut found: Vec<String> = nodes
        .iter()
        .filter(|n| n.label == "Gap" && n.props["kind"] == "migration-gap")
        .map(|n| n.props["reason"].as_str().unwrap().to_string())
        .collect();
    found.sort();
    found
}

fn with(revisions: &[(String, String)], name: &str, body: String) -> Vec<(String, String)> {
    let mut revisions = revisions.to_vec();
    revisions.push((format!("migrations/versions/{name}"), body));
    revisions
}

// AC-0338: an Alembic history with a branch and a merge replays, through
// literal ops, exact raw-SQL column changes and Alembic's autocommit block,
// to exactly the modeled tables and columns (one of an unknown type, a
// `deferred` one and a renamed one by its database name): nothing drifts,
// and what was compared is recorded. (T-0338)
#[test]
fn alembic_history_replays_to_the_modeled_schema() {
    let (nodes, _) = extract(MODELS, &history(), &[]);
    assert_eq!(drift(&nodes), Vec::<String>::new());
    assert_eq!(gaps(&nodes), Vec::<String>::new());
    let history = nodes
        .iter()
        .find(|n| n.label == "MigrationHistory")
        .unwrap();
    assert_eq!(history.props["head"], "d5");
    assert_eq!(history.props["revisions"], 5);
    assert_eq!(history.props["modeled_tables_compared"], 2);
    assert_eq!(history.props["modeled_columns_compared"], 6);
    assert_eq!(history.props["prov"]["confidence_tier"], "Confirmed");

    // A semicolon inside a quoted value does not end the statement.
    let quoted = with(
        &super::tests::history(),
        "e6_x.py",
        revision(
            "e6",
            "'d5'",
            "    op.execute(\"UPDATE users SET email = '; ALTER TABLE users DROP COLUMN email'\")",
        ),
    );
    let (nodes, _) = extract(MODELS, &quoted, &[]);
    assert_eq!(drift(&nodes), Vec::<String>::new());
    assert_eq!(gaps(&nodes), Vec::<String>::new());
}

// AC-0338: each difference is a cited InferredStrong Drift, both ways; a
// table a model file spells (a `Table(...)`) is not reported. (T-0338)
#[test]
fn alembic_drift_is_reported_both_ways() {
    let models = format!(
        "{MODELS}\nclass Tag(Base):\n    __tablename__ = 'tag'\n    id = sa.Column(sa.Integer, primary_key=True)\n\nassoc = sa.Table('post_tag', Base.metadata)\n"
    );
    let revisions = with(
        &history(),
        "e6_more.py",
        revision(
            "e6",
            "'d5'",
            "    op.drop_column('users', 'note')\n    op.add_column('post', sa.Column('legacy', sa.Integer()))\n    op.create_table('audit', sa.Column('id', sa.Integer()))\n    op.create_table('post_tag', sa.Column('post_id', sa.Integer()))",
        ),
    );
    let (nodes, edges) = extract(&models, &revisions, &[]);
    assert_eq!(
        drift(&nodes),
        [
            "migrated-not-modeled:audit",
            "migrated-not-modeled:post.legacy",
            "modeled-not-migrated:tag",
            "modeled-not-migrated:users.note",
        ]
    );
    let note = nodes
        .iter()
        .find(|n| n.props["key"] == "users.note")
        .unwrap();
    assert_eq!(note.props["prov"]["confidence_tier"], "InferredStrong");
    assert_eq!(note.props["migrated"], "absent at head e6");
    assert!(
        note.props["modeled"]
            .as_str()
            .unwrap()
            .starts_with("app/models.py bytes ")
    );
    let legacy = nodes
        .iter()
        .find(|n| n.props["key"] == "post.legacy")
        .unwrap();
    assert!(
        legacy.props["migrated"]
            .as_str()
            .unwrap()
            .starts_with("migrations/versions/e6_more.py bytes ")
    );
    assert!(edges.iter().any(|e| e.src == note.id
        && e.label == "MODELED_AS"
        && e.dst == "data:local/py@sqlalchemy:users"));
    assert!(
        !edges
            .iter()
            .any(|e| e.src.contains("@audit:") && e.label == "MODELED_AS")
    );
    // A model column whose database name is computed: extra migrated
    // columns on that table are not reported.
    let opaque = models.replace(
        "    user_id = sa.Column(sa.ForeignKey(\"users.id\"))\n",
        "    user_id = sa.Column(sa.ForeignKey(\"users.id\"))\n    other = sa.Column(COLUMN, sa.Integer)\n    extra = sa.Column(sa.Integer, name=NAME)\n",
    );
    assert_ne!(opaque, models);
    // A literal `schema=` qualifies the table as the model's schema does.
    let auth = format!(
        "{MODELS}\nclass Login(Base):\n    __tablename__ = 'users'\n    __table_args__ = {{'schema': 'auth'}}\n    id = sa.Column(sa.Integer, primary_key=True)\n"
    );
    let in_auth = with(
        &history(),
        "e6_auth.py",
        revision(
            "e6",
            "'d5'",
            "    op.create_table('users', sa.Column('id', sa.Integer()), schema='auth')",
        ),
    );
    let (nodes, _) = extract(&auth, &in_auth, &[]);
    assert_eq!(drift(&nodes), Vec::<String>::new());
    let renamed = with(
        &history(),
        "e6_auth.py",
        revision(
            "e6",
            "'d5'",
            "    op.create_table('legacy', sa.Column('id', sa.Integer()), schema='auth')\n    op.rename_table('legacy', 'users', schema='auth')",
        ),
    );
    let (nodes, _) = extract(&auth, &renamed, &[]);
    assert_eq!(drift(&nodes), Vec::<String>::new());
    let (nodes, _) = extract(&auth, &history(), &[]);
    assert_eq!(drift(&nodes), ["modeled-not-migrated:auth.users"]);
    let (nodes, _) = extract(&opaque, &revisions, &[]);
    assert_eq!(
        drift(&nodes),
        [
            "migrated-not-modeled:audit",
            "modeled-not-migrated:tag",
            "modeled-not-migrated:users.note",
        ]
    );
}

// AC-0338: what the replay cannot follow is never guessed. A table an op
// cannot be replayed for is a Gap naming the op and is not compared; a
// history that cannot be replayed at all is one Gap and no drift. (T-0338)
#[test]
fn alembic_replay_fails_closed() {
    // Per table: the modeled `users` is named as unchecked, nothing drifts.
    let unchecked = |upgrade: &str| {
        let revisions = with(&history(), "e6_x.py", revision("e6", "'d5'", upgrade));
        let (nodes, _) = extract(MODELS, &revisions, &[]);
        assert_eq!(drift(&nodes), Vec::<String>::new(), "{upgrade}");
        let found = gaps(&nodes);
        assert_eq!(found.len(), 1, "{upgrade}: {found:?}");
        assert!(
            found[0].starts_with("schema drift not checked for `users`: "),
            "{found:?}"
        );
        found[0].clone()
    };
    assert!(
        unchecked("    if True:\n        op.drop_column('users', 'note')")
            .ends_with("runs conditionally or in batch mode")
    );
    assert!(
        unchecked("    if True:\n        op.execute('ALTER TABLE users ADD COLUMN phantom int')")
            .ends_with("runs conditionally or in batch mode")
    );
    assert!(
        unchecked("    match flag:\n        case 1:\n            op.drop_column('users', 'note')")
            .ends_with("runs conditionally or in batch mode")
    );
    assert!(
        unchecked("    for _ in []:\n        op.execute('ALTER TABLE users DROP COLUMN note')")
            .ends_with("runs conditionally or in batch mode")
    );
    assert!(
        unchecked(
            "    with op.batch_alter_table('users') as batch:\n        batch.drop_column('note')"
        )
        .ends_with("runs conditionally or in batch mode")
    );
    assert!(
        unchecked("    op.add_column('users', sa.Column(NAME, sa.Text()))")
            .ends_with("an added column is not a literal Column")
    );
    assert!(
        unchecked("    op.execute('ALTER TABLE users RENAME COLUMN note TO memo')")
            .ends_with("raw SQL reshapes it")
    );
    assert!(
        unchecked("    op.execute('ALTER TABLE users ADD COLUMN a int, ADD COLUMN b int')")
            .ends_with("raw SQL reshapes it")
    );
    assert!(
        unchecked("    op.drop_column('users', 'missing')")
            .ends_with("a dropped column is not in the replay")
    );
    assert!(
        unchecked("    op.create_table('users', sa.Column('id', sa.Integer()))")
            .ends_with("created while it already exists")
    );
    assert!(
        unchecked("    op.create_table_from_select('users', query)")
            .ends_with("op.create_table_from_select is not replayed")
    );
    // A table no model maps is a Gap too when the replay loses it.
    let audit = with(
        &history(),
        "e6_x.py",
        revision(
            "e6",
            "'d5'",
            "    op.create_table('audit', sa.Column('id', sa.Integer()))\n    if True:\n        op.drop_column('audit', 'id')",
        ),
    );
    let (nodes, _) = extract(MODELS, &audit, &[]);
    assert_eq!(drift(&nodes), Vec::<String>::new());
    assert_eq!(
        gaps(&nodes),
        [
            "schema drift not checked for `audit`: a migration op on it runs conditionally or in batch mode"
        ]
    );
    // Two parallel branches both change `users`: their order is not fixed.
    let mut parallel = history();
    parallel.push((
        "migrations/versions/e6_x.py".into(),
        revision(
            "e6",
            "'d5'",
            "    op.add_column('users', sa.Column('x', sa.Text()))",
        ),
    ));
    parallel.push((
        "migrations/versions/e7_y.py".into(),
        revision("e7", "'d5'", "    op.drop_column('users', 'x')"),
    ));
    parallel.push((
        "migrations/versions/f8_m.py".into(),
        revision("f8", "('e6', 'e7')", "    pass"),
    ));
    let (nodes, _) = extract(MODELS, &parallel, &[]);
    assert_eq!(
        gaps(&nodes),
        ["schema drift not checked for `users`: changed on two parallel migration branches"]
    );

    // The whole history: one Gap, no drift, no history node.
    let refused = |revisions: Vec<(String, String)>, extra: &[(&str, &str)]| {
        let (nodes, _) = extract(MODELS, &revisions, extra);
        assert_eq!(drift(&nodes), Vec::<String>::new());
        assert!(
            !nodes.iter().any(|n| n.label == "MigrationHistory"),
            "{:?}",
            gaps(&nodes)
        );
        let found = gaps(&nodes);
        assert_eq!(found.len(), 1, "{found:?}");
        found[0]
            .strip_prefix("Alembic history not replayed: ")
            .unwrap()
            .to_string()
    };
    let two_heads = with(&history(), "e6_x.py", revision("e6", "'c3'", "    pass"));
    assert_eq!(
        refused(two_heads, &[]),
        "migration history has more than one root or head"
    );
    let orphan = with(&history(), "e6_x.py", revision("e6", "'zz'", "    pass"));
    assert_eq!(refused(orphan, &[]), "a down_revision names no migration");
    let duplicate = with(&history(), "e6_x.py", revision("d5", "'d5'", "    pass"));
    assert_eq!(
        refused(duplicate, &[]),
        "two migrations share a revision id"
    );
    let computed = with(&history(), "e6_x.py", "from alembic import op\nrevision = make()\ndown_revision = 'd5'\ndef upgrade():\n    pass\n".into());
    assert_eq!(
        refused(computed, &[]),
        "migrations/versions/e6_x.py has no literal revision"
    );
    let sql = with(
        &history(),
        "e6_x.py",
        revision("e6", "'d5'", "    op.execute(statement)"),
    );
    assert_eq!(refused(sql, &[]), "raw SQL is not a literal");
    // A bind taken from `op` runs raw SQL like `op.execute`.
    let driver = with(
        &history(),
        "e6_x.py",
        revision(
            "e6",
            "'d5'",
            "    bind = op.get_bind()\n    bind.exec_driver_sql(statement)",
        ),
    );
    assert_eq!(refused(driver, &[]), "raw SQL is not a literal");
    let ddl = with(
        &history(),
        "e6_x.py",
        revision(
            "e6",
            "'d5'",
            "    op.execute('CREATE TABLE audit (id int)')",
        ),
    );
    assert_eq!(refused(ddl, &[]), "raw SQL may change any table");
    let qualified = with(
        &history(),
        "e6_x.py",
        revision(
            "e6",
            "'d5'",
            "    op.execute('ALTER TABLE public.users ADD COLUMN x int')",
        ),
    );
    assert_eq!(refused(qualified, &[]), "raw SQL may change any table");
    for sql in [
        "UPDATE users SET email = 'open",
        "UPDATE users SET email = 1 -- ; ALTER TABLE users DROP COLUMN email",
        "DO $$ BEGIN ALTER TABLE users DROP COLUMN email; END $$",
    ] {
        let unreadable = with(
            &history(),
            "e6_x.py",
            revision("e6", "'d5'", &format!("    op.execute(\"{sql}\")")),
        );
        assert_eq!(
            refused(unreadable, &[]),
            "raw SQL may change any table",
            "{sql}"
        );
    }
    // An escape inside the SQL literal is not read.
    let escaped = with(
        &history(),
        "e6_x.py",
        revision(
            "e6",
            "'d5'",
            r#"    op.execute("UPDATE users SET email = 'a\nb'")"#,
        ),
    );
    assert_eq!(refused(escaped, &[]), "raw SQL is not a literal");
    let computed_schema = with(
        &history(),
        "e6_x.py",
        revision(
            "e6",
            "'d5'",
            "    op.drop_column('users', 'note', schema=SCHEMA)",
        ),
    );
    assert_eq!(
        refused(computed_schema, &[]),
        "migration op names no literal schema"
    );
    let helper = with(
        &history(),
        "e6_x.py",
        format!(
            "{}\ndef helper():\n    op.drop_table('users')\n",
            revision("e6", "'d5'", "    helper()")
        ),
    );
    assert_eq!(refused(helper, &[]), "migration op outside upgrade()");
    let unbound = with(
        &history(),
        "e6_x.py",
        revision("e6", "'d5'", "    op.drop_table('users')")
            .replace("from alembic import op", "from somewhere import op"),
    );
    assert_eq!(
        refused(unbound, &[]),
        "`op` is not bound by `from alembic import op`"
    );
    // `op` must stay Alembic's: no rebinding, member replacement or
    // wildcard import anywhere in the revision.
    let replaced = |upgrade: &str, tail: &str| {
        with(
            &history(),
            "e6_x.py",
            format!("{}{tail}", revision("e6", "'d5'", upgrade)),
        )
    };
    let rebound = "`op` is rebound or its members replaced";
    let outside = "migration op outside upgrade()";
    let phantom = "    op.add_column('users', sa.Column('phantom', sa.String()))";
    for (upgrade, tail, reason) in [
        (phantom, "\nop = FakeOps()\n", outside),
        (
            "    op = FakeOps()\n    op.add_column('users', sa.Column('phantom', sa.String()))",
            "",
            rebound,
        ),
        (
            "    from fake import op\n    op.add_column('users', sa.Column('phantom', sa.String()))",
            "",
            rebound,
        ),
        (phantom, "\nop.add_column = print\n", outside),
        ("    op.impl.dialect = None", "", rebound),
        ("    op.impl['dialect'] = None", "", rebound),
        ("    del op.impl", "", rebound),
        (
            "    add = op.add_column\n    add('users', sa.Column('phantom', sa.String()))",
            "",
            rebound,
        ),
        ("    register(op)", "", rebound),
        (
            "    op.__dict__['add_column']('users', sa.Column('phantom', sa.String()))",
            "",
            rebound,
        ),
        ("    op.impl.add_column('users', None)", "", rebound),
        (
            "    op.get_context().bind.execute('DROP TABLE users')",
            "",
            rebound,
        ),
        (
            "    with op.get_context().begin_transaction():\n        pass",
            "",
            rebound,
        ),
        (
            "    op.get_bind().exec_driver_sql('DROP TABLE users')",
            "",
            rebound,
        ),
        ("    op.get_bind()['users']", "", rebound),
        ("    op.get_bind()()", "", rebound),
        (phantom, "\nfrom fake import *\n", rebound),
        // Module-level ops, in a statement, a conditional or a class body.
        ("    pass", "\nop.drop_table('users')\n", outside),
        (
            "    pass",
            "\nif True:\n    op.drop_table('users')\n",
            outside,
        ),
        (
            "    pass",
            "\nclass Steps:\n    op.drop_table('users')\n",
            outside,
        ),
    ] {
        assert_eq!(
            refused(replaced(upgrade, tail), &[]),
            reason,
            "{upgrade}{tail}"
        );
    }
    let unnamed = with(
        &history(),
        "e6_x.py",
        revision("e6", "'d5'", "    op.drop_table(NAME)"),
    );
    assert_eq!(refused(unnamed, &[]), "migration op names no literal table");
    let interpolated = refused(
        history(),
        &[(
            "alembic.ini",
            "[alembic]\nscript_location = %(here)s/migrations\n",
        )],
    );
    assert_eq!(
        interpolated,
        "alembic.ini names no literal relative script_location"
    );
}

// AC-1805 (#1653): the project's Alembic bounds decide what is replayed.
// More revision files than allowed, or a file past the size bound, leaves the
// replay a gap naming why; the defaults replay the same history, and bounds
// past the hard ceilings are refused.
#[test]
fn alembic_replay_follows_the_projects_bounds() {
    use super::super::{IncrementalCache, PythonLimits, extract_dir_incremental_within};
    let dir = tempfile::tempdir().unwrap();
    let mut files = vec![
        (
            "alembic.ini".to_string(),
            "[alembic]\nscript_location = migrations\n".to_string(),
        ),
        ("app/models.py".into(), MODELS.into()),
    ];
    files.extend(history());
    for (path, body) in &files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let run = |limits| {
        let (out, _) = extract_dir_incremental_within(
            dir.path(),
            &SourceId {
                repo: REPO,
                commit: "workdir",
            },
            &mut IncrementalCache::default(),
            &mut |_| {},
            limits,
        )
        .unwrap();
        gaps(&out.nodes)
    };
    assert_eq!(run(PythonLimits::default()), Vec::<String>::new());
    let few = PythonLimits::within(4, 1 << 20, 8 << 20).unwrap();
    assert_eq!(
        run(few),
        ["Alembic history not replayed: more than 4 revision files"]
    );
    let small = PythonLimits::within(4096, 16, 8 << 20).unwrap();
    assert_eq!(
        run(small),
        ["Alembic history not replayed: alembic.ini is not a regular file within the size limit"]
    );
    assert_eq!(
        PythonLimits::within(4096, 1 << 20, 8 << 20),
        Some(PythonLimits::default())
    );
    assert!(PythonLimits::within(0, 1 << 20, 8 << 20).is_none());
    assert!(
        PythonLimits::within(
            PythonLimits::HARD_MAX_ALEMBIC_REVISIONS + 1,
            1 << 20,
            8 << 20
        )
        .is_none()
    );
    assert!(
        PythonLimits::within(4096, PythonLimits::HARD_MAX_ALEMBIC_FILE_BYTES + 1, 8 << 20)
            .is_none()
    );
    assert!(
        PythonLimits::within(4096, 1 << 20, PythonLimits::HARD_MAX_LOCKFILE_BYTES + 1).is_none()
    );
    assert!(
        PythonLimits::within(
            PythonLimits::HARD_MAX_ALEMBIC_REVISIONS,
            PythonLimits::HARD_MAX_ALEMBIC_FILE_BYTES,
            PythonLimits::HARD_MAX_LOCKFILE_BYTES
        )
        .is_some()
    );
}

// AC-0338: a symlink on the way to the revisions is refused. (T-0338)
#[cfg(unix)]
#[test]
fn alembic_refuses_symlinked_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real/versions");
    std::fs::create_dir_all(&real).unwrap();
    for (path, body) in history() {
        let name = path.rsplit('/').next().unwrap().to_string();
        std::fs::write(real.join(name), body).unwrap();
    }
    std::fs::write(
        dir.path().join("alembic.ini"),
        "[alembic]\nscript_location = migrations\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("migrations")).unwrap();
    let out = extract_dir(
        dir.path(),
        &SourceId {
            repo: REPO,
            commit: "workdir",
        },
    )
    .unwrap();
    assert_eq!(
        gaps(&out.nodes),
        ["Alembic history not replayed: migrations/versions is not a plain directory"]
    );
}
