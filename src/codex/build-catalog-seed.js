"use strict";

const fs = require("node:fs");
const path = require("node:path");
const zlib = require("node:zlib");

const {
  PACKAGED_CATALOG_SEED_FILE,
  PRIVATE_CATALOG_DIR,
  codexCliInvocation,
} = require("./model-catalog");

const rootDir = path.resolve(__dirname, "..", "..");
const outputPath = path.join(rootDir, "src", "codex", PACKAGED_CATALOG_SEED_FILE);
const privateJsonPath = path.join(rootDir, PRIVATE_CATALOG_DIR, "model-catalog.json");

function main() {
  const catalogText = readCatalogText();
  const parsed = JSON.parse(catalogText);
  if (!parsed || !Array.isArray(parsed.models) || parsed.models.length === 0) {
    throw new Error("Catalog seed source does not contain a non-empty models array.");
  }
  const compressed = zlib.brotliCompressSync(Buffer.from(JSON.stringify(parsed), "utf8"), {
    params: {
      [zlib.constants.BROTLI_PARAM_QUALITY]: 11,
    },
  });
  fs.mkdirSync(path.dirname(outputPath), { recursive: true });
  fs.writeFileSync(outputPath, compressed);
  console.log("[CodeSeeX] Wrote catalog seed:", outputPath);
  console.log("[CodeSeeX] Source models:", parsed.models.map((model) => model && model.slug).filter(Boolean).join(", "));
}

function readCatalogText() {
  const explicitSource = process.env.CODESEEX_CATALOG_SOURCE_FILE;
  if (explicitSource) {
    const sourcePath = path.resolve(explicitSource);
    console.log("[CodeSeeX] Reading explicit catalog source:", sourcePath);
    return fs.readFileSync(sourcePath, "utf8");
  }
  if (!isTruthyEnv(process.env.CODESEEX_CATALOG_SKIP_PRIVATE) && fs.existsSync(privateJsonPath)) {
    console.log("[CodeSeeX] Reading private catalog source:", privateJsonPath);
    return fs.readFileSync(privateJsonPath, "utf8");
  }
  if (isTruthyEnv(process.env.CODESEEX_CATALOG_SKIP_NATIVE)) {
    const existingSeed = readExistingPackagedSeed();
    if (existingSeed) {
      console.warn("[CodeSeeX] Native Codex catalog lookup skipped; reusing existing packaged catalog seed.");
      return existingSeed;
    }
    throw new Error("Native Codex catalog lookup skipped and no existing packaged catalog seed is available.");
  }
  const invocation = codexCliInvocation();
  console.log("[CodeSeeX] Reading native Codex catalog via:", invocation.command, invocation.args.concat(["debug", "models", "--bundled"]).join(" "));
  const { execFileSync } = require("node:child_process");
  try {
    return execFileSync(invocation.command, invocation.args.concat(["debug", "models", "--bundled"]), {
      encoding: "utf8",
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
      maxBuffer: 16 * 1024 * 1024,
      timeout: 15000,
    });
  } catch (error) {
    const existingSeed = readExistingPackagedSeed();
    if (existingSeed) {
      console.warn("[CodeSeeX] Native Codex catalog is unavailable; reusing existing packaged catalog seed.");
      return existingSeed;
    }
    throw new Error("Unable to build catalog seed. Provide CODESEEX_CATALOG_SOURCE_FILE, keep src/codex/catalog-seed.c6 available, or install Codex CLI. Cause: " + (error && error.message ? error.message : String(error)));
  }
}

function readExistingPackagedSeed() {
  try {
    if (!fs.existsSync(outputPath)) return "";
    return zlib.brotliDecompressSync(fs.readFileSync(outputPath)).toString("utf8");
  } catch {
    return "";
  }
}

function isTruthyEnv(value) {
  return /^(1|true|yes|on)$/i.test(String(value || "").trim());
}

main();
