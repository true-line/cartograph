//! Syntactic configuration verdict adapter over the shared classifier.
pub(crate) fn binds(path: &str, source: &str, identifier: &str) -> bool {
    super::super::claim_diagnostics::configuration::explain(path, source, identifier).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    // AC-0287: real r3 configuration access form, with unrelated text refused.
    #[test]
    fn binds_destructuring_and_default_identity_without_cooccurrence() {
        assert!(binds(
            "stock-location.service.ts",
            "const { stockLocationStrategy } = this.configService.catalogOptions;",
            "stockLocationStrategy"
        ));
        assert!(binds(
            "default-stock-allocation-strategy.ts",
            "export class DefaultStockAllocationStrategy {}",
            "DefaultStockAllocationStrategy"
        ));
        assert!(binds(
            "config.ts",
            "config.catalog.strategy = new MultiChannelStockLocationStrategy();",
            "MultiChannelStockLocationStrategy"
        ));
        assert!(binds(
            "config.ts",
            "const defaultStrategy = new Strategy();",
            "Strategy"
        ));
        for source in [
            "const config = {}; class Strategy {}",
            "// config.strategy = new Strategy();\nclass Strategy {}",
            "const description = 'config.strategy = new Strategy()'; class Strategy {}",
            "const { Other } = config.options; class Strategy {}",
            "const value = new Strategy(); const config = {};",
            "config.strategy = unrelated(Strategy);",
            "config.strategy = new Strategy() || fallback;",
            "config.strategy = Strategy.factory();",
            "const defaultStrategy = Strategy ? Other : Third;",
            "config.strategy = new Strategy(options);",
            "/* unclosed config.strategy = Strategy",
        ] {
            assert!(!binds("strategy.ts", source, "Strategy"), "{source}");
        }
        assert!(!binds(
            "stock-location.service.ts",
            "const { stockLocationStrategy } = this.configService.catalogOptions;",
            "MultiChannelStockLocationStrategy"
        ));
        assert!(!binds(
            "multi-channel-stock-location-strategy.ts",
            "class MultiChannelStockLocationStrategy {}",
            "MultiChannelStockLocationStrategy"
        ));
    }
}
