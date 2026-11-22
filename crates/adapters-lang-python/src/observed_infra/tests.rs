use super::*;
use std::collections::BTreeMap;

const REPO: &str = "local/py";

fn extract(files: &[(&str, &str)]) -> (Vec<Node>, Vec<Edge>) {
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    extract_dir(
        dir.path(),
        &SourceId {
            repo: REPO,
            commit: "workdir",
        },
        Registries::builtin(),
    )
    .unwrap()
}

fn uses<'a>(edges: &'a [Edge], file: &str) -> Vec<&'a str> {
    let mut found: Vec<&str> = edges
        .iter()
        .filter(|edge| edge.label == "USES" && edge.src.contains(file))
        .map(|edge| edge.dst.as_str())
        .collect();
    found.sort();
    found
}

fn res(logical: &str) -> String {
    format!("res:{REPO}@observed:{logical}")
}

// AC-0318: import-proven Python clients become the same observed resources
// the TypeScript producer emits: one per provider and instance, a URL kept
// as scheme and authority only, an env read by name, and boto3 told apart
// by the service it names. (T-0318)
#[test]
fn proven_python_clients_become_observed_resources() {
    let (nodes, edges) = extract(&[
        (
            "app/infra.py",
            r#"
import os
import os.path
import boto3
import redis as r
import smtplib
from redis import Redis as R
from sqlalchemy import create_engine
from os import getenv

ENGINE_URL = os.environ.get("DATABASE_URL")
boto3.DEFAULT_SESSION = None

def storage():
    import boto3
    return boto3.client("s3")

def queue():
    return boto3.resource("sqs", region_name="eu-west-1")

def cache():
    return R(host="cache.internal", port=6379)

def cache_url():
    return r.from_url("redis://user:secret@cache.internal:6379/0")

def mail():
    return smtplib.SMTP(getenv("SMTP_HOST"))

engine = create_engine(ENGINE_URL)
ec2 = boto3.client("ec2")
"#,
        ),
        (
            "app/sibling.py",
            "import redis\nredis.Redis.timeout = 3\nredis.Redis.from_url(\"redis://cache.internal\")\n",
        ),
        (
            "app/order.py",
            "def early():\n    import smtplib\n\nimport smtplib\nsmtplib.SMTP_PORT = 25\nsmtplib.SMTP(\"mail.internal\")\n",
        ),
    ]);
    // A local import of the same module, before or after, keeps the proof;
    // so does a write to a sibling member.
    assert_eq!(uses(&edges, "order.py"), vec![res("smtp:mail.internal")]);
    assert_eq!(
        uses(&edges, "sibling.py"),
        vec![res("redis:redis://cache.internal")]
    );
    assert_eq!(
        uses(&edges, "infra.py"),
        vec![
            res("aws-s3"),
            res("aws-sqs"),
            res("redis:cache.internal"),
            res("redis:redis://cache.internal:6379"),
            res("smtp:env:SMTP_HOST"),
            res("sql-database:env:DATABASE_URL"),
        ]
    );
    let s3 = nodes.iter().find(|n| n.id == res("aws-s3")).unwrap();
    assert_eq!(s3.props["kind"], "sdk-client");
    assert_eq!(s3.props["observed"], true);
    assert_eq!(s3.props["module"], "boto3");
    let smtp = nodes
        .iter()
        .find(|n| n.id == res("smtp:env:SMTP_HOST"))
        .unwrap();
    assert_eq!(smtp.props["env_key"], "SMTP_HOST");
    // Credentials, paths and the ec2 client (no registry entry) never appear.
    let all = serde_json::to_string(&(&nodes, &edges)).unwrap();
    assert!(!all.contains("secret"), "{all}");
    assert!(!all.contains("ec2"), "{all}");
    let storage = edges.iter().find(|edge| edge.dst == res("aws-s3")).unwrap();
    assert_eq!(storage.src, format!("sym:{REPO}@app/infra.py#storage"));
    assert_eq!(storage.props["registry"], "builtin:observed-clients.json");
}

// AC-0318: a bound name the file writes anywhere else proves nothing, and
// a function-local or relative import never proves a client. (T-0318)
#[test]
fn rebound_or_local_imports_prove_nothing() {
    let (nodes, edges) = extract(&[
        (
            "a_assign.py",
            "import boto3\nboto3 = object()\nboto3.client(\"s3\")\n",
        ),
        (
            "b_def.py",
            "from redis import Redis\ndef Redis(**kw):\n    pass\nRedis(host=\"h\")\n",
        ),
        (
            "c_param.py",
            "import smtplib\ndef send(smtplib):\n    smtplib.SMTP(\"mail\")\n",
        ),
        (
            "d_with.py",
            "import boto3\nwith open(\"x\") as boto3:\n    pass\nboto3.client(\"s3\")\n",
        ),
        (
            "e_except.py",
            "import boto3\ntry:\n    pass\nexcept Exception as boto3:\n    pass\nboto3.client(\"s3\")\n",
        ),
        (
            "e_except_tuple.py",
            "import boto3\ntry:\n    pass\nexcept (ValueError, KeyError) as boto3:\n    pass\nboto3.client(\"s3\")\n",
        ),
        (
            "e_except_group.py",
            "import boto3\ntry:\n    pass\nexcept* ValueError as boto3:\n    pass\nboto3.client(\"s3\")\n",
        ),
        (
            "f_loop.py",
            "from redis import from_url\nfor from_url in []:\n    pass\nfrom_url(\"redis://h\")\n",
        ),
        (
            "g_reimport.py",
            "import boto3\nimport fake as boto3\nboto3.client(\"s3\")\n",
        ),
        (
            "h_local.py",
            "def make():\n    import boto3\n    return boto3.client(\"s3\")\n",
        ),
        (
            "i_star.py",
            "import boto3\nfrom helpers import *\nboto3.client(\"s3\")\n",
        ),
        (
            "j_walrus.py",
            "import boto3\nif (boto3 := None):\n    pass\nboto3.client(\"s3\")\n",
        ),
        (
            "k_relative.py",
            "from . import boto3\nboto3.client(\"s3\")\n",
        ),
        (
            "m_member.py",
            "import boto3\nboto3.client = lambda *a: None\nboto3.client(\"s3\")\n",
        ),
        (
            "n_setattr.py",
            "import redis\nsetattr(redis, \"Redis\", object)\nredis.Redis(host=\"h\")\n",
        ),
        (
            "o_item.py",
            "import smtplib\nsmtplib[\"SMTP\"] = object\nsmtplib.SMTP(\"m\")\n",
        ),
        (
            "p_dotted.py",
            "import redis\nredis.Redis = object\nredis.Redis.from_url(\"redis://h\")\n",
        ),
        (
            "q_nested.py",
            "import redis\nredis.Redis.from_url = object\nredis.Redis.from_url(\"redis://h\")\n",
        ),
        (
            "r_setattr_nested.py",
            "import redis\nsetattr(redis.Redis, \"from_url\", object)\nredis.Redis.from_url(\"redis://h\")\n",
        ),
        (
            "s_del.py",
            "import boto3\ndel boto3.client\nboto3.client(\"s3\")\n",
        ),
        (
            "t_tuple.py",
            "import smtplib\nsmtplib.SMTP, x = object, 1\nsmtplib.SMTP(\"m\")\n",
        ),
        (
            "u_beneath.py",
            "import redis\nredis.Redis.from_url.cache = {}\nredis.Redis.from_url(\"redis://h\")\n",
        ),
        (
            "v_symbol_member.py",
            "from redis import Redis as R\nR.__new__ = lambda *a, **k: object()\nR(host=\"cache.internal\")\n",
        ),
        (
            "w_symbol_setattr.py",
            "from smtplib import SMTP\nsetattr(SMTP, \"__init__\", None)\nSMTP(\"m\")\n",
        ),
        (
            "l_global.py",
            "import boto3\ndef swap():\n    global boto3\n    boto3 = None\nboto3.client(\"s3\")\n",
        ),
    ]);
    assert!(edges.is_empty(), "{edges:?}");
    assert!(nodes.is_empty(), "{nodes:?}");
}

// AC-0318: a runtime-computed identity is a Gap naming the site; a client
// called without its identity still proves the provider. (T-0318)
#[test]
fn computed_identities_are_gaps_and_absent_ones_name_the_provider() {
    let (nodes, edges) = extract(&[(
        "svc.py",
        r#"
import os
import redis
from sqlalchemy import create_engine
from settings import DB

DB = "postgresql://db.internal:5432"

def connect(url):
    return create_engine(url)

local = redis.Redis()
dsn = create_engine("postgresql://admin:pw@" + os.environ["DB_HOST"])
shadowed = create_engine(DB)
"#,
    )]);
    let gaps: Vec<&Node> = nodes.iter().filter(|n| n.label == "Gap").collect();
    // An imported name assigned again is ambiguous at the call: a Gap.
    assert_eq!(gaps.len(), 3, "{gaps:?}");
    for gap in &gaps {
        assert_eq!(gap.props["provider"], "sql-database");
        assert_eq!(
            gap.props["reason"],
            "runtime-computed sql-database identity"
        );
        assert!(!gap.props.to_string().contains("pw@"), "{gap:?}");
    }
    assert!(nodes.iter().any(|n| n.id == res("redis")));
    // AC-0376: each Gap's site still proves its provider (#1116). Its second
    // `USES` edge reaches the provider resource and names the Gap.
    let sql = nodes.iter().find(|n| n.id == res("sql-database")).unwrap();
    assert_eq!(sql.props["instance_unresolved"], true, "{sql:?}");
    assert!(!sql.props.to_string().contains("pw@"), "{sql:?}");
    // The identity-less redis client proves its resource without a marker.
    let redis = nodes.iter().find(|n| n.id == res("redis")).unwrap();
    assert!(
        redis.props.get("instance_unresolved").is_none(),
        "{redis:?}"
    );
    let to_sql: Vec<&Edge> = edges
        .iter()
        .filter(|e| e.dst == res("sql-database"))
        .collect();
    assert_eq!(to_sql.len(), 3);
    for edge in &to_sql {
        assert_eq!(edge.props["instance_unresolved"], true);
        assert!(gaps.iter().any(|g| g.id == edge.props["gap"]), "{edge:?}");
    }
    // The fixture's `os.environ["DB_HOST"]` is also a configuration read
    // (#1789): its `READS_CONFIG` edge is counted apart from the client edges.
    assert_eq!(edges.iter().filter(|e| e.label == "USES").count(), 7);
    assert_eq!(
        edges.iter().filter(|e| e.label == "READS_CONFIG").count(),
        1
    );
}

// AC-0318: Redis reached through the `limits` rate-limit storage is the same
// `redis` resource, under either import form and with its URI passed by
// position or as `uri=`; a URL passed in is a Gap naming the site, and any
// other keyword names no instance. (T-0318)
#[test]
fn limits_redis_storage_is_observed_redis() {
    let (nodes, edges) = extract(&[
        (
            "app/redis_services.py",
            r#"
import limits.storage
from limits.storage import RedisSentinelStorage, RedisClusterStorage

def initialize(redis_url):
    return limits.storage.RedisStorage(redis_url)

def sentinel():
    return RedisSentinelStorage("redis+sentinel://sentinel.internal:26379/main")

def cluster():
    return RedisClusterStorage("redis+cluster://user:pw@cluster.internal:7000")

def keyed():
    return limits.storage.RedisStorage(uri="redis://keyed.internal:6379/0")

def other_keyword():
    return limits.storage.RedisStorage(options={"uri": "redis://ignored.internal"})

memory = limits.storage.MemoryStorage()
"#,
        ),
        (
            "app/keyed.py",
            "import os\nimport smtplib\nfrom sqlalchemy import create_engine\nsmtplib.SMTP(host=\"mail.keyed\")\ncreate_engine(url=os.environ[\"KEYED_DB\"])\n",
        ),
    ]);
    // The keyword spelling holds for every Python first-argument client.
    assert_eq!(
        uses(&edges, "keyed.py"),
        vec![res("smtp:mail.keyed"), res("sql-database:env:KEYED_DB")]
    );
    assert_eq!(
        uses(&edges, "redis_services.py"),
        vec![
            format!("gap:observed:{REPO}@app/redis_services.py@131"),
            res("redis"),
            // AC-0376: the Gap's site also proves the provider.
            res("redis"),
            res("redis:redis+cluster://cluster.internal:7000"),
            res("redis:redis+sentinel://sentinel.internal:26379"),
            res("redis:redis://keyed.internal:6379"),
        ]
    );
    let gaps: Vec<&Node> = nodes.iter().filter(|n| n.label == "Gap").collect();
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert_eq!(gaps[0].props["reason"], "runtime-computed redis identity");
    assert!(!serde_json::to_string(&nodes).unwrap().contains("pw@"));
}

const CONFIG: &str = "import os\nDB_URI = os.environ[\"DB_URI\"]\n";
const DB: &str = "from sqlalchemy import create_engine\nfrom app import config\nengine = create_engine(config.DB_URI)\n";

// AC-0331: another repository module's constant is followed one step
// through a proven import (`config.X`, `from m import X`, `import a.b` then
// `a.b.X`), and the resource and `USES` edge cite the assignment it rests
// on beside the call. (T-0331)
#[test]
fn module_constants_are_followed_into_their_module() {
    let (nodes, edges) = extract(&[
        ("app/__init__.py", ""),
        (
            "app/config.py",
            "import os\nDB_URI = os.environ[\"DB_URI\"]\nREDIS_URL = \"redis://cache.internal:6379/0\"\nSMTP_HOST = os.getenv(\"SMTP_HOST\")\n",
        ),
        ("app/db.py", DB),
        (
            "app/cache.py",
            "import redis\nfrom app.config import REDIS_URL\nclient = redis.Redis.from_url(REDIS_URL)\n",
        ),
        (
            "app/mail.py",
            "import smtplib\nimport app.config\ns = smtplib.SMTP(app.config.SMTP_HOST)\n",
        ),
    ]);
    assert_eq!(uses(&edges, "db.py"), vec![res("sql-database:env:DB_URI")]);
    assert_eq!(
        uses(&edges, "cache.py"),
        vec![res("redis:redis://cache.internal:6379")]
    );
    assert_eq!(uses(&edges, "mail.py"), vec![res("smtp:env:SMTP_HOST")]);
    assert!(nodes.iter().all(|node| node.label != "Gap"));
    let edge = edges.iter().find(|e| e.src.contains("db.py")).unwrap();
    assert_eq!(edge.props["identity_from"], "app/config.py");
    assert_eq!(edge.props["prov"]["confidence_tier"], "Confirmed");
    assert!(
        edge.props
            .get("identity_assumes_method_receivers")
            .is_none()
    );
    let resource = nodes
        .iter()
        .find(|n| n.id == res("sql-database:env:DB_URI"))
        .unwrap();
    for prov in [&edge.props["prov"], &resource.props["prov"]] {
        let evidence = prov["evidence"].as_array().unwrap();
        assert_eq!(evidence.len(), 2);
        assert_eq!(evidence[0]["path"], "app/db.py");
        assert_eq!(evidence[1]["path"], "app/config.py");
        let (start, end) = (
            evidence[1]["byte_start"].as_u64().unwrap() as usize,
            evidence[1]["byte_end"].as_u64().unwrap() as usize,
        );
        assert_eq!(&CONFIG[start..end], "DB_URI = os.environ[\"DB_URI\"]");
    }
}

// AC-0331: the follow fails closed: the site stays a Gap when the module
// is not one repository file, a package on the way binds the name, the
// module writes, imports or star-imports it, assigns it other than once at
// module level or to a computed value, or any file writes that member
// through a proven binding. An unrelated member write does not. (T-0331)
#[test]
fn module_constants_fail_closed() {
    let cases: [(&str, &[(&str, &str)]); 40] = [
        (
            "patch.object by keyword",
            &[(
                "tests/test_db.py",
                "from unittest import mock\nfrom app import config\nmock.patch.object(target=config, attribute=\"DB_URI\", new=\"sqlite://\")\n",
            )],
        ),
        (
            "patch target by keyword",
            &[(
                "tests/test_db.py",
                "from unittest.mock import patch\npatch(target=\"app.config.DB_URI\", new=\"sqlite://\")\n",
            )],
        ),
        (
            "monkeypatch name by keyword",
            &[(
                "tests/test_db.py",
                "from app import config\ndef test(monkeypatch):\n    monkeypatch.setattr(config, name=\"DB_URI\", value=\"sqlite://\")\n",
            )],
        ),
        (
            "splatted patch operands",
            &[(
                "tests/test_db.py",
                "from unittest import mock\nmock.patch.object(**operands)\n",
            )],
        ),
        (
            "computed setattr beneath an import",
            &[(
                "tools/patch.py",
                "from app import holder\nsetattr(holder.target, key, None)\n",
            )],
        ),
        (
            "static method called with the module",
            &[(
                "tools/patch.py",
                "from app import config\nclass Patcher:\n    @staticmethod\n    def apply(module, key):\n        setattr(module, key, \"sqlite://\")\nPatcher.apply(config, \"DB_URI\")\n",
            )],
        ),
        (
            "unknown decorator",
            &[(
                "tools/patch.py",
                "class Patcher:\n    @register\n    def apply(module, key):\n        setattr(module, key, None)\n",
            )],
        ),
        (
            "chained alias",
            &[(
                "tools/patch.py",
                "from types import SimpleNamespace\nfrom app import config\nholder = SimpleNamespace(target=config)\nsetattr(holder.target, key, \"sqlite://\")\n",
            )],
        ),
        (
            "chained parameter",
            &[(
                "tools/patch.py",
                "def patch(holder, key):\n    setattr(holder.target, key, None)\n",
            )],
        ),
        (
            "chained literal write",
            &[(
                "tools/patch.py",
                "def patch(holder):\n    holder.target.DB_URI = None\n",
            )],
        ),
        (
            "literal write beneath an imported module",
            &[(
                "tools/patch.py",
                "import app.holder\napp.holder.cfg.DB_URI = None\n",
            )],
        ),
        (
            "computed setattr on a name also bound otherwise",
            &[(
                "tests/utils.py",
                "def reset(g):\n    setattr(g, key, False)\ndef other():\n    from flask import g\n",
            )],
        ),
        (
            "computed setattr through an alias",
            &[(
                "app/db.py",
                "from sqlalchemy import create_engine\nfrom app import config\nalias = config\nkey = \"DB_URI\"\nsetattr(alias, key, \"sqlite://\")\nengine = create_engine(config.DB_URI)\n",
            )],
        ),
        (
            "computed setattr through a parameter",
            &[(
                "tools/patch.py",
                "def patch(cfg, key, value):\n    setattr(cfg, key, value)\n",
            )],
        ),
        (
            "computed setattr on a call",
            &[(
                "tools/patch.py",
                "import importlib\nsetattr(importlib.import_module(name), key, value)\n",
            )],
        ),
        (
            "globals in the module",
            &[(
                "app/config.py",
                "import os\nDB_URI = os.environ[\"DB_URI\"]\nfor key in OVERRIDES:\n    globals()[key] = OVERRIDES[key]\n",
            )],
        ),
        (
            "vars of the module",
            &[(
                "tools/patch.py",
                "from app import config\nvars(config)[key] = value\n",
            )],
        ),
        (
            "dict of a parameter",
            &[(
                "tools/patch.py",
                "def patch(module, key):\n    module.__dict__[key] = None\n",
            )],
        ),
        (
            "mock patch.object",
            &[(
                "tests/test_db.py",
                "from unittest import mock\nfrom app import config\n@mock.patch.object(config, \"DB_URI\", \"x\")\ndef test():\n    pass\n",
            )],
        ),
        (
            "binding replaced through a parameter",
            &[(
                "tools/swap.py",
                "def swap(module):\n    module.config = None\n",
            )],
        ),
        (
            "patched through an alias",
            &[(
                "tools/patch.py",
                "from app import config\nc = config\nc.DB_URI = None\n",
            )],
        ),
        (
            "patched through a parameter",
            &[("tools/patch.py", "def patch(cfg):\n    cfg.DB_URI = None\n")],
        ),
        (
            "monkeypatch by object",
            &[(
                "tests/test_db.py",
                "from app import config\ndef test(monkeypatch):\n    monkeypatch.setattr(config, \"DB_URI\", \"sqlite://\")\n",
            )],
        ),
        (
            "monkeypatch by name",
            &[(
                "tests/test_db.py",
                "def test(monkeypatch):\n    monkeypatch.setattr(\"app.config.DB_URI\", \"sqlite://\")\n",
            )],
        ),
        (
            "mock patch",
            &[(
                "tests/test_db.py",
                "from unittest.mock import patch\n@patch(\"app.config.DB_URI\", \"x\")\ndef test():\n    pass\n",
            )],
        ),
        (
            "reading module's binding patched",
            &[(
                "tests/test_db.py",
                "def test(monkeypatch):\n    monkeypatch.setattr(\"app.db.config\", object())\n",
            )],
        ),
        (
            "assigned twice",
            &[(
                "app/config.py",
                "import os\nDB_URI = \"postgres://a/b\"\nDB_URI = os.environ[\"X\"]\n",
            )],
        ),
        (
            "conditional",
            &[(
                "app/config.py",
                "import os\nif os.environ:\n    DB_URI = \"postgres://a/b\"\n",
            )],
        ),
        ("computed", &[("app/config.py", "DB_URI = build()\n")]),
        (
            "chained",
            &[(
                "app/config.py",
                "import os\nBASE = os.environ[\"DB_URI\"]\nDB_URI = BASE\n",
            )],
        ),
        (
            "star import",
            &[(
                "app/config.py",
                "from defaults import *\nDB_URI = \"postgres://h/d\"\n",
            )],
        ),
        (
            "imported",
            &[(
                "app/config.py",
                "import os\nDB_URI = os.environ[\"DB_URI\"]\nfrom other import DB_URI\n",
            )],
        ),
        (
            "global write",
            &[(
                "app/config.py",
                "import os\nDB_URI = os.environ[\"DB_URI\"]\ndef reset():\n    global DB_URI\n    DB_URI = None\n",
            )],
        ),
        (
            "patched elsewhere",
            &[(
                "tests/conftest.py",
                "from app import config\nconfig.DB_URI = \"sqlite://\"\n",
            )],
        ),
        (
            "setattr elsewhere",
            &[(
                "tools/patch.py",
                "import app.config\nsetattr(app.config, name, value)\n",
            )],
        ),
        (
            "package attribute",
            &[("app/__init__.py", "config = object()\n")],
        ),
        (
            "ambiguous module",
            &[("app/config/__init__.py", "DB_URI = \"postgres://h/d\"\n")],
        ),
        (
            "missing module",
            &[(
                "app/db.py",
                "from sqlalchemy import create_engine\nfrom settings import config\nengine = create_engine(config.DB_URI)\n",
            )],
        ),
        (
            "rebound locally",
            &[(
                "app/db.py",
                "from sqlalchemy import create_engine\nfrom app import config\nconfig = None\nengine = create_engine(config.DB_URI)\n",
            )],
        ),
        (
            "patched at the call",
            &[(
                "app/db.py",
                "from sqlalchemy import create_engine\nfrom app import config\nconfig.DB_URI = \"sqlite://\"\nengine = create_engine(config.DB_URI)\n",
            )],
        ),
    ];
    for (label, overrides) in cases {
        let mut files: BTreeMap<&str, &str> = [
            ("app/__init__.py", ""),
            ("app/config.py", CONFIG),
            ("app/db.py", DB),
        ]
        .into_iter()
        .collect();
        files.extend(overrides.iter().copied());
        let files: Vec<(&str, &str)> = files.into_iter().collect();
        let (nodes, edges) = extract(&files);
        let found = uses(&edges, "app/db.py");
        // AC-0376: the Gap, plus the provider the client call proves.
        assert!(
            found.len() == 2
                && found[0].starts_with("gap:observed")
                && found[1] == res("sql-database"),
            "{label}: {found:?}"
        );
        // The Gap keeps the call's own expression, not the module's.
        let gap = nodes.iter().find(|n| n.id == found[0]).unwrap();
        assert_eq!(gap.props["raw"], "config.DB_URI", "{label}");
    }
    // Writes of other members, of another module's `config` binding, or of
    // a computed member of an unproven root leave the follow standing.
    let (nodes, edges) = extract(&[
        ("app/__init__.py", ""),
        ("app/config.py", CONFIG),
        ("app/db.py", DB),
        (
            "tests/conftest.py",
            "from app import config\nconfig.OTHER = 1\ndef test(monkeypatch):\n    monkeypatch.setattr(config, \"SESSION\", True)\n    monkeypatch.setattr(\"app.abuser.config\", object())\n",
        ),
        (
            "app/model.py",
            "class M:\n    def __init__(self, **kw):\n        for k, v in kw.items():\n            setattr(self, k, v)\n    @classmethod\n    def make(cls, k):\n        setattr(cls, k, None)\n\nglobals()[NAME] = 1\nresult = {}\nresult[key] = 1\n",
        ),
        (
            "tests/utils.py",
            "def reset():\n    from flask import g\n    setattr(g, \"%s_done\" % prefix, False)\n",
        ),
    ]);
    assert_eq!(
        uses(&edges, "app/db.py"),
        vec![res("sql-database:env:DB_URI")]
    );
    // The receiver writes (`setattr(self, …)`, `setattr(cls, …)`) are only
    // conventionally harmless: the follow is inferred and says so.
    let edge = edges.iter().find(|e| e.src.contains("app/db.py")).unwrap();
    assert_eq!(edge.props["prov"]["confidence_tier"], "InferredStrong");
    assert_eq!(edge.props["identity_assumes_method_receivers"], true);
    let resource = nodes
        .iter()
        .find(|n| n.id == res("sql-database:env:DB_URI"))
        .unwrap();
    assert_eq!(resource.props["prov"]["confidence_tier"], "InferredStrong");
    assert_eq!(resource.props["identity_assumes_method_receivers"], true);

    // A later site that proves the same resource without the assumption
    // makes it Confirmed and unmarked.
    let (nodes, _) = extract(&[
        ("app/__init__.py", ""),
        ("app/config.py", CONFIG),
        ("app/db.py", DB),
        (
            "tests/conftest.py",
            "from app import config\nconfig.OTHER = 1\ndef test(monkeypatch):\n    monkeypatch.setattr(config, \"SESSION\", True)\n    monkeypatch.setattr(\"app.abuser.config\", object())\n",
        ),
        (
            "app/model.py",
            "class M:\n    def __init__(self, **kw):\n        for k, v in kw.items():\n            setattr(self, k, v)\n    @classmethod\n    def make(cls, k):\n        setattr(cls, k, None)\n\nglobals()[NAME] = 1\nresult = {}\nresult[key] = 1\n",
        ),
        (
            "tests/utils.py",
            "def reset():\n    from flask import g\n    setattr(g, \"%s_done\" % prefix, False)\n",
        ),
        (
            "app/db2.py",
            "import os\nfrom sqlalchemy import create_engine\nengine = create_engine(os.environ[\"DB_URI\"])\n",
        ),
    ]);
    let resource = nodes
        .iter()
        .find(|n| n.id == res("sql-database:env:DB_URI"))
        .unwrap();
    assert_eq!(resource.props["prov"]["confidence_tier"], "Confirmed");
    assert!(
        resource
            .props
            .get("identity_assumes_method_receivers")
            .is_none()
    );
}

const SERVICES: &str = r#"
import limits.storage
import smtplib

def initialize_redis_services(app, redis_url: str):
    if redis_url.startswith("redis://"):
        return limits.storage.RedisStorage(redis_url)

def mailer(host):
    return smtplib.SMTP(host)
"#;
const SERVER: &str = r#"
from app import config
from app.services import initialize_redis_services, mailer

def create_app(app):
    initialize_redis_services(app, config.MEM_STORE_URI)
    mailer(host="mail.internal")
"#;

fn with_services(overrides: &[(&str, &str)]) -> (Vec<Node>, Vec<Edge>) {
    let mut files: BTreeMap<&str, &str> = [
        ("app/__init__.py", ""),
        (
            "app/config.py",
            "import os\nMEM_STORE_URI = os.environ.get(\"MEM_STORE_URI\", None)\n",
        ),
        ("app/services.py", SERVICES),
        ("server.py", SERVER),
    ]
    .into_iter()
    .collect();
    files.extend(overrides.iter().copied());
    extract(&files.into_iter().collect::<Vec<_>>())
}

// AC-0337: a parameter of a module-level function is followed to the
// argument of its one caller, positional or keyword, and on into a
// `config.X` constant; the result rests on static callers. (T-0337)
#[test]
fn parameters_are_followed_to_their_one_caller() {
    let (nodes, edges) = with_services(&[]);
    assert_eq!(
        uses(&edges, "services.py"),
        vec![res("redis:env:MEM_STORE_URI"), res("smtp:mail.internal")]
    );
    let redis = nodes
        .iter()
        .find(|n| n.id == res("redis:env:MEM_STORE_URI"))
        .unwrap();
    assert_eq!(redis.props["identity_assumes_static_callers"], true);
    assert_eq!(redis.props["prov"]["confidence_tier"], "InferredStrong");
    assert!(
        redis
            .props
            .get("identity_assumes_method_receivers")
            .is_none()
    );
    let edge = edges
        .iter()
        .find(|e| e.dst == res("redis:env:MEM_STORE_URI"))
        .unwrap();
    assert_eq!(edge.props["identity_from"], "app/config.py");
    let smtp = edges
        .iter()
        .find(|e| e.dst == res("smtp:mail.internal"))
        .unwrap();
    assert_eq!(smtp.props["identity_from"], "server.py");
    // A call through an imported module's member is the one caller too.
    let module_call = "from app import config\nimport app.services as svc\n\ndef create_app(app):\n    svc.initialize_redis_services(app, config.MEM_STORE_URI)\n";
    let dotted_call = "from app import config\nimport app.services\n\ndef create_app(server):\n    app.services.initialize_redis_services(server, config.MEM_STORE_URI)\n";
    let (_, edges) = with_services(&[("server.py", dotted_call)]);
    assert!(uses(&edges, "services.py").contains(&res("redis:env:MEM_STORE_URI").as_str()));
    let (_, edges) = with_services(&[("server.py", module_call)]);
    assert!(uses(&edges, "services.py").contains(&res("redis:env:MEM_STORE_URI").as_str()));

    // Each way another caller may hide, or the parameter may not be what
    // the caller passed, leaves the Redis site a Gap.
    let gap = |overrides: &[(&str, &str)]| {
        let (_, edges) = with_services(overrides);
        let found = uses(&edges, "services.py");
        found.iter().any(|dst| dst.starts_with("gap:"))
            && !found.contains(&res("redis:env:MEM_STORE_URI").as_str())
    };
    let server = |extra: &str| format!("{SERVER}{extra}");
    let services = |from: &str, to: &str| SERVICES.replacen(from, to, 1);
    for (case, overrides) in [
        ("second caller", vec![("server.py", server("\ninitialize_redis_services(None, \"redis://x\")\n"))]),
        ("dotted module read as a value", vec![("app/other.py", "import app.services\nalias = app.services\nalias.initialize_redis_services(None, \"redis://other\")\n".to_string())]),
        ("module read as a value", vec![("app/other.py", "import app.services as svc\nalias = svc\nalias.initialize_redis_services(None, \"redis://other\")\n".to_string())]),
        ("parameter rebound by a local import", vec![("app/services.py", services("    if redis_url", "    from other_config import REDIS_URI as redis_url\n    if redis_url"))]),
        ("read as a value", vec![("server.py", server("\nhooks = [initialize_redis_services]\n"))]),
        ("attribute of it", vec![("server.py", server("\nname = initialize_redis_services.__name__\n"))]),
        ("re-exported", vec![("app/__init__.py", "from app.services import initialize_redis_services\n".to_string())]),
        ("relative import", vec![("app/other.py", "from .services import initialize_redis_services\n".to_string())]),
        ("aliased relative import", vec![("app/other.py", "from .services import initialize_redis_services as init\ninit(None, None)\n".to_string())]),
        ("aliased relative module", vec![("app/other.py", "from . import services as svc\nsvc.initialize_redis_services(None, None)\n".to_string())]),
        ("local import", vec![("app/other.py", "def go():\n    from app.services import initialize_redis_services\n    initialize_redis_services(None, None)\n".to_string())]),
        ("wildcard import", vec![("app/other.py", "from app.services import *\n".to_string())]),
        ("splat call", vec![("server.py", SERVER.replacen("(app, config.MEM_STORE_URI)", "(*args)", 1))]),
        ("named in its module", vec![("app/services.py", format!("{SERVICES}\nalias = initialize_redis_services\n"))]),
        ("rebound in body", vec![("app/services.py", services("    if redis_url", "    redis_url = redis_url.strip()\n    if redis_url"))]),
        ("default value", vec![("app/services.py", services("redis_url: str)", "redis_url: str = \"\")"))]),
        ("nested function", vec![("app/services.py", services("def initialize_redis_services(app, redis_url: str):\n    if redis_url.startswith(\"redis://\"):\n        return", "def initialize_redis_services(app, redis_url: str):\n    def inner():\n        return"))]),
        ("written via a holder", vec![("app/other.py", "def patch(holder):\n    holder.initialize_redis_services = print\n".to_string())]),
        ("computed write in a chain", vec![("app/other.py", "def patch(holder):\n    setattr(holder.services, name, print)\n".to_string())]),
        ("namespace write", vec![("app/services.py", format!("{SERVICES}\nglobals()[name] = print\n"))]),
        ("trailing splat", vec![("server.py", SERVER.replacen("(app, config.MEM_STORE_URI)", "(app, config.MEM_STORE_URI, *rest)", 1))]),
        ("positional and keyword", vec![("server.py", SERVER.replacen("(app, config.MEM_STORE_URI)", "(app, config.MEM_STORE_URI, redis_url=\"redis://x\")", 1))]),
        ("member write", vec![("app/other.py", "import app.services\napp.services.initialize_redis_services = print\n".to_string())]),
    ] {
        let overrides: Vec<(&str, &str)> = overrides.iter().map(|(p, b)| (*p, b.as_str())).collect();
        assert!(gap(&overrides), "{case}");
    }
    // A receiver write could rebind the function or the constant: both
    // assumptions are marked.
    let (nodes, _) = with_services(&[(
        "app/model.py",
        "class M:\n    def __init__(self, k, v):\n        setattr(self, k, v)\n",
    )]);
    let redis = nodes
        .iter()
        .find(|n| n.id == res("redis:env:MEM_STORE_URI"))
        .unwrap();
    assert_eq!(redis.props["identity_assumes_static_callers"], true);
    assert_eq!(redis.props["identity_assumes_method_receivers"], true);
    // A literal argument needs no constant follow, so the member-write
    // refusal is the parameter step's own.
    let (_, edges) = with_services(&[(
        "app/other.py",
        "def patch(holder):\n    setattr(holder.services, name, print)\n",
    )]);
    assert!(!uses(&edges, "services.py").contains(&res("smtp:mail.internal").as_str()));
}

fn fetches(files: &[(&str, &str)]) -> Vec<FetchSite> {
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    extract_dir_with_sites(
        dir.path(),
        &SourceId {
            repo: REPO,
            commit: "workdir",
        },
        Registries::builtin(),
    )
    .unwrap()
    .fetch_sites
}

fn shown(site: &FetchSite) -> String {
    let url = match &site.url {
        IdentityExpr::Literal(url) => format!("lit:{url}"),
        IdentityExpr::EnvRef(key) => format!("env:{key}"),
        IdentityExpr::Computed(text) => format!("computed:{text}"),
    };
    format!(
        "{} {} {} {}",
        site.path,
        site.method,
        url,
        site.symbol.as_deref().unwrap_or("-")
    )
}

// AC-0424: module functions of an import-proven `requests` or `httpx` are
// fetch sites with their verb, a classified URL and the enclosing function
// as owner. (T-0424)
#[test]
fn proven_requests_and_httpx_calls_become_fetch_sites() {
    let sites = fetches(&[(
        "app/client.py",
        r#"
import os
import requests
import httpx as h
from requests import post as send

USERS = "https://users.internal/api/users"

def list_users():
    return requests.get(USERS, timeout=5)

def create(body):
    send("/api/users", json=body)
    h.request("patch", url="/api/users/1")
    h.request(verb(), "/api/users/2")
    h.request(method="put", url="/api/users/3")
    requests.delete(url=os.environ["USERS_URL"])
    requests.put(f"/api/users/{body.id}")

def literals():
    requests.get("/users'")
    requests.get("/a\n")
    h.request("GET'", "/x")
    requests.get(os.environ["URL'"])
    requests.get(os.getenv("BASE"))

class Sync:
    def run(self):
        h.head("/health")

requests.options("/api/ping")
"#,
    )]);
    let shown: Vec<String> = sites.iter().map(shown).collect();
    let owner = |name: &str| format!("sym:{REPO}@app/client.py#{name}");
    assert_eq!(
        shown,
        vec![
            format!(
                "app/client.py GET lit:https://users.internal/api/users {}",
                owner("list_users")
            ),
            format!("app/client.py POST lit:/api/users {}", owner("create")),
            format!("app/client.py PATCH lit:/api/users/1 {}", owner("create")),
            format!("app/client.py ? lit:/api/users/2 {}", owner("create")),
            format!("app/client.py PUT lit:/api/users/3 {}", owner("create")),
            format!("app/client.py DELETE env:USERS_URL {}", owner("create")),
            format!(
                "app/client.py PUT computed:f\"/api/users/{{body.id}}\" {}",
                owner("create")
            ),
            format!("app/client.py GET lit:/users' {}", owner("literals")),
            format!("app/client.py GET computed:\"/a\\n\" {}", owner("literals")),
            format!("app/client.py ? lit:/x {}", owner("literals")),
            format!("app/client.py GET env:URL' {}", owner("literals")),
            format!("app/client.py GET env:BASE {}", owner("literals")),
            format!("app/client.py HEAD lit:/health {}", owner("Sync.run")),
            "app/client.py OPTIONS lit:/api/ping -".to_string(),
        ]
    );
    assert!(
        sites
            .windows(2)
            .all(|pair| pair[0].byte_start < pair[1].byte_start)
    );
}

// AC-0424: only a never-rebound module-level import proves the client;
// lookalikes, rebound names, unproven receivers and other functions of the
// module yield nothing. (T-0424)
#[test]
fn unproven_http_clients_are_not_fetch_sites() {
    let sites = fetches(&[
        (
            "app/lookalike.py",
            r#"
import requestsx
from . import requests
import httpx

def f(session, client):
    requestsx.get("/a")
    requests.get("/b")
    session.get("/c")
    client.post("/d")
    httpx.Client().get("/e")
    httpx.stream("GET", "/f")
"#,
        ),
        (
            "app/rebound.py",
            r#"
import requests

def g():
    requests.get("/g")

requests = None
"#,
        ),
    ]);
    assert!(
        sites.is_empty(),
        "{:?}",
        sites.iter().map(shown).collect::<Vec<_>>()
    );
}

// AC-0424 (#1239 review): a repository module or package named like the
// client may be what the import resolves to, and a file that patches the
// call mocks it: neither yields fetch sites. (T-0424)
#[test]
fn shadowed_or_patched_http_clients_are_not_fetch_sites() {
    let shown_sites =
        |files: &[(&str, &str)]| -> Vec<String> { fetches(files).iter().map(shown).collect() };
    let calls = r#"
import requests
import httpx

def f():
    requests.get("/r")
    httpx.get("/h")
"#;
    // A local `httpx` package refuses httpx only; `requests` stays proven.
    assert_eq!(
        shown_sites(&[("app/calls.py", calls), ("vendor/httpx/__init__.py", "")]),
        vec![format!("app/calls.py GET lit:/r sym:{REPO}@app/calls.py#f")]
    );
    assert!(
        shown_sites(&[
            ("app/calls.py", calls),
            ("requests.py", ""),
            ("lib/httpx.py", "")
        ])
        .is_empty()
    );
    // Patching in a test file mocks its own calls, not production code.
    let test = r#"
from unittest.mock import patch
import requests
import httpx

@patch("requests.get")
def test_r(_):
    requests.get("/r")

def test_h(monkeypatch):
    monkeypatch.setattr("app.calls.httpx", None)
    httpx.get("/h")
"#;
    // Local aliases are patched by their own names; another client is not.
    let aliased = r#"
from unittest.mock import patch
import requests as r
import httpx
from requests import get as fetch

@patch("app.client.r.get")
def test_alias(_):
    r.get("/a")

@patch("app.client.fetch")
def test_fetch(_):
    fetch("/b")
    httpx.get("/c")
"#;
    assert_eq!(
        shown_sites(&[("app/client.py", aliased)]),
        vec![format!(
            "app/client.py GET lit:/c sym:{REPO}@app/client.py#test_fetch"
        )]
    );
    assert_eq!(
        shown_sites(&[("app/calls.py", calls), ("tests/test_calls.py", test)]),
        vec![
            format!("app/calls.py GET lit:/r sym:{REPO}@app/calls.py#f"),
            format!("app/calls.py GET lit:/h sym:{REPO}@app/calls.py#f"),
        ]
    );
}

fn shown_base(site: &FetchSite) -> String {
    match &site.base {
        None => shown(site),
        Some(base) => {
            let url = match &base.url {
                IdentityExpr::Literal(url) => format!("lit:{url}"),
                IdentityExpr::EnvRef(key) => format!("env:{key}"),
                IdentityExpr::Computed(text) => format!("computed:{text}"),
            };
            format!("{} base={url} {:?}", shown(site), base.join)
        }
    }
}

// AC-0425: a name bound once to a proven `requests.Session()` or
// `httpx.Client()`/`AsyncClient()` makes its verb methods fetch sites where
// the binding is in scope; an httpx `base_url` is joined under. (T-0425)
#[test]
fn proven_client_receivers_become_fetch_sites() {
    let sites = fetches(&[(
        "app/svc.py",
        r#"
import os
import requests
import httpx
from requests import Session as S

API = os.getenv("API_URL")
shared = requests.Session()
shared.headers["X-Token"] = "t"

def pull():
    shared.get("/a")

def push(body):
    s = requests.Session()
    s.post("/b", json=body)
    s.request("delete", "/c")

async def fetch():
    async with httpx.AsyncClient(base_url="https://h.io/api") as c:
        await c.get("/d")

def env_base():
    with httpx.Client(base_url=API) as client:
        client.put("e")

def aliased_constructor():
    plain = S()
    plain.get("/f")
"#,
    )]);
    let owner = |name: &str| format!("sym:{REPO}@app/svc.py#{name}");
    assert_eq!(
        sites.iter().map(shown_base).collect::<Vec<_>>(),
        vec![
            format!("app/svc.py GET lit:/a {}", owner("pull")),
            format!("app/svc.py POST lit:/b {}", owner("push")),
            format!("app/svc.py DELETE lit:/c {}", owner("push")),
            format!(
                "app/svc.py GET lit:/d {} base=lit:https://h.io/api Merge",
                owner("fetch")
            ),
            format!(
                "app/svc.py PUT lit:e {} base=env:API_URL Merge",
                owner("env_base")
            ),
            format!("app/svc.py GET lit:/f {}", owner("aliased_constructor")),
        ]
    );
}

// AC-0425: receivers that are rebound, parameters, attributes, factory
// results, used outside their binding's function, bound in a class body,
// shadowed by a local module or mocked by a patch (also of a locally aliased
// constructor), built from a splat, read other than through a member (an
// alias may rewrite them), or whose base, verbs or transport the file
// rewrites yield nothing. (T-0425)
#[test]
fn unproven_client_receivers_are_not_fetch_sites() {
    let sites = fetches(&[
        (
            "app/alias.py",
            r#"
import httpx
from requests import Session as S
from unittest.mock import patch

c = httpx.Client(base_url="https://h/api")
alias = c
alias.base_url = "https://h/v2"
c.get("/aliased")

passed = httpx.Client()
passed.get("/passed")
print(passed)

@patch("app.alias.S")
def test_aliased_constructor(_):
    mocked = S()
    mocked.get("/mocked-alias")
"#,
        ),
        (
            "app/bad.py",
            r#"
import requests
import httpx
from unittest.mock import patch

twice = requests.Session()
twice = requests.Session()

def make():
    return httpx.Client()

class Holder:
    session = requests.Session()

    def run(self):
        self.session.get("/self")
        session.get("/class")

def f(param):
    param.get("/param")
    twice.get("/twice")
    made = make()
    made.get("/factory")
    local = requests.Session()
    def inner():
        local.get("/nested")

def g():
    local.get("/elsewhere")

@patch("requests.Session")
def test_mocked(_):
    mocked = requests.Session()
    mocked.get("/mocked")

configured = httpx.Client(**{"base_url": "https://elsewhere/api"})
configured.get("/kwargs")
positional = requests.Session(*())
positional.get("/args")

rebased = httpx.Client()
rebased.base_url = "https://elsewhere/api"
rebased.get("/rebased")

replaced = requests.Session()
replaced.get = lambda *a, **k: None
replaced.get("/replaced")

objected = requests.Session()

def test_object():
    with patch.object(objected, "get"):
        objected.get("/objected")

module_level = requests.Session()

def shadowing():
    module_level = object()
    module_level.get("/shadowed")

captured = requests.Session()

def parameter(captured):
    captured.get("/parameter")
"#,
        ),
    ]);
    assert!(
        sites.is_empty(),
        "{:?}",
        sites.iter().map(shown_base).collect::<Vec<_>>()
    );
}

// AC-0496: `aiohttp.request(...)` and a name bound once to a proven
// `aiohttp.ClientSession(...)` are fetch sites as AC-0424/AC-0425 read
// them; the session's `base_url` (positional or keyword) is resolved
// against as RFC 3986. (T-0496)
#[test]
fn proven_aiohttp_requests_become_fetch_sites() {
    let sites = fetches(&[(
        "app/aio.py",
        r#"
import aiohttp
from aiohttp import ClientSession as CS

async def module_call():
    async with aiohttp.request("get", "https://h/m") as r:
        return await r.text()

async def positional_base():
    async with aiohttp.ClientSession("https://h.io/api/") as s:
        async with s.get("users") as r:
            await r.json()
        await s.request("POST", "/root")

async def keyword_base():
    session = CS(base_url="https://k.io")
    await session.delete("/x")

async def no_base():
    async with aiohttp.ClientSession() as plain:
        await plain.patch("https://h/p")
"#,
    )]);
    let owner = |name: &str| format!("sym:{REPO}@app/aio.py#{name}");
    assert_eq!(
        sites.iter().map(shown_base).collect::<Vec<_>>(),
        vec![
            format!("app/aio.py GET lit:https://h/m {}", owner("module_call")),
            format!(
                "app/aio.py GET lit:users {} base=lit:https://h.io/api/ Resolve",
                owner("positional_base")
            ),
            format!(
                "app/aio.py POST lit:/root {} base=lit:https://h.io/api/ Resolve",
                owner("positional_base")
            ),
            format!(
                "app/aio.py DELETE lit:/x {} base=lit:https://k.io Resolve",
                owner("keyword_base")
            ),
            format!("app/aio.py PATCH lit:https://h/p {}", owner("no_base")),
        ]
    );
}

// AC-0496: aiohttp has no module verb functions, and splatted, rebound,
// patched or shadowed sessions yield nothing. (T-0496)
#[test]
fn unproven_aiohttp_requests_are_not_fetch_sites() {
    let sites = fetches(&[
        (
            "app/neg.py",
            r#"
import aiohttp

async def verbs():
    await aiohttp.get("https://h/none")

async def splat(opts):
    async with aiohttp.ClientSession(**opts) as s:
        await s.get("/splat")

async def rebound():
    t = aiohttp.ClientSession()
    t = aiohttp.ClientSession()
    await t.get("/twice")
"#,
        ),
        (
            "app/mocked.py",
            r#"
import aiohttp
from unittest.mock import patch

@patch("aiohttp.ClientSession.get")
async def test_mocked(_):
    s = aiohttp.ClientSession()
    await s.get("/mocked")
"#,
        ),
    ]);
    assert!(
        sites.is_empty(),
        "{:?}",
        sites.iter().map(shown).collect::<Vec<_>>()
    );
    let shadowed = fetches(&[
        ("aiohttp.py", "def request(method, url): ...\n"),
        (
            "app/svc.py",
            r#"
import aiohttp

async def call():
    await aiohttp.request("GET", "https://h/shadowed")
"#,
        ),
    ]);
    assert!(
        shadowed.is_empty(),
        "{:?}",
        shadowed.iter().map(shown).collect::<Vec<_>>()
    );
}

fn events(files: &[(&str, &str)]) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let observed = extract_dir_with_sites(
        dir.path(),
        &SourceId {
            repo: REPO,
            commit: "workdir",
        },
        Registries::builtin(),
    )
    .unwrap();
    observed
        .event_sites
        .iter()
        .map(|site| {
            let identity = match &site.identity {
                IdentityExpr::Literal(value) => format!("lit:{value}"),
                IdentityExpr::EnvRef(key) => format!("env:{key}"),
                IdentityExpr::Computed(text) => format!("computed:{text}"),
            };
            assert!(site.components.is_empty() && site.batch.is_none());
            let registries: Vec<&str> = site.registries.iter().map(|r| r.name.as_str()).collect();
            assert_eq!(registries, ["builtin:event-sdks-pypi.json"]);
            format!(
                "{} {} {:?} {} {}",
                site.path,
                site.kind,
                site.role,
                identity,
                site.symbol.as_deref().unwrap_or("-")
            )
        })
        .collect()
}

// AC-0458: boto3 SQS and SNS calls on a receiver bound once to a selected
// `boto3.client(…)`, or on the factory call inline, and kafka-python
// producer sends and consumer constructors are event sites: the identity is
// the named keyword or first positional argument, exact, followed within the
// file, and computed when absent. (T-0458)
#[test]
fn proven_boto3_and_kafka_python_calls_become_event_sites() {
    let sites = events(&[(
        "app/jobs.py",
        r#"
import os
import boto3
from kafka import KafkaConsumer, KafkaProducer as P

QUEUE = "https://sqs.eu-west-1.amazonaws.com/1/orders"
sqs = boto3.client("sqs")

def enqueue(body):
    sqs.send_message(QueueUrl=QUEUE, MessageBody=body)

def drain():
    sqs.receive_message(QueueUrl=os.environ["ORDERS_QUEUE"])

def notify():
    boto3.client("sns").publish(TopicArn="arn:aws:sns:eu-west-1:1:sent", Message="m")

def emit(event):
    producer = P(bootstrap_servers="k:9092")
    producer.send("orders", event)

def emit_named(event, topic):
    named = P()
    named.send(topic, event)

def unnamed(params):
    sqs.send_message(**params)

consumer = KafkaConsumer("orders", group_id="g")
"#,
    )]);
    let owner = |name: &str| format!("sym:{REPO}@app/jobs.py#{name}");
    assert_eq!(
        sites,
        vec![
            format!(
                "app/jobs.py sqs-queue Produces lit:https://sqs.eu-west-1.amazonaws.com/1/orders {}",
                owner("enqueue")
            ),
            format!(
                "app/jobs.py sqs-queue Consumes env:ORDERS_QUEUE {}",
                owner("drain")
            ),
            format!(
                "app/jobs.py sns-topic Produces lit:arn:aws:sns:eu-west-1:1:sent {}",
                owner("notify")
            ),
            format!(
                "app/jobs.py kafka-topic Produces lit:orders {}",
                owner("emit")
            ),
            format!(
                "app/jobs.py kafka-topic Produces computed:topic {}",
                owner("emit_named")
            ),
            format!(
                "app/jobs.py sqs-queue Produces computed:sqs.send_message(**params) {}",
                owner("unnamed")
            ),
            "app/jobs.py kafka-topic Consumes lit:orders -".to_string(),
        ]
    );
}

// AC-0458: a client of another service, an unselected or computed factory
// argument, a rebound, escaping, parameter or out-of-scope receiver, a local
// module shadowing the SDK, a patched factory or method, an unimported or
// wildcard-imported name, and npm-only shapes yield no event site. (T-0458)
#[test]
fn unproven_event_sdk_calls_are_not_event_sites() {
    let quiet = |files: &[(&str, &str)]| {
        let sites = events(files);
        assert!(sites.is_empty(), "{sites:?}");
    };
    quiet(&[(
        "app/a.py",
        r#"
import boto3
import kafka

s3 = boto3.client("s3")
named = boto3.client(service_name="sqs")
kind = "sqs"
computed = boto3.client(kind)
rebound = boto3.client("sqs")
rebound = None
alias = boto3.client("sqs")
other = alias

def f(client):
    s3.send_message(QueueUrl="q")
    named.send_message(QueueUrl="q")
    computed.send_message(QueueUrl="q")
    client.send_message(QueueUrl="q")
    other.send_message(QueueUrl="q")
    rebound.send_message(QueueUrl="q")
    alias.send_message(QueueUrl="q")
    boto3.resource("sqs").send_message(QueueUrl="q")
    boto3.client("sqs").sendMessage(QueueUrl="q")
    kafka.KafkaProducer().produce("t")
    kafka.producer().send("t")

def g():
    inner = boto3.client("sqs")

def h():
    inner.send_message(QueueUrl="q")
"#,
    )]);
    // Controls: each refused file below yields a site without its refusal.
    let inline = "import boto3\nboto3.client(\"sqs\").send_message(QueueUrl=\"q\")\n";
    assert_eq!(events(&[("app/b.py", inline)]).len(), 1);
    assert_eq!(
        events(&[(
            "app/d.py",
            "from kafka import KafkaConsumer\nKafkaConsumer(\"t\")\n"
        )])
        .len(),
        1
    );
    let kept = "import boto3\nkept = boto3.client(\"sqs\")\ndef f():\n    kept.send_message(QueueUrl=\"q\")\n";
    assert_eq!(events(&[("app/e.py", kept)]).len(), 1);
    quiet(&[
        (
            "app/b.py",
            "import boto3\nboto3.client(\"sqs\").send_message(QueueUrl=\"q\")\n",
        ),
        ("boto3.py", ""),
    ]);
    quiet(&[(
        "app/c.py",
        r#"
import boto3
from unittest.mock import patch

patch("boto3.client")
boto3.client("sqs").send_message(QueueUrl="q")
"#,
    )]);
    quiet(&[(
        "app/d.py",
        r#"
from kafka import *
from .kafka import KafkaConsumer
KafkaProducer().send("t")
KafkaConsumer("t")
"#,
    )]);
}

fn evidence_spans(value: &serde_json::Value) -> Vec<(u64, u64)> {
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

// AC-0597 (#1789, ADR-0059): one literal `os.getenv` read, one dynamic read
// and one shadowed look-alike (a local function named `getenv`) yield
// exactly one named ConfigRead, one dynamic ConfigRead and nothing for the
// look-alike. Only the key name and the span of the read are recorded,
// never the default value.
#[test]
fn os_env_reads_are_config_reads() {
    let source = r#"import os

def load(name):
    url = os.getenv("DATABASE_URL", "postgres://user:pw@db/app")
    return url, os.environ[name]

def getenv(key):
    return key

def fake():
    return getenv("SHADOWED")
"#;
    let files = [("app/settings.py", source)];
    let (nodes, edges) = extract(&files);
    let reads: Vec<&Node> = nodes.iter().filter(|n| n.label == "ConfigRead").collect();
    let ids: Vec<&str> = reads.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "cfgread:local/py@app/settings.py:env:DATABASE_URL",
            "cfgread:local/py@app/settings.py:env:?",
        ]
    );
    let named = reads[0];
    assert_eq!(named.props["source"], "env");
    assert_eq!(named.props["key"], "DATABASE_URL");
    assert_eq!(named.props["language"], "python");
    assert_eq!(named.props["prov"]["tier"], "Deterministic");
    assert_eq!(named.props["prov"]["confidence_tier"], "Confirmed");
    let read = r#"os.getenv("DATABASE_URL", "postgres://user:pw@db/app")"#;
    let at = source.find(read).unwrap() as u64;
    assert_eq!(evidence_spans(&named.props), [(at, at + read.len() as u64)]);
    let dynamic = reads[1];
    assert_eq!(dynamic.props["dynamic"], true);
    assert!(dynamic.props.get("key").is_none());
    let at = source.find("os.environ[name]").unwrap() as u64;
    assert_eq!(
        evidence_spans(&dynamic.props),
        [(at, at + "os.environ[name]".len() as u64)]
    );
    let read_edges: Vec<(&str, &str)> = edges
        .iter()
        .filter(|edge| edge.label == "READS_CONFIG")
        .map(|edge| (edge.src.as_str(), edge.dst.as_str()))
        .collect();
    assert_eq!(
        read_edges,
        [
            (
                "sym:local/py@app/settings.py#load",
                "cfgread:local/py@app/settings.py:env:DATABASE_URL"
            ),
            (
                "sym:local/py@app/settings.py#load",
                "cfgread:local/py@app/settings.py:env:?"
            ),
        ]
    );
    let all = serde_json::to_string(&nodes).unwrap() + &serde_json::to_string(&edges).unwrap();
    assert!(!all.contains("postgres://") && !all.contains("SHADOWED"));
    // Determinism: a second run yields the same facts.
    let (again_nodes, again_edges) = extract(&files);
    assert_eq!(
        serde_json::to_string(&nodes).unwrap(),
        serde_json::to_string(&again_nodes).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&edges).unwrap(),
        serde_json::to_string(&again_edges).unwrap()
    );
}

// AC-0597 (ADR-0059 decisions 1, 4, 6): every proven form of the read of
// one key in a file is one node citing each span in byte order; a read that
// is a client identity keeps its observed resource and is a ConfigRead too;
// a write, a rebound `os`, and a repository module named `os` are not reads.
// A file that writes `os.environ[...]` voids its own subscript reads.
#[test]
fn os_env_read_forms_and_refusals() {
    let reads = r#"import os
import smtplib
from os import environ, getenv

A = os.environ["SMTP_HOST"]
B = environ.get("SMTP_HOST")
C = getenv("SMTP_HOST")
mail = smtplib.SMTP(os.environ["SMTP_HOST"])
"#;
    let rebound = r#"import os
os = object()
D = os.getenv("REBOUND")
"#;
    // A write is no read, and a file that writes `os.environ` members has
    // rebound what `os.environ[...]` reads: those reads prove nothing.
    let written = r#"import os
os.environ["WRITTEN"] = "1"
del os.environ["GONE"]
E = os.environ["AFTER_WRITE"]
"#;
    let (nodes, edges) = extract(&[
        ("app/reads.py", reads),
        ("app/rebound.py", rebound),
        ("app/written.py", written),
    ]);
    let found: Vec<&Node> = nodes.iter().filter(|n| n.label == "ConfigRead").collect();
    assert_eq!(
        found.len(),
        1,
        "{:?}",
        found.iter().map(|n| &n.id).collect::<Vec<_>>()
    );
    assert_eq!(found[0].id, "cfgread:local/py@app/reads.py:env:SMTP_HOST");
    let starts: Vec<u64> = evidence_spans(&found[0].props)
        .iter()
        .map(|s| s.0)
        .collect();
    let expected = [
        reads.find(r#"os.environ["SMTP_HOST"]"#).unwrap() as u64,
        reads.find(r#"environ.get("SMTP_HOST")"#).unwrap() as u64,
        reads.find(r#"getenv("SMTP_HOST")"#).unwrap() as u64,
        reads.rfind(r#"os.environ["SMTP_HOST"]"#).unwrap() as u64,
    ];
    assert_eq!(starts, expected);
    let read_edges: Vec<&Edge> = edges.iter().filter(|e| e.label == "READS_CONFIG").collect();
    assert_eq!(read_edges.len(), 1);
    assert_eq!(read_edges[0].src, "file:local/py@app/reads.py");
    // The client identity is still the observed resource it was.
    let smtp = nodes
        .iter()
        .find(|n| n.id == res("smtp:env:SMTP_HOST"))
        .unwrap();
    assert_eq!(smtp.props["env_key"], "SMTP_HOST");

    // A repository package named `os` may be what `import os` binds.
    let (nodes, _) = extract(&[("app/reads.py", reads), ("os/__init__.py", "")]);
    assert!(!nodes.iter().any(|n| n.label == "ConfigRead"));
}

#[test]
fn numeric_defaults_of_environment_constants_are_recorded() {
    // AC-0903 (T-0903): a module constant bound to a proven environment read
    // records its numeric or boolean literal default on the read's node: a
    // `get` default, an `or` fallback, and a `try`/`except` fallback; a
    // string default, a rebound constant and a computed default record none.
    let (nodes, _) = extract(&[(
        "app/config.py",
        r#"import os

ALIAS_TRASH_DAYS = int(os.environ.get("ALIAS_TRASH_DAYS", 30))
RATIO = float(os.getenv("RATIO") or 0.5)
FLAG = os.environ.get("FLAG", True)
NAME = os.environ.get("NAME", "simplelogin")
REBOUND = int(os.environ.get("REBOUND", 3))
REBOUND = 4
COMPUTED = int(os.environ.get("COMPUTED", 2 * 3))

try:
    MAX_FREE = int(os.environ["MAX_FREE"])
except Exception:
    print("not set")
    MAX_FREE = 5
"#,
    )]);
    let defaults = |key: &str| {
        nodes
            .iter()
            .find(|n| n.id == format!("cfgread:local/py@app/config.py:env:{key}"))
            .unwrap_or_else(|| panic!("{key}"))
            .props
            .get("defaults")
            .map(|d| {
                d.as_array()
                    .unwrap()
                    .iter()
                    .map(|e| {
                        (
                            e["constant"].as_str().unwrap().to_owned(),
                            e["value"].clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
    };
    assert_eq!(
        defaults("ALIAS_TRASH_DAYS"),
        Some(vec![("ALIAS_TRASH_DAYS".into(), 30.into())])
    );
    assert_eq!(defaults("RATIO"), Some(vec![("RATIO".into(), 0.5.into())]));
    assert_eq!(defaults("FLAG"), Some(vec![("FLAG".into(), true.into())]));
    assert_eq!(
        defaults("MAX_FREE"),
        Some(vec![("MAX_FREE".into(), 5.into())])
    );
    for key in ["NAME", "REBOUND", "COMPUTED"] {
        assert_eq!(defaults(key), None, "{key}");
    }
}

// AC-1055: over supplied sources, every file and every followed module is
// read through the supplied bytes, never the checkout, and an identity is
// not followed into a module outside the supplied paths. (T-1055)
#[test]
fn supplied_sources_are_observed_instead_of_the_checkout() {
    let files: BTreeMap<&str, &str> = [
        ("app/__init__.py", ""),
        ("app/config.py", CONFIG),
        ("app/db.py", DB),
        (
            "app/cache.py",
            "import redis\nclient = redis.Redis.from_url(\"redis://cache.internal:6379/0\")\n",
        ),
    ]
    .into();
    let id = SourceId {
        repo: REPO,
        commit: "workdir",
    };
    let read = |path: &str| {
        files
            .get(path)
            .map(|body| body.as_bytes().to_vec())
            .ok_or_else(|| std::io::Error::other(format!("not supplied: {path}")))
    };
    let all: Vec<String> = files.keys().map(|path| path.to_string()).collect();
    let observed =
        extract_from_sources_with_sites(&all, &read, &id, Registries::builtin()).unwrap();
    assert_eq!(
        uses(&observed.edges, "db.py"),
        vec![res("sql-database:env:DB_URI")]
    );
    assert_eq!(
        uses(&observed.edges, "cache.py"),
        vec![res("redis:redis://cache.internal:6379")]
    );
    // Without `app/config.py` among the paths, the constant is not followed.
    let without: Vec<String> = all
        .iter()
        .filter(|path| *path != "app/config.py")
        .cloned()
        .collect();
    let observed =
        extract_from_sources_with_sites(&without, &read, &id, Registries::builtin()).unwrap();
    assert_ne!(
        uses(&observed.edges, "db.py"),
        vec![res("sql-database:env:DB_URI")]
    );
    assert_eq!(
        uses(&observed.edges, "cache.py"),
        vec![res("redis:redis://cache.internal:6379")]
    );
}
