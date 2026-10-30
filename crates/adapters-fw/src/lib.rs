//! Framework registries: versioned deterministic knowledge mapping framework
//! registration patterns to endpoints (SPEC-00 §3.3) and event SDK call
//! shapes to channels (§3.4). Registries are data, not inference — a
//! language adapter consults them while walking the AST.

pub mod client;
pub mod events;
pub mod observed;
pub mod tsconfig;
pub mod vue;

/// HTTP framework registry for factory-created routers (Express/Fastify).
///
/// An endpoint registration is `receiver.method(route, handler)` where the
/// receiver is a tracked framework object (e.g. a variable bound to
/// `express()` / `express.Router()`) and `method` is in this registry.
pub struct HttpFrameworkRegistry {
    /// npm module whose import marks a file as using this framework.
    pub module_name: &'static str,
    /// Factory call patterns that create a routable object, matched against
    /// the callee text of a variable initializer (e.g. `express`,
    /// `express.Router`).
    pub factories: &'static [&'static str],
    methods: &'static [&'static str],
}

/// Express registry (SPEC-00 M1 proving ground).
pub const EXPRESS: HttpFrameworkRegistry = HttpFrameworkRegistry {
    module_name: "express",
    factories: &["express", "express.Router"],
    methods: &[
        "get", "post", "put", "delete", "patch", "options", "head", "all",
    ],
};

/// Fastify registry. Its route-registration shape is the same deterministic
/// `receiver.method(path, handler)` form as Express, but the receiver must be
/// proven to come from the `fastify` factory.
pub const FASTIFY: HttpFrameworkRegistry = HttpFrameworkRegistry {
    module_name: "fastify",
    factories: &["fastify"],
    methods: &[
        "get", "post", "put", "delete", "patch", "options", "head", "all",
    ],
};

/// Factory-created HTTP framework registries consulted by language adapters.
pub const HTTP_FACTORIES: &[&HttpFrameworkRegistry] = &[&EXPRESS, &FASTIFY];

/// NestJS's decorator registry. These names are only authoritative when the
/// decorator binding is import-proven from `@nestjs/common`.
pub struct NestRegistry {
    /// npm module that owns the decorators.
    pub module_name: &'static str,
    /// Class decorator that supplies the route prefix.
    pub controller: &'static str,
    methods: &'static [(&'static str, &'static str)],
}

/// NestJS controller and HTTP method decorators.
pub const NEST: NestRegistry = NestRegistry {
    module_name: "@nestjs/common",
    controller: "Controller",
    methods: &[
        ("Get", "GET"),
        ("Post", "POST"),
        ("Put", "PUT"),
        ("Delete", "DELETE"),
        ("Patch", "PATCH"),
        ("Options", "OPTIONS"),
        ("Head", "HEAD"),
        ("All", "ALL"),
    ],
};

impl NestRegistry {
    /// Map an import-proven Nest decorator name to its canonical HTTP verb.
    pub fn http_method(&self, decorator: &str) -> Option<&'static str> {
        self.methods
            .iter()
            .find_map(|(name, verb)| (*name == decorator).then_some(*verb))
    }
}

impl HttpFrameworkRegistry {
    /// If `prop` is a route-registration method, returns its canonical
    /// uppercase HTTP verb (`all` maps to `ALL`).
    pub fn http_method(&self, prop: &str) -> Option<String> {
        self.methods
            .contains(&prop)
            .then(|| prop.to_ascii_uppercase())
    }

    /// True when `callee` has a registered factory shape. The language adapter
    /// must separately prove the base identifier is imported from
    /// [`Self::module_name`], so default-import aliases remain deterministic.
    pub fn is_factory(&self, callee: &str) -> bool {
        match callee.split_once('.') {
            None => self.factories.iter().any(|factory| !factory.contains('.')),
            Some((_, member)) => self.factories.iter().any(|factory| {
                factory
                    .split_once('.')
                    .is_some_and(|(_, registered)| registered == member)
            }),
        }
    }
}

/// Maven/Gradle source sets that hold only test code. Their sources never
/// ship, so a JVM adapter emits no product Endpoint from them (#1250).
const JVM_TEST_SOURCE_SETS: &[&str] = &["test", "testFixtures", "integrationTest", "androidTest"];

/// Source-root directories under `src/<set>/` that hold JVM sources.
const JVM_SOURCE_LANGUAGES: &[&str] = &["java", "kotlin", "groovy", "scala"];

/// Whether `path` lies in a Maven/Gradle test source set. The source set is
/// the one its source root names: the first `src/<set>/<language>` triple,
/// else the first `src/<set>` pair. Package directories below that root
/// never name a second source set, so `src/main/java/a/src/test/X.java` is
/// product code. `src/main` and every non-test set are product code.
pub fn in_jvm_test_source_set(path: &str) -> bool {
    let segments: Vec<&str> = path.split(['/', '\\']).collect();
    let root = segments
        .windows(3)
        .find(|triple| triple[0] == "src" && JVM_SOURCE_LANGUAGES.contains(&triple[2]))
        .map(|triple| triple[1])
        .or_else(|| {
            segments
                .windows(2)
                .find(|pair| pair[0] == "src")
                .map(|pair| pair[1])
        });
    root.is_some_and(is_test_source_set)
}

/// Whether a source-set name holds only test code: a fixed Maven/Gradle
/// test set, a Kotlin Multiplatform or Android test set (`jvmTest`,
/// `commonTest`, `androidUnitTest`, `iosSimulatorArm64Test`, …), test
/// fixtures of a target (`jvmTestFixtures`) or an Android variant test set
/// (`testDebug`, `androidTestRelease`) (#1424).
fn is_test_source_set(set: &str) -> bool {
    let variant = |prefix: &str| {
        set.strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_uppercase()))
    };
    JVM_TEST_SOURCE_SETS.contains(&set)
        || (set.len() > "Test".len() && set.ends_with("Test"))
        || set.ends_with("TestFixtures")
        || variant("test")
        || variant("androidTest")
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-0431 (#1250): only test source sets are excluded; a `test` segment
    // that is not a `src/<set>` pair, `src/main`, other sets and package
    // directories named `src/test` below a production source root stay product.
    #[test]
    fn jvm_test_source_sets_are_exactly_the_test_sets() {
        for path in [
            "web/src/test/java/org/x/T.java",
            "src/testFixtures/java/X.java",
            "app/src/integrationTest/kotlin/X.kt",
            "app/src/androidTest/java/X.kt",
            "a\\src\\test\\java\\X.java",
            "src/app/src/test/java/X.java",
            "src/test/X.java",
            "shared/src/commonTest/kotlin/X.kt",
            "shared/src/jvmTest/kotlin/X.kt",
            "app/src/androidUnitTest/kotlin/X.kt",
            "app/src/androidInstrumentedTest/kotlin/X.kt",
            "shared/src/iosSimulatorArm64Test/kotlin/X.kt",
            "lib/src/jvmTestFixtures/kotlin/X.kt",
            "app/src/testDebug/java/X.java",
            "app/src/androidTestRelease/kotlin/X.kt",
        ] {
            assert!(in_jvm_test_source_set(path), "{path}");
        }
        for path in [
            "src/main/java/org/x/test/T.java",
            "test-suite/module/omod/src/main/java/X.java",
            "src/jvmMain/kotlin/X.kt",
            "src/testing/java/X.java",
            "shared/src/commonMain/kotlin/X.kt",
            "app/src/debug/kotlin/X.kt",
            "src/Test/java/X.java",
            "src/contest/java/X.java",
            "src/testdata/java/X.java",
            "test/src/main/java/X.java",
            "web/src/main/java/com/acme/src/test/Controller.java",
            "src/main/kotlin/com/acme/src/test/Controller.kt",
            "src/main/java/a/src/test/java/X.java",
            "X.java",
        ] {
            assert!(!in_jvm_test_source_set(path), "{path}");
        }
    }

    #[test]
    fn express_methods_map_to_verbs() {
        assert_eq!(EXPRESS.http_method("get").as_deref(), Some("GET"));
        assert_eq!(EXPRESS.http_method("all").as_deref(), Some("ALL"));
        // `listen`, `use` and arbitrary methods are not endpoint registrations.
        assert_eq!(EXPRESS.http_method("listen"), None);
        assert_eq!(EXPRESS.http_method("use"), None);
    }

    #[test]
    fn express_factories() {
        assert!(EXPRESS.is_factory("express"));
        assert!(EXPRESS.is_factory("express.Router"));
        assert!(EXPRESS.is_factory("makeExpress"));
        assert!(EXPRESS.is_factory("e.Router"));
        assert!(!EXPRESS.is_factory("express.createApplication"));
    }

    #[test]
    fn fastify_and_nest_registry_shapes() {
        assert!(FASTIFY.is_factory("fastify"));
        assert!(FASTIFY.is_factory("Fastify"));
        assert!(!FASTIFY.is_factory("fastify.Router"));
        assert_eq!(NEST.http_method("Get"), Some("GET"));
        assert_eq!(NEST.http_method("SubscribeMessage"), None);
    }
}
