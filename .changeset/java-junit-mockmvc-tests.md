---
"cartograph": minor
---

Java JUnit tests are now recovered and linked to the Spring endpoints they call through MockMvc (`mvc.perform(post(ENTITY_API_URL))`), with URL constants resolved across files and the statuses each test expects, so the product specification's acceptance section lists Java test scenarios per capability. On the JHipster sample app: 215 tests, 97 links reaching 37 of 38 endpoints.
