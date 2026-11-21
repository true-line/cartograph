---
"cartograph": minor
---

TypeScript recovery states how a value is chosen: a predicate's answer, a pick with a fallback (`rate || this.defaultTaxRate` with its `.find(r => r.test(zone, taxCategory))`), and a configured strategy's answer are business rules, so D1 and the area briefings say how Vendure picks a tax rate.
