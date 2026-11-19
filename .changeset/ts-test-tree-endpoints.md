---
"cartograph": patch
---

TypeScript and JavaScript test code no longer shows up as the system's own API. Controllers and routes in end-to-end suites, `__tests__`/`__mocks__`/`__fixtures__`/`test`/`tests` directories and `*.spec.ts`/`*.test.ts`/`*.e2e-spec.ts` files yield no product Endpoint, as JVM test source sets already don't. On Vendure `packages/core` this removes 63 of 374 endpoints (e2e fixture plugins such as `TestController` at `GET /run-job`) from the endpoint inventory, flows, capabilities and user stories.
