import { createServer } from "node:http";
createServer((req, res) => {
  const url = new URL(req.url!, "http://127.0.0.1");
  if (req.method === "POST" && url.pathname === "/v0/vouch") { res.end(); }
});
