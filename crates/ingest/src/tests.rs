//! Ingest tests (US-0001). Clones run offline against `file://` bare-repo
//! fixtures; the auth-failure path is unit-tested at the error-mapping
//! layer (a live 401 requires the network — MT-covered).

use super::*;

/// Build a bare repo with one commit containing `app.ts`; returns its
/// `file://` URL. Shells out to `git` (present in CI and dev machines).
fn bare_fixture(dir: &Path) -> String {
    let src = dir.join("src-repo");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("app.ts"), "export function run() {}\n").unwrap();
    let run = |args: &[&str], cwd: &Path| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out);
    };
    run(&["init", "-q", "-b", "main"], &src);
    run(&["add", "."], &src);
    run(&["commit", "-q", "-m", "init"], &src);
    let bare = dir.join("fixture.git");
    run(
        &[
            "clone",
            "-q",
            "--bare",
            src.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
        dir,
    );
    format!("file://{}", bare.display())
}

// AC-0001: a valid repo URL clones read-only and is listed with its
// commit SHA. (T-0001)
#[test]
fn clone_lists_repo_with_commit_sha() {
    let dir = tempfile::tempdir().unwrap();
    let url = bare_fixture(dir.path());
    let dest = dir.path().join("clones");
    std::fs::create_dir(&dest).unwrap();
    let dest = dunce::canonicalize(dest).unwrap();
    let origin = managed::parse_managed_origin(&url).unwrap();
    let checkout = managed::ManagedCheckout::new(
        &dest,
        "reg_11111111111111111111111111111111",
        "src_22222222222222222222222222222222",
    )
    .unwrap();
    let mut guard = checkout.try_write().unwrap();
    let cloned = guard.clone_from(&origin, None).unwrap();
    assert_eq!(cloned.repo, "local/src_22222222222222222222222222222222");
    assert_eq!(cloned.commit_sha.len(), 40, "full SHA");
    assert!(cloned.path.join("app.ts").exists());
    // Re-adding replaces, same identity/SHA (v1 one-shot ingest).
    let again = guard.clone_from(&origin, None).unwrap();
    assert_eq!(again.commit_sha, cloned.commit_sha);
}

// AC-0001: a clone fetches only the default branch, so a remote branch
// whose name nests past Windows' MAX_PATH (a dependabot branch on a deep
// manifest path) cannot fail the recovery with "path too long". The ref is
// written into packed-refs so the fixture itself never needs a long path.
#[test]
fn clone_fetches_only_the_default_branch() {
    let dir = tempfile::tempdir().unwrap();
    let url = bare_fixture(dir.path());
    let bare = dir.path().join("fixture.git");
    let source = git2::Repository::open_bare(&bare).unwrap();
    let head = source.refname_to_id("refs/heads/main").unwrap();
    // The deep branch points at its own commit, so cloning it by mistake
    // fails the SHA assertion instead of passing by accident.
    let other = {
        let parent = source.find_commit(head).unwrap();
        let signature = git2::Signature::now("t", "t@t").unwrap();
        source
            .commit(
                None,
                &signature,
                &signature,
                "other",
                &parent.tree().unwrap(),
                &[&parent],
            )
            .unwrap()
    };
    assert_ne!(other, head);
    // libgit2's local (file://) transport looks every advertised ref up by
    // name in the destination during negotiation (local_negotiate_fetch,
    // git_revparse_single, whose longest DWIM candidate is
    // refs/remotes/<name>/HEAD), so offline the name must fit under
    // MAX_PATH there (260, counting ".lock").
    // An https clone does no such lookup; there the failure was writing
    // refs/remotes/origin/<branch> for every branch, which the remote-ref
    // assertion below rules out. Size the name for that longest candidate,
    // with the longest attempt directory name.
    let attempt = ".attempt-4294967295-18446744073709551615000-999";
    let destination_prefix = dunce::canonicalize(dir.path())
        .unwrap()
        .join("clones/sources/src_22222222222222222222222222222222")
        .join(attempt)
        .join("checkout/.git/refs/remotes/refs/heads/")
        .to_str()
        .unwrap()
        .len()
        + "/HEAD.lock".len();
    let budget = 259 - destination_prefix;
    assert!(
        budget > 40,
        "temp path too long for the fixture: {destination_prefix}"
    );
    let mut deep = String::from("dependabot/npm_and_yarn");
    while deep.len() + "/nested-dir".len() <= budget {
        deep.push_str("/nested-dir");
    }
    let packed = bare.join("packed-refs");
    // Appending breaks the order a `sorted` header promises; drop the trait.
    let mut refs = std::fs::read_to_string(&packed)
        .unwrap_or_default()
        .replace(" sorted", "");
    refs.push_str(&format!("{other} refs/heads/{deep}\n"));
    // A branch sharing HEAD's commit and sorting before `main`: a default
    // branch guessed by commit (the local transport has no HEAD symref)
    // would pick it, and the remote-ref assertion below would catch that.
    refs.push_str(&format!("{head} refs/heads/a-same-commit\n"));
    std::fs::write(&packed, refs).unwrap();
    let dest = dir.path().join("clones");
    std::fs::create_dir(&dest).unwrap();
    let dest = dunce::canonicalize(dest).unwrap();
    let origin = managed::parse_managed_origin(&url).unwrap();
    let checkout = managed::ManagedCheckout::new(
        &dest,
        "reg_11111111111111111111111111111111",
        "src_22222222222222222222222222222222",
    )
    .unwrap();
    let mut guard = checkout.try_write().unwrap();
    let cloned = guard.clone_from(&origin, None).unwrap();
    assert_eq!(cloned.commit_sha, head.to_string());
    let repo = git2::Repository::open(&cloned.path).unwrap();
    let remotes: Vec<String> = repo
        .references_glob("refs/remotes/**")
        .unwrap()
        .map(|r| r.unwrap().name().unwrap().to_string())
        .collect();
    assert_eq!(
        remotes,
        ["refs/remotes/origin/HEAD", "refs/remotes/origin/main"],
        "only the default branch (and its HEAD alias) is fetched"
    );
    assert_eq!(repo.head().unwrap().shorthand().unwrap(), "main");
}

// AC-0003: a failed clone leaves no partial clone behind. (T-0003)
#[test]
fn failed_clone_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("clones");
    std::fs::create_dir(&dest).unwrap();
    let dest = dunce::canonicalize(dest).unwrap();
    let not_a_repo = dir.path().join("not-a-repository");
    std::fs::create_dir(&not_a_repo).unwrap();
    let url = url::Url::from_file_path(not_a_repo).unwrap();
    let origin = managed::parse_managed_origin(url.as_str()).unwrap();
    let checkout = managed::ManagedCheckout::new(
        &dest,
        "reg_11111111111111111111111111111111",
        "src_22222222222222222222222222222222",
    )
    .unwrap();
    let mut guard = checkout.try_write().unwrap();
    let err = guard.clone_from(&origin, None).unwrap_err();
    assert!(matches!(err, IngestError::Git(_)));
    assert!(!checkout.root().exists());
    let leftovers: Vec<_> = std::fs::read_dir(checkout.root().parent().unwrap())
        .map(|it| it.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert_eq!(
        leftovers.len(),
        1,
        "only persistent owner metadata: {leftovers:?}"
    );
    assert_eq!(leftovers[0].file_name(), "owner.json");
}

// AC-0003: auth-shaped git errors map to a typed failure with
// remediation the UI can show verbatim. (T-0003)
#[test]
fn auth_errors_carry_remediation() {
    let e = git2::Error::new(
        git2::ErrorCode::Auth,
        git2::ErrorClass::Http,
        "too many redirects or authentication replays",
    );
    let mapped = classify_git_error("https://github.com/acme/private", e, true);
    match mapped {
        IngestError::Auth { remediation, .. } => {
            assert!(remediation.contains("found but refused"));
            assert!(remediation.contains("GH_TOKEN"));
            assert!(remediation.contains("gh auth login"));
        }
        other => panic!("expected Auth, got {other:?}"),
    }
    // With no credential at all, it says so and names where `gh` was
    // looked for, rather than implying a refused login (#992).
    let e = git2::Error::new(
        git2::ErrorCode::Auth,
        git2::ErrorClass::Callback,
        "remote authentication required but no callback set",
    );
    match classify_git_error("https://github.com/acme/private", e, false) {
        IngestError::Auth { remediation, .. } => {
            assert!(remediation.starts_with("No GitHub credential was found"));
            assert!(remediation.contains("/opt/homebrew/bin/gh"));
        }
        other => panic!("expected Auth, got {other:?}"),
    }
    // Non-auth errors pass through untouched.
    let e = git2::Error::new(
        git2::ErrorCode::NotFound,
        git2::ErrorClass::Repository,
        "could not find repository",
    );
    assert!(matches!(
        classify_git_error("u", e, false),
        IngestError::Git(_)
    ));
}

// URL forms: https, ssh, shorthand, file — one identity scheme.
#[test]
fn repo_urls_parse_to_identities() {
    let cases = [
        ("https://github.com/acme/shop", "acme/shop"),
        ("https://github.com/acme/shop.git", "acme/shop"),
        ("git@github.com:acme/shop.git", "acme/shop"),
        ("acme/shop", "acme/shop"),
        ("file:///tmp/x/mirror.git", "local/mirror"),
    ];
    for (url, want) in cases {
        let (id, _) = parse_repo_url(url).unwrap();
        assert_eq!(id, want, "{url}");
    }
    for bad in [
        "https://gitlab.com/a/b",
        "not a url",
        "https://github.com/onlyowner",
    ] {
        assert!(parse_repo_url(bad).is_err(), "{bad}");
    }
}

// AC-0002: the topology manifest parses repos, layer hints, and known
// identities; unknown layers fail loudly. (T-0002)
#[test]
fn manifest_parses_repos_layers_and_env() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(manifest::MANIFEST_NAME),
        r#"
[[repos]]
url = "acme/shop"
layers = ["server", "client"]

[[repos]]
url = "./infra"
layers = ["infra"]
pulumi_json = "stacks/infra.json"
otel_jsonl = ["traces/infra.jsonl"]

[env]
ORDERS_QUEUE = "https://sqs.example/orders"
"#,
    )
    .unwrap();
    // Loading by directory finds the canonical file name.
    let m = manifest::SystemManifest::load(dir.path()).unwrap();
    assert_eq!(m.repos.len(), 2);
    assert_eq!(m.repos[0].url, "acme/shop");
    assert_eq!(m.repos[0].layers, ["server", "client"]);
    assert_eq!(m.repos[1].layers, ["infra"]);
    assert_eq!(m.repos[1].pulumi_json.as_deref(), Some("stacks/infra.json"));
    assert_eq!(m.repos[1].otel_jsonl, ["traces/infra.jsonl"]);
    assert_eq!(
        m.env.get("ORDERS_QUEUE").map(String::as_str),
        Some("https://sqs.example/orders")
    );
}

// AC-1018 (ADR-0075): `env_read` in the manifest and `read` in a
// repository's `.cartograph/env.toml` list exact env keys; a glob or other
// shape is refused, and no file is an empty allowlist.
#[test]
fn env_read_allowlists_are_exact_keys() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join(manifest::MANIFEST_NAME);
    std::fs::write(&f, "env_read = [\"QUEUE_URL\", \"SETTINGS.apiUrl\"]\n").unwrap();
    let m = manifest::SystemManifest::load(&f).unwrap();
    assert_eq!(
        m.env_read_keys().into_iter().collect::<Vec<_>>(),
        ["QUEUE_URL", "SETTINGS.apiUrl"]
    );
    std::fs::write(&f, "env_read = [\"QUEUE_*\"]\n").unwrap();
    assert!(matches!(
        manifest::SystemManifest::load(&f),
        Err(manifest::ManifestError::InvalidEnvReadKey(key)) if key == "QUEUE_*"
    ));

    let repo = tempfile::tempdir().unwrap();
    assert!(manifest::repo_env_read(repo.path()).unwrap().is_empty());
    std::fs::create_dir(repo.path().join(".cartograph")).unwrap();
    let env = repo.path().join(manifest::REPO_ENV_REL_PATH);
    std::fs::write(&env, "read = [\"ORDERS_QUEUE\"]\n").unwrap();
    assert_eq!(
        manifest::repo_env_read(repo.path())
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        ["ORDERS_QUEUE"]
    );
    std::fs::write(&env, "read = [\"1BAD\"]\n").unwrap();
    assert!(manifest::repo_env_read(repo.path()).is_err());
    std::fs::write(&env, "reads = [\"ORDERS_QUEUE\"]\n").unwrap();
    assert!(manifest::repo_env_read(repo.path()).is_err());

    // A symlink is refused, never followed out of the repository.
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("env.toml");
        std::fs::write(&target, "read = [\"ORDERS_QUEUE\"]\n").unwrap();
        std::fs::remove_file(&env).unwrap();
        std::os::unix::fs::symlink(&target, &env).unwrap();
        assert!(matches!(
            manifest::repo_env_read(repo.path()),
            Err(manifest::ManifestError::RepoEnvUnreadable(reason)) if reason.contains("regular file")
        ));
    }
}

// AC-0240 (#589): services and gateway routes are author-declared binding
// evidence; references and base URLs are validated at load.
#[test]
fn manifest_declares_services_and_gateway_routes() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join(manifest::MANIFEST_NAME);
    std::fs::write(
        &f,
        r#"
[[repos]]
url = "simple-login/app"

[[repos]]
url = "simple-login/browser-extension"

[[services]]
name = "api"
repo = "simple-login/app"
base_urls = ["https://app.simplelogin.io", "http://localhost:7777"]

[[gateway_routes]]
base_url = "https://gateway.example.com"
prefix = "/sl"
service = "api"
strip_prefix = true
"#,
    )
    .unwrap();
    let m = manifest::SystemManifest::load(&f).unwrap();
    assert_eq!(m.services[0].name, "api");
    assert_eq!(m.services[0].repo, "simple-login/app");
    assert_eq!(m.services[0].base_urls.len(), 2);
    assert_eq!(m.gateway_routes[0].prefix, "/sl");
    assert!(m.gateway_routes[0].strip_prefix);

    for (body, expected) in [
        (
            "[[repos]]\nurl = \"a/b\"\n[[services]]\nname = \"s\"\nrepo = \"x/y\"\n",
            "names repo \"x/y\"",
        ),
        (
            "[[repos]]\nurl = \"a/b\"\n[[services]]\nname = \"s\"\nrepo = \"a/b\"\nbase_urls = [\"app.example.com\"]\n",
            "base URL \"app.example.com\" must be absolute",
        ),
        (
            "[[repos]]\nurl = \"a/b\"\n[[gateway_routes]]\nbase_url = \"https://g.example\"\nprefix = \"/p\"\nservice = \"nope\"\n",
            "undeclared service \"nope\"",
        ),
        (
            // #606 review: declaration order must never choose a route's repo.
            "[[repos]]\nurl = \"a/b\"\n[[repos]]\nurl = \"c/d\"\n[[services]]\nname = \"api\"\nrepo = \"a/b\"\n[[services]]\nname = \"api\"\nrepo = \"c/d\"\n",
            "service \"api\" is declared more than once",
        ),
    ] {
        std::fs::write(&f, body).unwrap();
        let err = manifest::SystemManifest::load(&f).unwrap_err().to_string();
        assert!(err.contains(expected), "{err}");
    }
}

#[test]
fn manifest_rejects_unknown_layers() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join(manifest::MANIFEST_NAME);
    std::fs::write(&f, "[[repos]]\nurl = \"a/b\"\nlayers = [\"backend\"]\n").unwrap();
    let err = manifest::SystemManifest::load(&f).unwrap_err();
    assert!(err.to_string().contains("unknown layer \"backend\""));
}
