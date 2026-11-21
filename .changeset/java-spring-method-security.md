---
"cartograph": minor
---

Java Spring endpoints now carry who may call them, read from their method-security annotations (`@PreAuthorize`/`@PostAuthorize` SpEL role, authority and sign-in checks, `@Secured`, `@RolesAllowed`, `@PermitAll`, `@DenyAll`, on the handler or its class) and from the `HttpSecurity` URL rules that cover them, with string constants resolved across files. `endpoint-permissions.md` now covers Spring, and the product specification's §1.2 lists each caller. On the JHipster sample app all 38 endpoints are read: 6 public (register, activate, sign-in, password reset), 22 for signed-in users and 10 for `ROLE_ADMIN` (via `AuthoritiesConstants.ADMIN`); before, none were.
