---
"cartograph": patch
---

A Java Spring `WebClient` base URL that ends in `/` now keeps the slash before a relative request path, so `create("https://host/api/").get().uri("orders")` binds to `/api/orders` (#1380).
