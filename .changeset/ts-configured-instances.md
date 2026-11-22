---
"cartograph": minor
---

TypeScript: a call through a configured instance (`new ShippingCalculator({ calculate })` reached through the class's `this.calculateFn(…)`) and a call on an instance looked up from a typed field (`this.allCalculators[code].calculate(…)`) are now inferred `CALLS`, so the rules in a configured default object are reached from the code that runs it.
