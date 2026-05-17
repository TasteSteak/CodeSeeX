const fs = require("node:fs");
const path = require("node:path");

const STATIC_DIR = path.join(__dirname, "static");
const INDEX_FILE = path.join(STATIC_DIR, "index.html");

function readIndexPage() {
  return fs.readFileSync(INDEX_FILE, "utf8");
}

function staticFilePath(requestPath) {
  const pathname = requestPath === "/" ? "/index.html" : requestPath;
  const normalized = path.normalize(pathname).replace(/^(\.\.[\\/])+/, "");
  const filePath = path.join(STATIC_DIR, normalized);
  const relative = path.relative(STATIC_DIR, filePath);
  if (relative.startsWith("..") || path.isAbsolute(relative)) return null;
  return filePath;
}

function contentTypeFor(filePath) {
  const ext = path.extname(filePath).toLowerCase();
  if (ext === ".html") return "text/html; charset=utf-8";
  if (ext === ".css") return "text/css; charset=utf-8";
  if (ext === ".js") return "text/javascript; charset=utf-8";
  if (ext === ".json") return "application/json; charset=utf-8";
  if (ext === ".svg") return "image/svg+xml; charset=utf-8";
  return "application/octet-stream";
}

module.exports = {
  STATIC_DIR,
  contentTypeFor,
  readIndexPage,
  staticFilePath,
};
