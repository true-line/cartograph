//! Client-side registries (SPEC-00 §3.5, US-0005): router registration
//! patterns → `Screen` nodes, data-fetch call shapes → `FETCHES` edges.
//! Like every registry, this is data, not inference — the language adapter
//! consults it while walking the AST and emits [`FetchSite`] facts; the
//! `events` crate resolves fetch URLs against recovered endpoints.

use crate::events::IdentityExpr;

/// Router registry: JSX route components proven from their module import.
pub struct RouterRegistry {
    /// npm modules whose import marks a file as using this router.
    pub modules: &'static [&'static str],
    /// JSX component whose `path`/`element` attributes declare a screen.
    pub route_component: &'static str,
}

/// React Router (v6+; `react-router` and `react-router-dom` share the API).
pub const REACT_ROUTER: RouterRegistry = RouterRegistry {
    modules: &["react-router-dom", "react-router"],
    route_component: "Route",
};

/// Directory whose file structure declares Next.js pages-router screens
/// (`pages/users/[id].tsx` → route `/users/[id]`).
pub const NEXT_PAGES_DIR: &str = "pages";

/// A data-fetch call site emitted by a language adapter — the client-side
/// analog of an event site. The URL classifies exactly like a channel
/// identity (literal / env ref / computed); the `events` crate resolves it
/// against recovered `Endpoint` nodes (AC-0014).
#[derive(Debug, Clone)]
pub struct FetchSite {
    /// HTTP method, uppercase (`GET` when the call shape defaults it).
    pub method: String,
    /// The URL expression, classified.
    pub url: IdentityExpr,
    /// Symbol id of the enclosing component/function, if any.
    pub symbol: Option<String>,
    /// File the site was found in (repo-relative).
    pub path: String,
    /// Byte span of the matched call.
    pub byte_start: u64,
    /// End of the matched call's byte span.
    pub byte_end: u64,
    /// Base URL a relative `url` joins to (#737): a Retrofit `baseUrl(...)`
    /// or the base a table-driven client prepends at its join site. `None`
    /// when `url` is the whole request URL.
    pub base: Option<FetchBase>,
    /// Declared client entry the call comes from (`API_ROUTE.LOGIN`,
    /// `ApiService.stats`), named in the Gap when the call stays unresolved.
    pub entry: Option<String>,
}

/// How a client library joins a request path to its base URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseJoin {
    /// String concatenation, `base + path` (table-driven JS clients).
    Concat,
    /// RFC 3986 reference resolution, as Retrofit's `HttpUrl.resolve`: a
    /// path with a leading `/` replaces the base path, a relative one
    /// replaces its last segment.
    Resolve,
    /// httpx `base_url` merging (#1240), as `Client._merge_url`: a URL with
    /// a scheme and a host stands alone; any other URL's raw path (query
    /// included, leading `/`s removed) is appended to the base's raw path,
    /// which httpx first ends with `/` — after the base's query, if it has
    /// one. A network-path URL (`//host/x`) is relative there and adds only
    /// its path. Fragments never reach the server and are dropped.
    Merge,
}

/// The base-URL half of a joined request, with the site that declares the
/// join so a bound edge or Gap can cite it.
#[derive(Debug, Clone)]
pub struct FetchBase {
    /// The base URL expression, classified like any identity.
    pub url: IdentityExpr,
    /// How the path joins to it.
    pub join: BaseJoin,
    /// File declaring the join (repo-relative).
    pub path: String,
    /// Byte offset of the declaring expression.
    pub byte_start: u64,
}

/// Join a literal request path to a resolved base URL. Under `Resolve` and
/// `Merge` an absolute `path` stands alone; `Concat` never inspects either
/// side.
pub fn join_base(base: &str, path: &str, join: BaseJoin) -> String {
    match join {
        BaseJoin::Concat => format!("{base}{path}"),
        BaseJoin::Merge => {
            let path = path.split('#').next().unwrap_or(path);
            let (scheme, rest) = match path.split_once(':') {
                Some((scheme, rest)) if is_scheme(scheme) => (Some(scheme), rest),
                _ => (None, path),
            };
            let (host, reference) = match rest.strip_prefix("//") {
                Some(network) => {
                    network.split_at(network.find(['/', '?']).unwrap_or(network.len()))
                }
                None => ("", rest),
            };
            if scheme.is_some() && !host.is_empty() {
                return without_dot_segments(path);
            }
            let base = base.split('#').next().unwrap_or(base);
            let start = base.find("://").map_or(0, |i| i + 3);
            let end = base[start..]
                .find(['/', '?'])
                .map_or(base.len(), |i| start + i);
            let (origin, base_raw) = base.split_at(end);
            // httpx normalises an empty base path to `/`.
            let base_raw = if base_raw.starts_with('/') {
                base_raw.to_string()
            } else {
                format!("/{base_raw}")
            };
            let slash = if base_raw.ends_with('/') { "" } else { "/" };
            without_dot_segments(&format!(
                "{origin}{base_raw}{slash}{}",
                reference.trim_start_matches('/')
            ))
        }
        BaseJoin::Resolve => {
            if path.starts_with("http://") || path.starts_with("https://") {
                return path.to_string();
            }
            let base = base.split(['?', '#']).next().unwrap_or(base);
            let authority_end = base
                .find("://")
                .map(|i| i + 3)
                .and_then(|start| base[start..].find('/').map(|j| start + j))
                .unwrap_or(base.len());
            let (origin, base_path) = base.split_at(authority_end);
            // The query or fragment of the reference is not path.
            let split = path.find(['?', '#']).unwrap_or(path.len());
            let (reference, suffix) = path.split_at(split);
            let merged = if reference.starts_with('/') {
                reference.to_string()
            } else {
                // Merge: drop the base path's last segment, keep the directory.
                let dir = match base_path.rfind('/') {
                    Some(i) => &base_path[..=i],
                    None => "/",
                };
                format!("{dir}{reference}")
            };
            format!("{origin}{}{suffix}", remove_dot_segments(&merged))
        }
    }
}

/// Whether `scheme` is an RFC 3986 scheme: a letter, then letters, digits,
/// `+`, `-` or `.`.
/// httpx's `normalize_path` on an absolute URL: `.` and `..` segments of
/// the path (before any `?`) are dropped, never climbing above the root.
fn without_dot_segments(url: &str) -> String {
    let start = url.find("://").map_or(0, |i| i + 3);
    let end = url[start..]
        .find(['/', '?'])
        .map_or(url.len(), |i| start + i);
    let query = url[end..].find('?').map_or(url.len(), |i| end + i);
    let (origin, rest) = url.split_at(end);
    let (path, query) = rest.split_at(query - end);
    let segments: Vec<&str> = path.split('/').collect();
    if !segments
        .iter()
        .any(|segment| matches!(*segment, "." | ".."))
    {
        return url.to_string();
    }
    let mut out: Vec<&str> = Vec::new();
    for segment in segments {
        match segment {
            "." => {}
            ".." => {
                if out.len() > 1 || out.first().is_some_and(|first| !first.is_empty()) {
                    out.pop();
                }
            }
            _ => out.push(segment),
        }
    }
    // An emptied path is the root.
    let path = match out.join("/") {
        path if path.is_empty() => "/".to_string(),
        path => path,
    };
    format!("{origin}{path}{query}")
}

fn is_scheme(scheme: &str) -> bool {
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// RFC 3986 §5.2.4 for an absolute path: `.` segments vanish and `..`
/// removes the segment before it, never climbing above the root.
fn remove_dot_segments(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    for (i, segment) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        match *segment {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                out.pop();
                if last {
                    out.push("");
                }
            }
            other => out.push(other),
        }
    }
    format!("/{}", out.join("/"))
}

/// Give every unjoined site the repository's one client base (#737): the
/// Retrofit `baseUrl(...)` declared once per repo. Zero or several distinct
/// bases cannot be chosen between, so each site carries a `Computed` base
/// saying why, at its own span, and stays a Gap downstream.
pub fn join_repo_base(sites: &mut [FetchSite], bases: &[FetchBase], library: &str) {
    let mut distinct: Vec<&FetchBase> = Vec::new();
    let mut sorted: Vec<&FetchBase> = bases.iter().collect();
    sorted.sort_by(|a, b| (&a.path, a.byte_start).cmp(&(&b.path, b.byte_start)));
    for base in sorted {
        if !distinct.iter().any(|seen| seen.url == base.url) {
            distinct.push(base);
        }
    }
    for site in sites.iter_mut().filter(|site| site.base.is_none()) {
        site.base = Some(match distinct.as_slice() {
            [one] => (*one).clone(),
            many => FetchBase {
                url: IdentityExpr::Computed(match many.len() {
                    0 => format!("no {library} base URL in repo"),
                    n => format!("{n} distinct {library} base URL candidates"),
                }),
                join: BaseJoin::Resolve,
                path: site.path.clone(),
                byte_start: site.byte_start,
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn react_router_registry_shape() {
        assert!(REACT_ROUTER.modules.contains(&"react-router-dom"));
        assert_eq!(REACT_ROUTER.route_component, "Route");
    }

    #[test]
    fn retrofit_resolution_and_concatenation_join_paths() {
        let r = BaseJoin::Resolve;
        assert_eq!(join_base("https://h.io", "api/x", r), "https://h.io/api/x");
        assert_eq!(
            join_base("https://h.io/v1/", "api/x", r),
            "https://h.io/v1/api/x"
        );
        assert_eq!(
            join_base("https://h.io/v1/", "/api/x", r),
            "https://h.io/api/x"
        );
        assert_eq!(
            join_base("https://h.io/v1", "api/x", r),
            "https://h.io/api/x"
        );
        assert_eq!(
            join_base("https://h.io/", "https://o.io/y", r),
            "https://o.io/y"
        );
        let c = BaseJoin::Concat;
        assert_eq!(join_base("https://h.io", "/api/x", c), "https://h.io/api/x");
    }

    #[test]
    fn httpx_merge_appends_under_the_base_path() {
        // AC-0425: httpx joins under the base path, whatever the slashes.
        let m = BaseJoin::Merge;
        assert_eq!(join_base("https://h.io", "/users", m), "https://h.io/users");
        assert_eq!(
            join_base("https://h.io/api", "/users", m),
            "https://h.io/api/users"
        );
        assert_eq!(
            join_base("https://h.io/api/", "users", m),
            "https://h.io/api/users"
        );
        assert_eq!(join_base("https://h.io", "", m), "https://h.io/");
        assert_eq!(join_base("https://h.io/", "//x", m), "https://h.io/");
        // httpx drops dot segments of the merged path, never above the root.
        assert_eq!(
            join_base("https://h/api/", "../users", m),
            "https://h/users"
        );
        assert_eq!(join_base("https://h/api", "./a/../b", m), "https://h/api/b");
        assert_eq!(join_base("https://h/api", "../../x", m), "https://h/x");
        assert_eq!(join_base("https://h/a/./b", "c/..", m), "https://h/a/b");
        assert_eq!(join_base("https://h/a", "..", m), "https://h/");
        assert_eq!(
            join_base("https://h/api", "../u?next=../x", m),
            "https://h/u?next=../x"
        );
        assert_eq!(
            join_base("https://h/api", "https://o.io/a/../b", m),
            "https://o.io/b"
        );
        // httpx appends to the raw path, so a base query swallows the path.
        assert_eq!(
            join_base("https://h.io/api?k=1", "users?a=2", m),
            "https://h.io/api?k=1/users?a=2"
        );
        assert_eq!(
            join_base("https://h.io/api#top", "users#x", m),
            "https://h.io/api/users"
        );
        // A network-path URL has no scheme, so httpx merges only its path.
        assert_eq!(
            join_base("https://h.io/api", "//o.io/y?q=1", m),
            "https://h.io/api/y?q=1"
        );
        assert_eq!(
            join_base("https://h.io/api", "https://o.io/y", m),
            "https://o.io/y"
        );
        assert_eq!(
            join_base("https://h.io/api", "HTTP://o.io/y#f", m),
            "HTTP://o.io/y"
        );
    }

    // #757 review: resolution removes dot segments (RFC 3986 §5.2.4); the
    // cases are §5.4's, against its base `http://a/b/c/d;p?q`.
    #[test]
    fn retrofit_resolution_removes_dot_segments() {
        let base = "http://a/b/c/d;p?q";
        for (reference, expected) in [
            ("../users", "http://a/b/users"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../..", "http://a/"),
            ("../../../g", "http://a/g"),
            ("/./g", "http://a/g"),
            ("/../g", "http://a/g"),
            ("g.", "http://a/b/c/g."),
            ("..g", "http://a/b/c/..g"),
            ("./g/.", "http://a/b/c/g/"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
            ("g?y/./x", "http://a/b/c/g?y/./x"),
            ("g#s/../x", "http://a/b/c/g#s/../x"),
        ] {
            assert_eq!(
                join_base(base, reference, BaseJoin::Resolve),
                expected,
                "{reference}"
            );
        }
        assert_eq!(
            join_base("https://h/v1/dir/", "../users", BaseJoin::Resolve),
            "https://h/v1/users"
        );
    }

    fn site(path: &str) -> FetchSite {
        FetchSite {
            method: "GET".into(),
            url: IdentityExpr::Literal("api/x".into()),
            symbol: None,
            path: path.into(),
            byte_start: 3,
            byte_end: 9,
            base: None,
            entry: None,
        }
    }

    fn base(url: &str, path: &str, byte_start: u64) -> FetchBase {
        FetchBase {
            url: IdentityExpr::Literal(url.into()),
            join: BaseJoin::Resolve,
            path: path.into(),
            byte_start,
        }
    }

    // AC-0315: one distinct repo base joins every site (repeats of the same
    // base collapse to the first); none or several leave a named Computed
    // base at the site itself. (T-0315)
    #[test]
    fn repo_base_joins_only_when_unique() {
        let mut sites = [site("Api.kt")];
        join_repo_base(
            &mut sites,
            &[
                base("https://a.io", "B.kt", 9),
                base("https://a.io", "A.kt", 4),
            ],
            "Retrofit",
        );
        let joined = sites[0].base.as_ref().unwrap();
        assert_eq!((joined.path.as_str(), joined.byte_start), ("A.kt", 4));

        let mut sites = [site("Api.kt")];
        join_repo_base(
            &mut sites,
            &[
                base("https://a.io", "A.kt", 4),
                base("https://b.io", "B.kt", 9),
            ],
            "Retrofit",
        );
        let ambiguous = sites[0].base.as_ref().unwrap();
        assert_eq!(
            ambiguous.url,
            IdentityExpr::Computed("2 distinct Retrofit base URL candidates".into())
        );
        assert_eq!(
            (ambiguous.path.as_str(), ambiguous.byte_start),
            ("Api.kt", 3)
        );

        let mut sites = [site("Api.kt")];
        join_repo_base(&mut sites, &[], "Retrofit");
        assert_eq!(
            sites[0].base.as_ref().unwrap().url,
            IdentityExpr::Computed("no Retrofit base URL in repo".into())
        );
    }
}
