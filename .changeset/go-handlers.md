---
"cartograph": patch
---

Go: routes registered on a router passed in as a parameter are recovered, and handlers written as func literals, local closures, receiver method values or wrapper calls resolve to a callable, so Go services get capabilities (#2006, #2007). On agent-bot, endpoints go from 75 to 159 and capabilities from 0 to 16.
