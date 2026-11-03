const express = require("express");
const app = express();

app.get("/proxy", async (req, res) => {
  const page = req.query.page;
  const r = await fetch(`/api/posts?page=${page}`);
  res.send(await r.text());
});

app.get("/open", async (req, res) => {
  const r = await fetch(req.query.url);  // TP: SSRF
  res.send(await r.text());
});

app.get("/proxy2", async (req, res) => {
  const r = await fetch("//" + req.query.host + "/x");  // TP: protocol-relative host from the request
  res.send(await r.text());
});
