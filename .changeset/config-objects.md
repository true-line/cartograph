---
"cartograph": minor
---

Calls through configuration now reach plain objects as well as classes. An object declared with an interface type (`const defaultPaymentProcess: PaymentProcess = {…}`) or returned by a function declared to return one implements that interface. A loop over configured hooks (`for (const process of processes)`), or a call on a typed class field (`this.config.onTransitionStart`), links to every implementation. On Vendure, a state-machine transition now reaches the default order, payment, fulfillment and refund process rules (#2069).
