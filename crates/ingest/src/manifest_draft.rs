//! Pure new-manifest drafting (#1671, AC-0002). Never reads repositories or
//! writes a file; the desktop host owns explicit save and destination selection.

use crate::manifest::{ManifestGatewayRoute, ManifestService, SystemManifest};
use std::collections::{BTreeMap, BTreeSet};

/// One explicitly declared repository, with all extractors enabled.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DraftRepo {
    /// Exact repository location; no path or identity normalization.
    pub url: String,
}

/// A configuration row, kept as a row until duplicates have been rejected.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DraftConfig {
    /// Exact source configuration key, including punctuation and case.
    pub key: String,
    /// Exact author-declared value (an empty value is allowed).
    pub value: String,
}

/// The supported new-file subset. Existing advanced manifests are never loaded
/// into this type for rewriting, so their unsupported fields cannot be lost.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ManifestDraft {
    /// Explicit repository declarations.
    pub repos: Vec<DraftRepo>,
    /// Exact configuration declarations.
    pub env: Vec<DraftConfig>,
    /// Explicit service ownership and base URLs.
    pub services: Vec<ManifestService>,
    /// Optional gateway routing declarations.
    pub gateway_routes: Vec<ManifestGatewayRoute>,
}

#[derive(serde::Serialize)]
struct Document<'a> {
    repos: &'a [DraftRepo],
    env: BTreeMap<&'a str, &'a str>,
    services: &'a [ManifestService],
    gateway_routes: &'a [ManifestGatewayRoute],
}

impl ManifestDraft {
    /// Validate and render literal TOML. Required-field checks apply only to
    /// the wizard; existing hand-authored manifest semantics are unchanged.
    pub fn render(&self) -> Result<String, String> {
        if self.repos.is_empty() {
            return Err("Add at least one repository.".into());
        }
        let mut repos = BTreeSet::new();
        for repo in &self.repos {
            required(&repo.url, "Repository location")?;
            if !repos.insert(&repo.url) {
                return Err("A repository location is declared more than once.".into());
            }
        }
        let mut env = BTreeMap::new();
        for row in &self.env {
            required(&row.key, "Configuration key")?;
            if env.insert(row.key.as_str(), row.value.as_str()).is_some() {
                return Err("A configuration key is declared more than once.".into());
            }
        }
        for service in &self.services {
            required(&service.name, "Service name")?;
            required(&service.repo, "Service repository")?;
            if service.base_urls.is_empty() {
                return Err("Add at least one base URL for each service.".into());
            }
        }
        for route in &self.gateway_routes {
            required(&route.prefix, "Gateway prefix")?;
            required(&route.service, "Gateway service")?;
        }
        let text = toml::to_string_pretty(&Document {
            repos: &self.repos,
            env,
            services: &self.services,
            gateway_routes: &self.gateway_routes,
        })
        .map_err(|_| "Could not encode the manifest.".to_string())?;
        // Deserialize the actual serialized bytes, then use the production
        // reference/URL validator. No parallel frontend interpretation.
        let parsed: SystemManifest = toml::from_str(&text)
            .map_err(|_| "Could not validate the generated manifest.".to_string())?;
        parsed.validate_bindings().map_err(|err| err.to_string())?;
        Ok(text)
    }
}

fn required(value: &str, label: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{label} is required."))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> ManifestDraft {
        ManifestDraft {
            repos: vec![
                DraftRepo {
                    url: "../client".into(),
                },
                DraftRepo {
                    url: "../api".into(),
                },
            ],
            env: vec![DraftConfig {
                key: "SETTINGS.apiUrl".into(),
                value: "https://api.example.test".into(),
            }],
            services: vec![ManifestService {
                name: "api".into(),
                repo: "../api".into(),
                base_urls: vec!["https://api.example.test".into()],
            }],
            gateway_routes: vec![ManifestGatewayRoute {
                base_url: "https://gateway.example.test".into(),
                prefix: "/api".into(),
                service: "api".into(),
                strip_prefix: true,
            }],
        }
    }

    // AC-0002: literal identity strings survive the production file loader.
    #[test]
    fn manifest_draft_round_trips_literal_config_and_bindings() {
        let mut input = draft();
        input.env.push(DraftConfig {
            key: " odd.\"雪\\key\n".into(),
            value: "value\n\t\"\\雪\u{0001}".into(),
        });
        input.env.push(DraftConfig {
            key: "empty".into(),
            value: String::new(),
        });
        let text = input.render().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cartograph.system.toml");
        std::fs::write(&path, text).unwrap();
        let loaded = SystemManifest::load(&path).unwrap();
        for row in &input.env {
            assert_eq!(loaded.env.get(&row.key), Some(&row.value));
        }
        assert_eq!(loaded.repos[0].url, "../client");
        assert_eq!(loaded.services[0].repo, "../api");
        assert_eq!(loaded.gateway_routes[0].service, "api");
        assert!(loaded.gateway_routes[0].strip_prefix);
    }

    // AC-0002: rows cannot collapse silently into the serialized map.
    #[test]
    fn manifest_draft_rejects_duplicate_keys_and_incomplete_rows() {
        let mut input = draft();
        input.env.push(DraftConfig {
            key: input.env[0].key.clone(),
            value: "different".into(),
        });
        assert!(input.render().unwrap_err().contains("configuration key"));
        input.env.pop();
        input.services[0].base_urls.clear();
        assert!(input.render().unwrap_err().contains("base URL"));
        input.services.clear();
        input.gateway_routes.clear();
        input.repos[0].url = "  ".into();
        assert!(input.render().unwrap_err().contains("required"));
        input.repos.clear();
        assert!(input.render().unwrap_err().contains("repository"));
    }

    // AC-0002: production binding policy rejects each invalid declaration.
    #[test]
    fn manifest_draft_uses_production_binding_validation() {
        let mut input = draft();
        input.services.push(input.services[0].clone());
        assert!(input.render().unwrap_err().contains("more than once"));
        input.services.pop();
        input.services[0].repo = "missing".into();
        assert!(input.render().unwrap_err().contains("no [[repos]]"));
        input.services[0].repo = "../api".into();
        input.services[0].base_urls = vec!["relative/path".into()];
        assert!(input.render().unwrap_err().contains("base URL"));
        input.services[0].base_urls = vec!["https://api.example.test".into()];
        input.gateway_routes[0].service = "missing".into();
        assert!(input.render().unwrap_err().contains("undeclared service"));
        input.gateway_routes[0].service = "api".into();
        input.gateway_routes[0].base_url = "https://gateway.example.test?query".into();
        assert!(input.render().unwrap_err().contains("base URL"));
    }
}
