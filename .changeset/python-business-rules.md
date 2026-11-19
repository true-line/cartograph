---
"cartograph": minor
---

Python code now yields business-rule observations: every guarded `return` and `raise` in a function is recovered with its branch conditions and outcome, sanitized like TypeScript rules. The product specification's §1.5 lists a Flask handler's `raise` and error-status responses (`return jsonify(error="Forbidden"), 403`) under each capability. On SimpleLogin, §1.5 goes from empty to 155 rejections and `rule-evidence.md` from 0 to 2,540 observations.
