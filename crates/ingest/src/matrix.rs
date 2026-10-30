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

/// What the Go HTTP client cell reads (AC-0434).
const GO_CLIENT: &str = "`net/http` package functions, `http.DefaultClient` verbs and `NewRequest` sites; a constructed `http.Client` is read only through its `NewRequest`, and a URL must be an exact literal";
/// What the Python HTTP client cells read (AC-0424).
const PY_CLIENT: &str = "module functions and once-bound `Session`/`Client` receivers in their own scope; instance attributes and passed-in clients are not read, and a URL is followed only within its file";
const NO_JVM_CLIENT: &str = "outbound HTTP client calls are recognized only through Retrofit interfaces, Spring `RestTemplate` and `WebClient`, OkHttp request builders and Apache HttpClient request constructors";
/// What the Kotlin WebClient client cell reads (AC-0457).
const KT_WEBCLIENT: &str = "`WebClient` request chains `<client>.<verb>().uri(…)` on a named-imported or fully qualified `WebClient` typed local, parameter or property, `this.x`, or an inline `WebClient.create(…)`/`WebClient.builder()…build()`; the base is read only from an inline literal `create`/`baseUrl` argument, so a held client's base is an undeclared runtime base, and a `uri` must be a pure string literal";
/// What the Kotlin OkHttp client cell reads (AC-0477).
const KT_OKHTTP: &str = "inline `Request.Builder()…build()` chains on a named-imported (or aliased) or fully qualified `okhttp3.Request.Builder` whose every link is a positional builder member; a builder held in a variable and built later, a trailing lambda, a link name the file declares or imports and files with wildcard imports are not read, and a URL is a literal only as a pure string `url(\"…\")`";
/// What the Kotlin Apache HttpClient cell reads (AC-0471).
const KT_APACHE: &str = "`HttpGet(<url>)` and the other HttpClient 4 and 5 request classes, named-imported (or aliased) or fully qualified, with one positional argument and no trailing lambda; a file-declared name, a binding of the first segment, a captured `org` (declared, imported or possibly wildcard-imported) and no-argument constructors are not read, and a URL is a literal only as a pure string literal";
/// What the Kotlin RestTemplate client cell reads (AC-0456).
const KT_REST: &str = "named-imported or fully qualified `RestTemplate` receivers: a typed local, parameter or property, `this.x`, or an inline `RestTemplate()`; star imports, `RestTemplateBuilder` results, named arguments and a one-argument `getForObject<T>(url)` are not read, and a URL must be a pure string literal";
/// What the Kotlin Spring Kafka event cell reads (AC-0468).
const KT_KAFKA: &str = "`send` on a named-imported or fully qualified `KafkaTemplate` typed or constructed local, parameter or property, or `this.x`, and function-level `@KafkaListener` `topics`; subclasses, `sendDefault`, star imports, named arguments, trailing lambdas and class-level listeners are not read, and a topic must be a pure string literal with no `#{…}` expression";
/// What the Java RestTemplate client cell reads (AC-0435).
const JAVA_REST: &str = "single-type-imported or fully qualified `RestTemplate` receivers: a local, parameter or own-type field, or `new RestTemplate()`; wildcard imports, inherited or outer-class fields and `RestTemplateBuilder` results are not read, and a URL must be an exact literal";
const JAVA_FEIGN: &str = "single-type-imported or fully qualified `@FeignClient` interfaces with Spring mapping annotations; `Feign.builder()` clients, inherited interface methods and `@RequestLine` contracts are not read, and a service-name or placeholder base resolves only through the system manifest";
const JAVA_WEBCLIENT: &str = "`WebClient` request chains `<client>.<verb>().uri(…)` on a single-type-imported or fully qualified `WebClient` local, parameter or own-type field, or an inline `WebClient.create(…)`/`WebClient.builder()…build()`; the base is read only from an inline literal `create`/`baseUrl` argument, so a field-held client's base is an undeclared runtime base, and a `uri` must be an exact literal";
/// What the Java `java.net.http` client cell reads (AC-0475).
const JAVA_NET_HTTP: &str = "inline `HttpRequest.newBuilder([uri])…build()` chains on a single-type-imported or fully qualified `java.net.http.HttpRequest` whose every link is a builder member; a builder held in a variable and built later is not read, and a URL is a literal only as `URI.create(\"…\")` or `new URI(\"…\")` on a proven `java.net.URI`";
/// What the Java OkHttp client cell reads (AC-0476).
const JAVA_OKHTTP: &str = "inline `new Request.Builder()…build()` chains on an imported or fully qualified `okhttp3.Request.Builder` whose every link is a builder member; a builder held in a variable and built later is not read, and a URL is a literal only as `url(\"…\")`";
/// What the Java Apache HttpClient cell reads (AC-0478).
const JAVA_APACHE: &str = "one-argument `new HttpGet|HttpHead|HttpPost|HttpPut|HttpPatch|HttpDelete|HttpOptions|HttpTrace(…)` of HttpClient 4 or 5, single-type-imported or fully qualified, without a class body; request builders, wildcard imports and a URI set after construction are not read, and a URL is a literal only as a string literal or a proven `URI.create`/`new URI` literal";
/// What the Java core client cell still lacks (AC-0435, AC-0437, AC-0441,
/// AC-0475, AC-0476, AC-0478).
const NO_JAVA_CLIENT: &str = "outbound HTTP client calls are recognized only through Retrofit and OpenFeign interfaces, Spring `RestTemplate` and `WebClient`, JDK `java.net.http` and OkHttp request builders, and Apache HttpClient request constructors";
const NO_EVENTS: &str = "message/event SDK sites are recognized only in TypeScript/JavaScript, Python boto3/kafka-python, Java and Kotlin Spring Kafka, Go AWS SDK/kafka-go and Rust rdkafka";
/// What the Go event SDK cells read (AC-0462).
const GO_EVENTS: &str = "keyed AWS SDK `SendMessageInput`/`ReceiveMessageInput`/`PublishInput` and kafka-go `Writer`/`WriterConfig`/`ReaderConfig` literals on the file's import of the exact path; the client a request is sent through is not read, unkeyed literals are not read, and an identity must be an exact literal (through `aws.String` for AWS)";
/// What the Rust rdkafka cell reads (AC-0469).
const RUST_KAFKA: &str = "import-proven `FutureRecord::to`/`BaseRecord::to` records, and `subscribe` on a `StreamConsumer`/`BaseConsumer` local, parameter or own-struct field whose type is written; an inferred `create()` consumer, turbofish records and `assign` are not read, and a topic must be a string literal";
/// What the Java Spring Kafka cell reads (AC-0459).
const JAVA_KAFKA: &str = "`send` on a single-type-imported or fully qualified `KafkaTemplate` local, parameter or own-type field, and method-level `@KafkaListener` `topics`; subclasses, `sendDefault`, wildcard imports and class-level listeners are not read, and a topic must be an exact literal with no placeholder";
/// What the Python event SDK cells read (AC-0458).
const PY_EVENTS: &str = "registry `pypi` entries: methods on a once-bound or inline selected factory call (`boto3.client(\"sqs\")`, `KafkaProducer(…)`) in their own scope, and constructors; instance attributes and passed-in clients are not read, an identity is followed only within its file, and qualifiers and batch keys are not applied";
/// What the Python SQLAlchemy data model cell reads (AC-0474).
const PY_SQLALCHEMY: &str = "declarative models on an import-proven base (`declarative_base()`, `registry().generate_base()`, `DeclarativeBase`, Flask-SQLAlchemy `db.Model`) with a literal `__tablename__`; imperative `Table()` and `map_imperatively` mappings are not read, and a dynamic table name or an unprovable reference target is a Gap";
const NEXT_API: &str = "pages/api and app-router route handlers under a package declaring next; next.config basePath is not applied and wrapped or re-exported handlers stay Gaps";
const TSX_ONLY: &str = "recorded in .tsx/.js/.jsx files; plain .ts modules are not scanned";

/// Every cell this build reports, compiled-in adapters only.
pub const COVERAGE_MATRIX: &[CoverageCell] = &[
    cell("TypeScript", CORE, Imports, Supported, ""),
    cell("TypeScript", CORE, Calls, Supported, ""),
    cell("TypeScript", "express", HttpEndpoints, Supported, ""),
    cell("TypeScript", "node-http", HttpEndpoints, Supported, ""),
    cell("TypeScript", "next", ClientRoutes, Supported, ""),
    cell("TypeScript", "next", HttpEndpoints, Partial, NEXT_API),
    cell("TypeScript", "react-router", ClientRoutes, Supported, ""),
    cell("TypeScript", "vue-router", ClientRoutes, Supported, ""),
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
    cell("JavaScript", "node-http", HttpEndpoints, Supported, ""),
    cell("JavaScript", "next", ClientRoutes, Supported, ""),
    cell("JavaScript", "next", HttpEndpoints, Partial, NEXT_API),
    cell("JavaScript", "react-router", ClientRoutes, Supported, ""),
    cell("JavaScript", "vue-router", ClientRoutes, Supported, ""),
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
        Partial,
        "route tables are read from script blocks; templates are not parsed, so `<router-link>` targets are not read",
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
    cell("Python", "requests", HttpClientCalls, Partial, PY_CLIENT),
    cell("Python", "httpx", HttpClientCalls, Partial, PY_CLIENT),
    cell("Python", "boto3", EventSites, Partial, PY_EVENTS),
    cell("Python", "kafka-python", EventSites, Partial, PY_EVENTS),
    cell("Python", "sqlalchemy", DataModel, Partial, PY_SQLALCHEMY),
    cell("Go", CORE, Imports, Supported, ""),
    cell("Go", CORE, Calls, Supported, ""),
    cell("Go", "net-http", HttpEndpoints, Supported, ""),
    cell("Go", "chi", HttpEndpoints, Supported, ""),
    cell("Go", "gin", HttpEndpoints, Supported, ""),
    cell("Go", "net-http", HttpClientCalls, Partial, GO_CLIENT),
    cell("Go", "aws-sdk", EventSites, Partial, GO_EVENTS),
    cell("Go", "kafka-go", EventSites, Partial, GO_EVENTS),
    cell("Rust", CORE, Imports, Supported, ""),
    cell("Rust", CORE, Calls, Supported, ""),
    cell("Rust", "axum", HttpEndpoints, Supported, ""),
    cell("Rust", "actix-web", HttpEndpoints, Supported, ""),
    cell("Rust", "reqwest", HttpClientCalls, Supported, ""),
    cell("Rust", "rdkafka", EventSites, Partial, RUST_KAFKA),
    cell("Java", CORE, Types, Supported, ""),
    cell("Java", CORE, Imports, Supported, ""),
    cell("Java", CORE, Calls, Supported, ""),
    cell("Java", "spring-web", HttpEndpoints, Supported, ""),
    cell(
        "Java",
        "jax-rs",
        HttpEndpoints,
        Partial,
        "resource methods inherited from a base class or declared on an interface, \
         sub-resource locators and non-literal paths are explicit route Gaps",
    ),
    cell(
        "Java",
        "jpa",
        DataModel,
        Partial,
        "fields inherited from @MappedSuperclass and @Embeddable types are not merged, \
         and repository methods outside the Spring Data naming vocabulary assert no access",
    ),
    cell("Java", "retrofit", HttpClientCalls, Supported, ""),
    cell("Java", "rest-template", HttpClientCalls, Partial, JAVA_REST),
    cell("Java", "openfeign", HttpClientCalls, Partial, JAVA_FEIGN),
    cell(
        "Java",
        "webclient",
        HttpClientCalls,
        Partial,
        JAVA_WEBCLIENT,
    ),
    cell(
        "Java",
        "java-net-http",
        HttpClientCalls,
        Partial,
        JAVA_NET_HTTP,
    ),
    cell("Java", "okhttp", HttpClientCalls, Partial, JAVA_OKHTTP),
    cell(
        "Java",
        "apache-httpclient",
        HttpClientCalls,
        Partial,
        JAVA_APACHE,
    ),
    cell("Java", CORE, HttpClientCalls, Unsupported, NO_JAVA_CLIENT),
    cell("Java", "spring-kafka", EventSites, Partial, JAVA_KAFKA),
    cell("Kotlin", CORE, Types, Supported, ""),
    cell("Kotlin", CORE, Calls, Supported, ""),
    cell("Kotlin", "spring-web", HttpEndpoints, Supported, ""),
    cell(
        "Kotlin",
        "navigation3",
        ClientRoutes,
        Partial,
        "callbacks passed into entries are recorded as registrations rather than followed at T0, \
         and only same-class back-stack helpers are read",
    ),
    cell("Kotlin", "retrofit", HttpClientCalls, Supported, ""),
    cell("Kotlin", "rest-template", HttpClientCalls, Partial, KT_REST),
    cell(
        "Kotlin",
        "webclient",
        HttpClientCalls,
        Partial,
        KT_WEBCLIENT,
    ),
    cell("Kotlin", "okhttp", HttpClientCalls, Partial, KT_OKHTTP),
    cell(
        "Kotlin",
        "apache-httpclient",
        HttpClientCalls,
        Partial,
        KT_APACHE,
    ),
    cell("Kotlin", "spring-kafka", EventSites, Partial, KT_KAFKA),
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
