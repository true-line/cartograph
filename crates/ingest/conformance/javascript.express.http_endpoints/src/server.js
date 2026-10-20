import express from "express";
const app = express();
app.get("/users/:id", (req, res) => { res.json({}); });
