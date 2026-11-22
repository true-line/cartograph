---
"cartograph": minor
---

TypeScript calls through an interface, such as an RPC proxy typed `ExtHostUrlsShape` or an injected `ILogService`, now link to the method on each class that implements the interface instead of staying an open gap. The links are marked inferred, name the interface and implementation, and never point product code at test mocks (#258, AC-1515).
