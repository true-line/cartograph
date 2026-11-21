---
"cartograph": patch
---

Go repositories now have their tests recovered (#2019): each `func TestXxx(t *testing.T)` is a test case linked to the code it calls and, through `httptest.NewRequest`/`http.NewRequest` with a literal method and path, to the endpoint it exercises with the statuses it checks. `test_arrangements.md`, D1 §1.4 and the flow dossiers list them. Calls between files of one Go package are now followed too, so capabilities reach the rules in those files.
