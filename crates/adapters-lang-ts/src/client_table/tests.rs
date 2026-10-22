use super::*;

fn extract(files: &[(&str, &str)]) -> Vec<FetchSite> {
    let dir = tempfile::tempdir().unwrap();
    for (path, source) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, source).unwrap();
    }
    let id = SourceId {
        repo: "local/ext",
        commit: "abc123",
    };
    extract_dir(dir.path(), &id).unwrap()
}

/// (method, path, entry, base) per site, in output order.
fn summary(sites: &[FetchSite]) -> Vec<(String, String, String, IdentityExpr)> {
    sites
        .iter()
        .map(|site| {
            let IdentityExpr::Literal(path) = &site.url else {
                panic!("literal path: {site:?}")
            };
            let base = site.base.as_ref().expect("joined site");
            assert_eq!(base.join, BaseJoin::Concat);
            (
                site.method.clone(),
                path.clone(),
                site.entry.clone().unwrap(),
                base.url.clone(),
            )
        })
        .collect()
}

const SERVICE: &str = r#"
const API_ROUTE = {
  GET_USER_INFO: { method: "GET", path: "/api/user_info" },
  LOGIN: { method: "POST", path: "/api/auth/login" },
  NOT_A_ROUTE: { label: "x" },
};
const SETTINGS = { apiUrl: "" };
const callAPI = async function (route, params = {}) {
  const { method, path } = route;
  const url = SETTINGS.apiUrl + bindQueryParams(path, params);
  let fetchParam = { method: method, headers: {} };
  fetchParam.body = "{}";
  return fetch(url, fetchParam);
};
function bindQueryParams(url, params) { return url; }
export { callAPI, API_ROUTE };
"#;

// AC-0315: a wrapper call naming a literal route entry becomes a joined
// client call across files: the entry's method and path, the wrapper's
// runtime base, and the declared entry name. (T-0315)
#[test]
fn wrapper_calls_with_imported_route_entries_become_joined_sites() {
    let sites = extract(&[
        ("src/popup/APIService.js", SERVICE),
        (
            "src/popup/Login.js",
            concat!(
                "import { callAPI, API_ROUTE as R } from \"./APIService\";\n",
                "export async function login() { return callAPI(R.LOGIN, {}); }\n",
                "export async function missing() { return callAPI(R.NOT_A_ROUTE, {}); }\n",
            ),
        ),
    ]);
    assert_eq!(
        summary(&sites),
        [(
            "POST".to_string(),
            "/api/auth/login".to_string(),
            "API_ROUTE.LOGIN".to_string(),
            IdentityExpr::Computed("SETTINGS.apiUrl".into()),
        )]
    );
    let site = &sites[0];
    assert_eq!(site.path, "src/popup/Login.js");
    assert_eq!(site.base.as_ref().unwrap().path, "src/popup/APIService.js");
    assert!(site.symbol.as_deref().unwrap().ends_with("#login"));
}

// AC-0315: a direct join `fetch(base + T.K.path, …)` takes the option's
// literal method, or the entry's own through `T.K.method`, and a literal
// base stays literal. (T-0315)
#[test]
fn direct_joins_take_their_method_from_options_or_the_entry() {
    let sites = extract(&[
        ("src/APIService.js", SERVICE),
        (
            "src/background.js",
            concat!(
                "import { API_ROUTE } from './APIService.js';\n",
                "async function setup() {\n",
                "  const apiUrl = await storage.get('url');\n",
                "  const url = apiUrl + API_ROUTE.LOGIN.path;\n",
                "  return fetch(url, { method: 'post' });\n",
                "}\n",
                "function info() {\n",
                "  return fetch('https://app.example.io' + API_ROUTE.GET_USER_INFO.path, {\n",
                "    method: API_ROUTE.GET_USER_INFO.method,\n",
                "  });\n",
                "}\n",
                "function plain() { return fetch(BASE + API_ROUTE.GET_USER_INFO.path); }\n",
            ),
        ),
    ]);
    assert_eq!(
        summary(&sites),
        [
            (
                "POST".to_string(),
                "/api/auth/login".to_string(),
                "API_ROUTE.LOGIN".to_string(),
                IdentityExpr::Computed("apiUrl".into()),
            ),
            (
                "GET".to_string(),
                "/api/user_info".to_string(),
                "API_ROUTE.GET_USER_INFO".to_string(),
                IdentityExpr::Literal("https://app.example.io".into()),
            ),
            (
                "GET".to_string(),
                "/api/user_info".to_string(),
                "API_ROUTE.GET_USER_INFO".to_string(),
                IdentityExpr::Computed("BASE".into()),
            ),
        ]
    );
}

// AC-0315: unproven bindings assert nothing. A same-named table in another
// file that is not imported, a bare-package import, an unexported table, a
// shadowed `fetch` and a wrapper that does not join its route parameter all
// produce no client call. (T-0315)
#[test]
fn unproven_tables_wrappers_and_shadowed_fetch_produce_no_sites() {
    let sites = extract(&[
        ("src/APIService.js", SERVICE),
        (
            "src/private.js",
            "const API_ROUTE = { LOGIN: { method: 'POST', path: '/p' } };\n",
        ),
        (
            "src/a.js",
            concat!(
                "import { callAPI } from './APIService';\n",
                "import { API_ROUTE } from 'some-package';\n",
                "callAPI(API_ROUTE.LOGIN);\n",
            ),
        ),
        (
            "src/b.js",
            concat!(
                "import { callAPI } from './APIService';\n",
                "import { API_ROUTE } from './private';\n",
                "callAPI(API_ROUTE.LOGIN);\n",
            ),
        ),
        (
            "src/c.js",
            concat!(
                "import { API_ROUTE } from './APIService';\n",
                "const fetch = (u) => u;\n",
                "fetch(BASE + API_ROUTE.LOGIN.path);\n",
            ),
        ),
        (
            "src/d.js",
            concat!(
                "import { API_ROUTE } from './APIService';\n",
                "const fixed = (route) => fetch(BASE + '/fixed', { method: route.method });\n",
                "const get = (route) => fetch(BASE + route.path, { method: 'GET' });\n",
                "fixed(API_ROUTE.LOGIN);\n",
                "get(API_ROUTE.LOGIN);\n",
            ),
        ),
    ]);
    assert!(sites.is_empty(), "sites: {sites:?}");
}

// AC-0315: an expression-bodied arrow wrapper joins like a function body;
// the control for the rejected wrappers above. (T-0315)
#[test]
fn arrow_wrappers_join_their_route_parameter() {
    let sites = extract(&[
        ("src/APIService.js", SERVICE),
        (
            "src/d.js",
            concat!(
                "import { API_ROUTE } from './APIService';\n",
                "const send = (route) => fetch(BASE + route.path, { method: route.method });\n",
                "send(API_ROUTE.LOGIN);\n",
            ),
        ),
    ]);
    assert_eq!(
        summary(&sites),
        [(
            "POST".to_string(),
            "/api/auth/login".to_string(),
            "API_ROUTE.LOGIN".to_string(),
            IdentityExpr::Computed("BASE".into()),
        )]
    );
}

// AC-0315: Vue single-file components call the wrapper from their script
// block, at byte offsets in the original file. (T-0315)
#[test]
fn vue_components_call_wrappers_at_original_offsets() {
    let component = concat!(
        "<template><div/></template>\n",
        "<script>\n",
        "import { callAPI, API_ROUTE } from \"../APIService\";\n",
        "export default { methods: { async load() { await callAPI(API_ROUTE.GET_USER_INFO); } } };\n",
        "</script>\n",
    );
    let sites = extract(&[
        ("src/APIService.js", SERVICE),
        ("src/components/Main.vue", component),
    ]);
    assert_eq!(sites.len(), 1, "sites: {sites:?}");
    let start = usize::try_from(sites[0].byte_start).unwrap();
    assert!(component[start..].starts_with("callAPI(API_ROUTE.GET_USER_INFO)"));
}
