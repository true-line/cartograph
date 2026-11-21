//! Env-file value safety shared by every reader of env-file values (#933,
//! AC-0990, ADR-0075): the events config resolver and the toolchain's
//! allowlisted values apply the same secret test and userinfo strip.

/// True for an env-file template (`.env.example`, `env.sample`,
/// `example.env`, `.env.dist`): a name with an `example`, `sample`,
/// `template` or `dist` dot-segment. Its values are placeholders, so a real
/// env file outranks it (ADR-0075 decision 6).
pub fn is_env_template(name: &str) -> bool {
    name.to_ascii_lowercase()
        .split('.')
        .any(|part| matches!(part, "example" | "sample" | "template" | "dist"))
}

/// `value` with any `user@` removed from a `scheme://user@host…` URL
/// (ADR-0075 decision 4); any other value unchanged.
pub fn without_userinfo(value: &str) -> String {
    let Some((scheme, rest)) = value.split_once("://") else {
        return value.to_string();
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..authority_end].rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://{host}{}", &rest[authority_end..]),
        None => value.to_string(),
    }
}

/// Why an env-file value counts as a secret, or none (#933, AC-0990):
/// a recognized secret pattern (provider tokens, private keys, cloud access
/// keys), a token-shaped value, a URL carrying a password, or a value under
/// a credential key name that is not an obvious placeholder.
pub fn env_secret_reason(key: &str, value: &str) -> Option<&'static str> {
    if value.is_empty() {
        return None;
    }
    if !crate::recognized_secrets(value).is_empty() {
        return Some("recognized secret pattern");
    }
    if crate::is_token_shaped(value) {
        return Some("token-shaped value");
    }
    if url_password(value) {
        return Some("URL with a password");
    }
    let credential_key = crate::is_sensitive_name(key) || credential_suffix(key);
    if credential_key && !placeholder(value) {
        return Some("value under a credential key");
    }
    None
}

/// Whether `value` is a `scheme://user:password@host` URL.
fn url_password(value: &str) -> bool {
    let Some((_, rest)) = value.split_once("://") else {
        return false;
    };
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    authority.rsplit_once('@').is_some_and(|(userinfo, _)| {
        userinfo
            .split_once(':')
            .is_some_and(|(_, pass)| !pass.is_empty())
    })
}

/// Whether `key` ends in a credential word (`DB_PASSWORD`, `APP_SECRET`,
/// `GITHUB_TOKEN`, `STRIPE_API_KEY`), whatever the value's length.
fn credential_suffix(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "pwd",
        "secret",
        "token",
        "apikey",
        "api_key",
        "api-key",
        "private_key",
        "secret_key",
        "access_key",
        "credentials",
    ]
    .iter()
    .any(|word| lower.ends_with(word))
}

/// Whether a credential value is an explicit template placeholder rather
/// than a secret: a reference (`${VAR}`, `<your-key>`), one repeated
/// character (`xxxx`), a whole placeholder word (`changeme`), or a
/// `your-...` / `your_...` fill-in. A value merely containing such a word
/// (`mydummyPass!42`) is not one.
fn placeholder(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    (value.starts_with("${") && value.ends_with('}'))
        || (value.starts_with('<') && value.ends_with('>'))
        || value.chars().all(|c| value.starts_with(c))
        || lower.starts_with("your-")
        || lower.starts_with("your_")
        || [
            "changeme",
            "change-me",
            "change_me",
            "example",
            "placeholder",
            "replaceme",
            "replace-me",
            "replace_me",
            "todo",
            "dummy",
            "fixme",
        ]
        .contains(&lower.as_str())
}
