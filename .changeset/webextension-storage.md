---
"cartograph": minor
---

Observed infrastructure from TypeScript and JavaScript now recovers WebExtension storage: `chrome.storage.sync`, `browser.storage.local` and the other areas each become an observed resource, including through `webextension-polyfill` (#734).
