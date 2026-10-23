use super::*;

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
            "app/order.py",
            "def early():\n    import smtplib\n\nimport smtplib\nsmtplib.SMTP(\"mail.internal\")\n",
        ),
    ]);
    // A local import of the same module, before or after, keeps the proof.
    assert_eq!(uses(&edges, "order.py"), vec![res("smtp:mail.internal")]);
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
