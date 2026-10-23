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
    assert_eq!(edges.len(), 4);
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
        assert!(
            found.len() == 1 && found[0].starts_with("gap:observed"),
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
