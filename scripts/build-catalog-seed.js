"use strict";

const fs = require("node:fs");
const path = require("node:path");
const zlib = require("node:zlib");

const {
  PACKAGED_CATALOG_SEED_FILE,
  PRIVATE_CATALOG_DIR,
  codexCliInvocation,
} = require("../src/codex/model-catalog");

const rootDir = path.resolve(__dirname, "..");
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
  if (fs.existsSync(privateJsonPath)) {
    console.log("[CodeSeeX] Reading private catalog source:", privateJsonPath);
    return fs.readFileSync(privateJsonPath, "utf8");
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
    console.warn("[CodeSeeX] Native Codex catalog is unavailable; writing an emergency public seed.");
    console.warn("[CodeSeeX] Release builds should use a private/native catalog seed when possible.");
    return JSON.stringify(emergencySeedCatalog());
  }
}

function emergencySeedCatalog() {
  const instructions = [
    "You are Codex, a coding agent based on DeepSeek-V4 and running through the local CodeSeeX proxy inside the Codex environment.",
    "You and the user share the same workspace and collaborate to achieve the user's goals.",
  ].join(" ");
  return {
    models: [
      {
        slug: "gpt-5.5",
        display_name: "gpt-5.5",
        description: "Emergency CodeSeeX build seed for local catalog generation.",
        default_reasoning_level: "medium",
        supported_reasoning_levels: [
          { effort: "low", description: "Fast responses with lighter reasoning" },
          { effort: "medium", description: "Balances speed and reasoning depth for everyday tasks" },
          { effort: "high", description: "Greater reasoning depth for complex problems" },
          { effort: "xhigh", description: "Extra high reasoning depth for complex problems" },
        ],
        shell_type: "shell_command",
        visibility: "list",
        supported_in_api: true,
        base_instructions: instructions,
        model_messages: {
          instructions_template: instructions + "\n\n{{ personality }}",
          instructions_variables: { personality_default: "" },
        },
        supports_reasoning_summaries: true,
        default_reasoning_summary: "none",
        support_verbosity: true,
        default_verbosity: "low",
        apply_patch_tool_type: "freeform",
        web_search_tool_type: "text_and_image",
        supports_parallel_tool_calls: true,
        supports_image_detail_original: true,
        supports_search_tool: true,
        input_modalities: ["text", "image"],
        context_window: 1000000,
        max_context_window: 1000000,
        effective_context_window_percent: 90,
      },
    ],
  };
}

main();
