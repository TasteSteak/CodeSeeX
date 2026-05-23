"use strict";

const http = require("node:http");
const { createHttpSmokeServer } = require("./mcp-http-smoke-server");

function createLegacySseSmokeServer() {
  const messageServer = createHttpSmokeServer({ sse: false });
  const clients = new Set();
  const server = http.createServer((req, res) => {
    if (req.method === "GET" && req.url === "/sse") {
      const address = server.address();
      res.writeHead(200, {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-cache",
        Connection: "keep-alive",
      });
      res.write("event: endpoint\ndata: http://127.0.0.1:" + address.port + "/message\n\n");
      clients.add(res);
      req.on("close", () => {
        clients.delete(res);
      });
      return;
    }

    if (req.method === "POST" && req.url === "/message") {
      const proxyRes = {
        writeHead() {},
        end(body) {
          for (const client of clients) {
            client.write("event: message\ndata: " + String(body || "{}") + "\n\n");
          }
          res.writeHead(202, { "Content-Type": "text/plain" });
          res.end("");
        },
      };
      req.url = "/mcp";
      messageServer.emit("request", req, proxyRes);
      return;
    }

    res.writeHead(404, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: "not_found" }));
  });
  return server;
}

if (require.main === module) {
  const port = Number(process.env.PORT || "0") || 0;
  const server = createLegacySseSmokeServer();
  server.listen(port, "127.0.0.1", () => {
    const address = server.address();
    console.log(JSON.stringify({ port: address && address.port }));
  });
}

module.exports = {
  createLegacySseSmokeServer,
};
