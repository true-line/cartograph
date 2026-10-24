---
"cartograph": minor
---

Python recovery now reads pytest tests and links each Flask test-client call to the endpoint it requests, by `url_for` endpoint name or by path, with the statuses the test asserts and the response keys it reads. Calls it cannot resolve become named gaps. The spec bundle gains `test_arrangements.md`, which lists the endpoints tests reference and those with no recovered test reference.
