---
"cartograph": minor
---

Go code now yields business rules: guarded returns and error responses written before a bare `return` (`http.Error(w, "forbidden", http.StatusForbidden)`) are recovered, and D1 §1.5 states them in domain terms ("When the request's method is not “POST”, the request is refused with status 405 (“method not allowed”)"). On true-line/agent-bot, §1.5 goes from no Go rows to 186 stated rules (#2009).
