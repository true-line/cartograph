import { createAuth0Client } from "@auth0/auth0-spa-js";

export function login() {
  return createAuth0Client({ domain: "shop.eu.auth0.com", clientId: "abc" });
}
