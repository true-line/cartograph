//! Coverage matrix (AC-0253): what each installed adapter extracts, stated
//! per language × framework × capability cell instead of one prose sentence.
//!
//! A `Supported` or `Partial` cell names a conformance fixture under
//! `crates/ingest/conformance/<cell-id>/` (`src/` plus `expect.json`). The
//! app crate's conformance test extracts every fixture and asserts its
//! expected facts, so a claim in this table cannot outlive the extractor
//! behaviour behind it. `Unsupported` cells carry no fixture; they exist so
//! preflight and Settings show the hole rather than silently omitting it.

use serde::Serialize;

/// How far an adapter covers one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    /// Recognized; the fixture proves the canonical shape.
    Supported,
    /// Recognized for some shapes; `note` names what is missed.
    Partial,
    /// Not recognized; `note` names the gap.
    Unsupported,
}

/// A kind of fact an adapter can contribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Type declarations (classes, interfaces, objects).
    Types,
    /// Import edges between files and modules.
    Imports,
    /// Call edges between symbols.
    Calls,
    /// Server route registrations (`Endpoint` + `HANDLES`).
    HttpEndpoints,
    /// Client screens (`Screen` + `RENDERS`).
    ClientRoutes,
    /// Outbound HTTP call sites (`fetch`, axios).
    HttpClientCalls,
    /// Message/event producer and consumer sites.
    EventSites,
    /// Persistent data entities.
    DataModel,
    /// Browser-extension contexts, commands and permissions.
    ExtensionContexts,
    /// Infrastructure resources and their capability edges.
    InfraResources,
}

impl Capability {
    /// Wire/fixture spelling (`http_endpoints`).
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Types => "types",
            Capability::Imports => "imports",
            Capability::Calls => "calls",
            Capability::HttpEndpoints => "http_endpoints",
            Capability::ClientRoutes => "client_routes",
            Capability::HttpClientCalls => "http_client_calls",
            Capability::EventSites => "event_sites",
            Capability::DataModel => "data_model",
            Capability::ExtensionContexts => "extension_contexts",
            Capability::InfraResources => "infra_resources",
        }
    }
}

/// Framework slot for language-level capabilities (imports, calls, types).
pub const CORE: &str = "core";

/// One language × framework × capability cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoverageCell {
    /// Language as `AdapterInfo::language` spells it.
    pub language: &'static str,
    /// Framework or library the cell is about, or [`CORE`].
    pub framework: &'static str,
    /// Kind of fact the cell is about.
    pub capability: Capability,
    /// How far the adapter covers it.
    pub support: Support,
    /// What is missed (partial) or absent (unsupported); empty exactly when
    /// supported, so a limited cell can never count as complete.
    pub note: &'static str,
}

impl CoverageCell {
    /// Stable id, also the conformance fixture directory name.
    pub fn id(&self) -> String {
        format!(
            "{}.{}.{}",
            self.language.to_lowercase(),
            self.framework,
            self.capability.as_str()
        )
    }

    /// Fixture directory for a supported or partial cell.
    pub fn fixture(&self) -> Option<String> {
        (self.support != Support::Unsupported).then(|| self.id())
    }
}

/// A cell as reported to Preflight and Settings: compiled-in cells and the
/// cells a gated plugin declares share this owned shape (AC-0253, AC-0263).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportedCell {
    /// Language the cell is about.
    pub language: String,
    /// Framework or library, or [`CORE`].
    pub framework: String,
    /// Capability in snake_case; a plugin may name one the core does not.
    pub capability: String,
    /// How far the adapter covers it.
    pub support: Support,
    /// What is missed (partial) or absent (unsupported).
    pub note: String,
}

impl From<&CoverageCell> for ReportedCell {
    fn from(cell: &CoverageCell) -> Self {
        Self {
            language: cell.language.to_string(),
            framework: cell.framework.to_string(),
            capability: cell.capability.as_str().to_string(),
            support: cell.support,
            note: cell.note.to_string(),
        }
    }
}

const fn cell(
    language: &'static str,
    framework: &'static str,
    capability: Capability,
    support: Support,
    note: &'static str,
) -> CoverageCell {
    CoverageCell {
        language,
        framework,
        capability,
        support,
        note,
    }
}

use Capability::*;
use Support::*;

const NO_CLIENT: &str = "outbound HTTP client calls are recognized only in TypeScript/JavaScript";
const NO_JVM_CLIENT: &str =
    "outbound HTTP client calls are recognized only through Retrofit interfaces";
const NO_EVENTS: &str = "message/event SDK sites are recognized only in TypeScript/JavaScript";
const TSX_ONLY: &str = "recorded in .tsx/.js/.jsx files; plain .ts modules are not scanned";

/// Every cell this build reports, compiled-in adapters only.
pub const COVERAGE_MATRIX: &[CoverageCell] = &[
    cell("TypeScript", CORE, Imports, Supported, ""),
    cell("TypeScript", CORE, Calls, Supported, ""),
    cell("TypeScript", "express", HttpEndpoints, Supported, ""),
    cell("TypeScript", "next", ClientRoutes, Supported, ""),
    cell(
        "TypeScript",
        "next",
        HttpEndpoints,
        Unsupported,
        "pages/api and app-router route handlers are not recognized",
    ),
    cell("TypeScript", "react-router", ClientRoutes, Supported, ""),
    cell("TypeScript", "fetch", HttpClientCalls, Partial, TSX_ONLY),
    cell("TypeScript", "axios", HttpClientCalls, Partial, TSX_ONLY),
    cell("TypeScript", "chrome-messaging", EventSites, Supported, ""),
    cell("TypeScript", "aws-sdk-v3", EventSites, Supported, ""),
    cell(
        "TypeScript",
        "kafkajs",
        EventSites,
        Partial,
        "the producer/consumer must be bound through named variables",
    ),
    cell("TypeScript", "node-events", EventSites, Supported, ""),
    cell("TypeScript", "indexeddb", DataModel, Supported, ""),
    cell(
        "TypeScript",
        "browser-storage",
        InfraResources,
        Supported,
        "",
    ),
    cell("TypeScript", "sdk-client", InfraResources, Supported, ""),
    cell("TypeScript", "auth-provider", InfraResources, Supported, ""),
    cell(
        "TypeScript",
        "http-api",
        InfraResources,
        Partial,
        "only calls whose URL starts with a proven absolute origin; relative URLs and runtime bases name no resource",
    ),
    cell("JavaScript", CORE, Imports, Supported, ""),
    cell("JavaScript", CORE, Calls, Supported, ""),
    cell(
        "JavaScript",
        "express",
        HttpEndpoints,
        Partial,
        "ES module imports only; CommonJS require(\"express\") is not recognized",
    ),
    cell("JavaScript", "next", ClientRoutes, Supported, ""),
    cell("JavaScript", "react-router", ClientRoutes, Supported, ""),
    cell("JavaScript", "fetch", HttpClientCalls, Supported, ""),
    cell("JavaScript", "route-table", HttpClientCalls, Supported, ""),
    cell("JavaScript", "chrome-messaging", EventSites, Supported, ""),
    cell("JavaScript", "indexeddb", DataModel, Supported, ""),
    cell("JavaScript", "sdk-client", InfraResources, Supported, ""),
    cell("Vue", CORE, Imports, Supported, ""),
    cell("Vue", CORE, Calls, Supported, ""),
    cell("Vue", "fetch", HttpClientCalls, Supported, ""),
    cell(
        "Vue",
        "vue-router",
        ClientRoutes,
        Unsupported,
        "templates are not parsed and vue-router route tables are not recognized",
    ),
    cell(
        "Python",
        CORE,
        Imports,
        Partial,
        "in-repository modules stay unresolved placeholders at T0",
    ),
    cell("Python", CORE, Calls, Supported, ""),
    cell("Python", "fastapi", HttpEndpoints, Supported, ""),
    cell("Python", "flask", HttpEndpoints, Supported, ""),
    cell(
        "Python",
        "sdk-client",
        InfraResources,
        Partial,
        "module-level imports only; an identity imported from another module is a Gap",
    ),
    cell("Python", CORE, HttpClientCalls, Unsupported, NO_CLIENT),
    cell("Python", CORE, EventSites, Unsupported, NO_EVENTS),
    cell("Go", CORE, Imports, Supported, ""),
    cell("Go", CORE, Calls, Supported, ""),
    cell("Go", "net-http", HttpEndpoints, Supported, ""),
    cell("Go", "chi", HttpEndpoints, Supported, ""),
    cell("Go", "gin", HttpEndpoints, Supported, ""),
    cell("Go", CORE, HttpClientCalls, Unsupported, NO_CLIENT),
    cell("Go", CORE, EventSites, Unsupported, NO_EVENTS),
    cell("Java", CORE, Types, Supported, ""),
    cell("Java", CORE, Imports, Supported, ""),
    cell("Java", CORE, Calls, Supported, ""),
    cell("Java", "spring-web", HttpEndpoints, Supported, ""),
    cell(
        "Java",
        "jpa",
        DataModel,
        Partial,
        "fields inherited from @MappedSuperclass and @Embeddable types are not merged, \
         and repository methods outside the Spring Data naming vocabulary assert no access",
    ),
    cell("Java", "retrofit", HttpClientCalls, Supported, ""),
    cell("Java", CORE, HttpClientCalls, Unsupported, NO_JVM_CLIENT),
    cell("Java", CORE, EventSites, Unsupported, NO_EVENTS),
    cell("Kotlin", CORE, Types, Supported, ""),
    cell("Kotlin", CORE, Calls, Supported, ""),
    cell("Kotlin", "spring-web", HttpEndpoints, Supported, ""),
    cell("Kotlin", "retrofit", HttpClientCalls, Supported, ""),
    cell("Kotlin", CORE, HttpClientCalls, Unsupported, NO_JVM_CLIENT),
    cell("Kotlin", CORE, EventSites, Unsupported, NO_EVENTS),
    cell(
        "WebExtension",
        "manifest-v3",
        ExtensionContexts,
        Supported,
        "",
    ),
    cell("Terraform", "aws", InfraResources, Supported, ""),
];

/// Cells for one language, in table order.
pub fn cells_for(language: &str) -> impl Iterator<Item = &'static CoverageCell> + '_ {
    COVERAGE_MATRIX
        .iter()
        .filter(move |cell| cell.language == language)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preflight::INSTALLED_ADAPTERS;
    use std::collections::BTreeSet;
    use std::path::Path;

    fn conformance_root() -> &'static Path {
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/conformance"))
    }

    #[test]
    fn cell_ids_are_unique() {
        let mut seen = BTreeSet::new();
        for cell in COVERAGE_MATRIX {
            assert!(seen.insert(cell.id()), "duplicate cell {}", cell.id());
        }
    }

    #[test]
    fn every_installed_language_reports_cells_and_no_cell_is_orphaned() {
        let installed: BTreeSet<_> = INSTALLED_ADAPTERS.iter().map(|a| a.language).collect();
        for language in &installed {
            assert!(
                cells_for(language).next().is_some(),
                "{language} has no coverage cells"
            );
        }
        for cell in COVERAGE_MATRIX {
            assert!(
                installed.contains(cell.language),
                "{} names no installed adapter",
                cell.id()
            );
        }
    }

    #[test]
    fn claims_have_fixtures_and_shortfalls_have_notes() {
        for cell in COVERAGE_MATRIX {
            match cell.fixture() {
                Some(dir) => {
                    let dir = conformance_root().join(dir);
                    assert!(dir.join("src").is_dir(), "{} lacks src/", cell.id());
                    assert!(
                        dir.join("expect.json").is_file(),
                        "{} lacks expect.json",
                        cell.id()
                    );
                }
                None => assert!(!cell.note.is_empty(), "{} needs a note", cell.id()),
            }
            match cell.support {
                Support::Supported => {
                    assert!(
                        cell.note.is_empty(),
                        "{} is limited: mark it partial",
                        cell.id()
                    )
                }
                Support::Partial => assert!(!cell.note.is_empty(), "{} needs a note", cell.id()),
                Support::Unsupported => {}
            }
        }
    }

    #[test]
    fn every_fixture_belongs_to_a_claimed_cell() {
        let claimed: BTreeSet<_> = COVERAGE_MATRIX
            .iter()
            .filter_map(CoverageCell::fixture)
            .collect();
        for entry in std::fs::read_dir(conformance_root()).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(
                claimed.contains(&name),
                "fixture {name} has no claimed cell"
            );
        }
    }
}
