//! Readable Capability titles (#1443): what a reader does with the group's
//! operations, in the words of the class, Blueprint, package or module that
//! serves them.
//!
//! A title is derived, never invented: the subject comes from the source
//! name with its framework words (`Resolver`, `Controller`, `views`, …)
//! removed, the verb from whether any operation changes state, and the
//! audience from the API or parent group that serves it. `DraftOrderResolver`
//! serving admin mutations reads `Manage draft orders (admin API)`;
//! `ShopProductsResolver` serving shop queries reads `View products (shop API)`.

/// Framework words that name the mechanism, not the subject. Removed from
/// the end of a source name while a subject word remains.
const MECHANISM: &[&str] = &[
    "admin",
    "api",
    "blueprint",
    "controller",
    "controllers",
    "endpoint",
    "endpoints",
    "handler",
    "handlers",
    "http",
    "resolver",
    "resolvers",
    "resource",
    "resources",
    "rest",
    "route",
    "router",
    "routes",
    "service",
    "shop",
    "view",
    "views",
    "web",
];

/// Framework and audience words removed wherever they stand, while a
/// subject word remains: `ShopAuthResolver`, `OrderController.Audit`.
/// `api` is not one of them: it is the subject of `ApiKeyResolver`.
const INNER: &[&str] = &[
    "admin",
    "controller",
    "controllers",
    "handler",
    "handlers",
    "resolver",
    "resolvers",
    "shop",
    "views",
];

/// Subjects read as uncountable: not pluralized.
const MASS: &[&str] = &[
    "access",
    "analytics",
    "authentication",
    "billing",
    "configuration",
    "data",
    "environment",
    "export",
    "health",
    "history",
    "import",
    "info",
    "information",
    "login",
    "logout",
    "metadata",
    "onboarding",
    "search",
    "security",
    "settings",
    "setup",
    "status",
    "sync",
];

/// Abbreviations spelled as a reader expects.
const SPELLED: &[(&str, &str)] = &[
    ("api", "API"),
    ("auth", "authentication"),
    ("dns", "DNS"),
    ("fido", "FIDO"),
    ("gpg", "GPG"),
    ("graphql", "GraphQL"),
    ("id", "ID"),
    ("json", "JSON"),
    ("mfa", "MFA"),
    ("oauth", "OAuth"),
    ("oidc", "OIDC"),
    ("otp", "OTP"),
    ("pgp", "PGP"),
    ("saml", "SAML"),
    ("smtp", "SMTP"),
    ("sso", "SSO"),
    ("ui", "UI"),
    ("url", "URL"),
];

/// Lowercase words of an identifier: camelCase, PascalCase with acronyms,
/// snake_case, kebab-case, dotted and path names.
pub(super) fn words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut out = Vec::new();
    let mut word = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            continue;
        }
        let prev = i.checked_sub(1).and_then(|p| chars.get(p)).copied();
        let next = chars.get(i + 1).copied();
        let boundary = c.is_uppercase()
            && prev.is_some_and(|p| {
                (p.is_alphanumeric() && !p.is_uppercase())
                    || (p.is_uppercase() && next.is_some_and(char::is_lowercase))
            });
        if boundary && !word.is_empty() {
            out.push(std::mem::take(&mut word));
        }
        word.extend(c.to_lowercase());
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

fn plural(word: &str) -> String {
    if MASS.contains(&word) || word.ends_with('s') || !word.chars().all(|c| c.is_ascii_lowercase())
    {
        return word.to_owned();
    }
    if let Some(stem) = word.strip_suffix('y')
        && stem.ends_with(|c: char| !"aeiou".contains(c))
    {
        return format!("{stem}ies");
    }
    if ["x", "z", "ch", "sh"].iter().any(|end| word.ends_with(end)) {
        return format!("{word}es");
    }
    format!("{word}s")
}

pub(super) fn spelled(word: &str) -> &str {
    SPELLED
        .iter()
        .find(|(short, _)| *short == word)
        .map_or(word, |(_, long)| long)
}

/// The subject a class name speaks of: its words without the framework
/// words, abbreviations spelled out, the last word plural. A class usually
/// names an entity (`DraftOrderResolver`), so its subject is the many of it.
/// A name made only of framework words keeps them.
pub(super) fn subject(name: &str) -> String {
    phrase(name, true)
}

/// The subject a file or module name speaks of, as [`subject`] but never
/// pluralized: a file may name a product or a provider (`proton.py`).
pub(super) fn file_subject(name: &str) -> String {
    phrase(name, false)
}

/// Whether a file or module name is generic and says nothing of its subject.
pub(super) fn generic(name: &str) -> bool {
    let words = words(name);
    words.is_empty()
        || words.iter().all(|w| {
            MECHANISM.contains(&w.as_str())
                || [
                    "app", "base", "common", "index", "init", "main", "mod", "lib", "utils",
                ]
                .contains(&w.as_str())
        })
}

/// Words of a whole name, abbreviations spelled out: an audience.
pub(super) fn spelled_words(name: &str) -> String {
    words(name)
        .iter()
        .map(|w| spelled(w).to_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn phrase(name: &str, pluralize: bool) -> String {
    let mut kept = words(name);
    while kept.len() > 1 && MECHANISM.contains(&kept[kept.len() - 1].as_str()) {
        kept.pop();
    }
    if kept.iter().any(|w| !INNER.contains(&w.as_str())) {
        kept.retain(|w| !INNER.contains(&w.as_str()));
    }
    // `stock in transit` is one thing; its plural is not `transits`.
    let pluralize = pluralize
        && !kept
            .iter()
            .any(|w| ["at", "by", "for", "in", "of", "on", "to", "with"].contains(&w.as_str()));
    let last = kept.len().saturating_sub(1);
    kept.iter()
        .enumerate()
        .map(|(i, w)| {
            let w = spelled(w);
            // A spelled-out abbreviation is kept as written.
            if pluralize && i == last && w.chars().all(|c| !c.is_uppercase()) {
                plural(w)
            } else {
                w.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Who a GraphQL API is served to: `admin` and `admin-api` read `admin API`.
pub(super) fn api_audience(api: &str) -> String {
    let mut words = words(api);
    if words.len() > 1 && words.last().is_some_and(|w| w == "api") {
        words.pop();
    }
    words.push("API".into());
    words.join(" ")
}

/// Words that name an action, not a thing: a subject that starts with one
/// is the title itself (`Reset password`, `Delete account`).
const ACTIONS: &[&str] = &[
    "activate",
    "add",
    "approve",
    "authorize",
    "cancel",
    "change",
    "check",
    "confirm",
    "create",
    "delete",
    "disable",
    "enable",
    "enter",
    "exit",
    "export",
    "forgot",
    "import",
    "invite",
    "join",
    "link",
    "login",
    "logout",
    "manage",
    "register",
    "remove",
    "request",
    "resend",
    "reset",
    "restore",
    "send",
    "setup",
    "sign",
    "subscribe",
    "transfer",
    "unlink",
    "unsubscribe",
    "update",
    "upload",
    "verify",
];

/// `Manage …` when any operation changes state, `View …` otherwise, then
/// the subject and, when known, who it is served to. A subject that already
/// names an action (`reset password`) stands alone, capitalized.
pub(super) fn title(subject: &str, audience: Option<&str>, mutates: bool) -> String {
    let first = subject.split(' ').next().unwrap_or("");
    let head = if ACTIONS.contains(&first) {
        let mut chars = subject.chars();
        chars
            .next()
            .map(|c| c.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    } else {
        let verb = if mutates { "Manage" } else { "View" };
        format!("{verb} {subject}")
    };
    match audience {
        Some(audience) => format!("{head} ({audience})"),
        None => head,
    }
}

/// Whether an operation changes state: a GraphQL mutation, or an HTTP
/// method other than GET, HEAD and OPTIONS.
pub(super) fn mutating(method: &str) -> bool {
    !matches!(
        method.to_ascii_uppercase().as_str(),
        "GET" | "HEAD" | "OPTIONS" | "QUERY"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-0851: a source name reads as the subject it speaks of.
    #[test]
    fn source_names_read_as_subjects() {
        assert_eq!(subject("DraftOrderResolver"), "draft orders");
        assert_eq!(subject("ShopProductsResolver"), "products");
        assert_eq!(subject("SupplierStockAdminResolver"), "supplier stocks");
        assert_eq!(subject("CountryResolver"), "countries");
        assert_eq!(
            subject("SupplierStockInTransitResolver"),
            "supplier stock in transit"
        );
        assert_eq!(subject("ShopAuthResolver"), "authentication");
        assert_eq!(subject("APIKeyResolver"), "API keys");
        assert_eq!(subject("ApiKeyResolver"), "API keys");
        assert_eq!(subject("SearchResolver"), "search");
        assert_eq!(subject("custom_domain"), "custom domains");
        assert_eq!(subject("abuser_lookup"), "abuser lookups");
        assert_eq!(subject("BoxController"), "boxes");
        assert_eq!(subject("Resolver"), "resolvers");
        assert_eq!(subject("oidc"), "OIDC");
        assert_eq!(subject("OrderController.Audit"), "order audits");
        assert_eq!(subject("InvoiceResource"), "invoices");
        assert_eq!(subject("ShopController"), "shops");
        assert_eq!(api_audience("admin-api"), "admin API");
        assert_eq!(api_audience("shop"), "shop API");
        assert_eq!(subject("v2"), "v2");
        assert_eq!(file_subject("custom_domain"), "custom domain");
        assert_eq!(file_subject("proton"), "proton");
        assert!(generic("views") && generic("__init__") && generic("mod"));
        assert!(!generic("alias"));
        assert_eq!(spelled_words("api"), "API");
        assert_eq!(spelled_words("admin.domain_check"), "admin domain check");
        assert_eq!(subject("$OrdersResolver"), "orders");
        assert_eq!(subject("注文Resolver"), "注文");
    }

    // AC-0851: the verb says whether the group changes state.
    #[test]
    fn titles_say_what_the_reader_does() {
        assert_eq!(
            title("draft orders", Some("admin API"), true),
            "Manage draft orders (admin API)"
        );
        assert_eq!(title("products", None, false), "View products");
        assert_eq!(
            title("reset password", Some("authentication"), true),
            "Reset password (authentication)"
        );
        assert!(mutating("POST") && mutating("mutation") && mutating("delete"));
        assert!(!mutating("GET") && !mutating("query") && !mutating("HEAD"));
    }
}
