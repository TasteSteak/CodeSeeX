"use strict";

const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const {
  DEFAULT_AVAILABLE_PLANS,
  buildCodeSeeXCatalog,
  validateCodeSeeXCatalog,
} = require("../src/codex/model-catalog");

const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "codeseex-catalog-seed-"));

try {
  const result = buildCodeSeeXCatalog({
    nativeCatalog: null,
    nativeError: new Error("forced native catalog miss"),
    rootDir: tempRoot,
    allowFallback: true,
  });

  const validation = validateCodeSeeXCatalog(result.catalog);
  assert.equal(result.source, "seed");
  assert.ok(["gpt-5.4", "gpt-5.5", "gpt-5.2", "target-seed"].includes(result.baseModel), "seed must use a supported full catalog base or target-only seed");
  assert.deepEqual(result.targetModels, ["deepseek-v4-flash", "deepseek-v4-pro"]);
  assert.equal(validation.ok, true, validation.error);
  assert.ok(Buffer.byteLength(JSON.stringify(result.catalog)) > 50000, "packaged catalog seed must not collapse to the tiny fallback template");
  for (const model of result.catalog.models) {
    assert.equal(typeof model.priority, "number", model.slug + " must include Codex catalog priority");
    assert.equal(model.supported_in_api, true, model.slug + " must be visible to Codex");
    assertDesktopModelShape(model);
  }

  const nativeWithoutPriority = {
    models: [
      {
        slug: "gpt-5.4",
        display_name: "gpt-5.4",
        description: "Base model without priority, used to guard schema backfill.",
        visibility: "list",
        supported_in_api: true,
        context_window: 272000,
        max_context_window: 1000000,
        available_in_plans: ["plus"],
        base_instructions: "You are Codex, a coding agent based on GPT-5.",
      },
    ],
  };
  const generatedFromSparseNative = buildCodeSeeXCatalog({
    nativeCatalog: nativeWithoutPriority,
    allowSeed: false,
    allowFallback: false,
  });
  for (const model of generatedFromSparseNative.catalog.models) {
    assert.equal(typeof model.priority, "number", model.slug + " must backfill missing priority from native catalogs");
    assert.equal(model.shell_type, "shell_command", model.slug + " must backfill Codex shell type");
    assert.equal(model.apply_patch_tool_type, "freeform", model.slug + " must keep native apply_patch support");
    assert.equal(model.web_search_tool_type, "text_and_image", model.slug + " must keep native web search support");
    assertDesktopModelShape(model);
  }
  assert.equal(validateCodeSeeXCatalog(generatedFromSparseNative.catalog).ok, true);

  const targetCatalogWithBrokenDesktopFields = {
    models: [
      {
        slug: "deepseek-v4-flash",
        display_name: "",
        displayName: "",
        description: "Broken target seed entry for display metadata hardening.",
        visibility: "list",
        supported_in_api: true,
        context_window: 1000000,
        max_context_window: 1000000,
        available_in_plans: ["pro"],
        base_instructions: "You are Codex, a coding agent based on DeepSeek-V4.",
      },
      {
        slug: "deepseek-v4-pro",
        display_name: "Wrong inherited name",
        displayName: "Wrong inherited name",
        description: "Broken target seed entry for display metadata hardening.",
        visibility: "list",
        supported_in_api: true,
        context_window: 1000000,
        max_context_window: 1000000,
        available_in_plans: ["team"],
        base_instructions: "You are Codex, a coding agent based on DeepSeek-V4.",
      },
    ],
  };
  const brokenTargetSeedPath = path.join(tempRoot, "broken-target-seed.json");
  fs.writeFileSync(brokenTargetSeedPath, JSON.stringify(targetCatalogWithBrokenDesktopFields), "utf8");
  const generatedFromBrokenTargetCatalog = buildCodeSeeXCatalog({
    nativeCatalog: null,
    seedCatalogPath: brokenTargetSeedPath,
    nativeError: new Error("forced native catalog miss"),
    allowFallback: true,
  });
  assert.equal(validateCodeSeeXCatalog(generatedFromBrokenTargetCatalog.catalog).ok, true);
  for (const model of generatedFromBrokenTargetCatalog.catalog.models) {
    assertDesktopModelShape(model);
  }

  const targetOnlySeedPath = path.join(tempRoot, "target-only-seed.json");
  fs.writeFileSync(targetOnlySeedPath, JSON.stringify(result.catalog), "utf8");
  const generatedFromTargetSeed = buildCodeSeeXCatalog({
    nativeCatalog: null,
    nativeError: new Error("forced native catalog miss"),
    seedCatalogPath: targetOnlySeedPath,
    rootDir: path.join(tempRoot, "empty-root"),
    allowFallback: true,
  });
  assert.equal(generatedFromTargetSeed.source, "seed");
  assert.equal(generatedFromTargetSeed.baseModel, "target-seed");
  assert.equal(validateCodeSeeXCatalog(generatedFromTargetSeed.catalog).ok, true);
  for (const model of generatedFromTargetSeed.catalog.models) {
    assertDesktopModelShape(model);
  }

  const generatedPath = path.join(tempRoot, "generated-catalog.json");
  buildCodeSeeXCatalog({
    outputPath: generatedPath,
    nativeCatalog: null,
    nativeError: new Error("forced native catalog miss"),
    rootDir: tempRoot,
    allowFallback: true,
  });
  assertCodexCliCanReadCatalog(generatedPath);
  assertBuildCatalogSeedReusesPackagedSeedWithoutNative();
} finally {
  fs.rmSync(tempRoot, { recursive: true, force: true });
}

console.log("catalog seed tests passed");

function assertDesktopModelShape(model) {
  const expectedDisplayNames = {
    "deepseek-v4-flash": "DeepSeek-V4 Flash",
    "deepseek-v4-pro": "DeepSeek-V4 Pro",
  };
  assert.equal(typeof model.id, "string", model.slug + " must include desktop id");
  assert.equal(typeof model.model, "string", model.slug + " must include desktop model");
  assert.equal(typeof model.displayName, "string", model.slug + " must include desktop displayName");
  assert.equal(model.displayName, expectedDisplayNames[model.slug], model.slug + " must use CodeSeeX displayName");
  assert.equal(model.display_name, expectedDisplayNames[model.slug], model.slug + " must use CodeSeeX display_name");
  assert.equal(typeof model.description, "string", model.slug + " must include description");
  assert.equal(typeof model.hidden, "boolean", model.slug + " must include desktop hidden flag");
  assert.equal(typeof model.isDefault, "boolean", model.slug + " must include desktop isDefault flag");
  assert.equal(typeof model.supportsPersonality, "boolean", model.slug + " must include desktop supportsPersonality flag");
  assert.ok(Array.isArray(model.supportedReasoningEfforts), model.slug + " must include desktop supportedReasoningEfforts");
  assert.ok(model.supportedReasoningEfforts.length > 0, model.slug + " must expose reasoning efforts");
  for (const effort of model.supportedReasoningEfforts) {
    assert.equal(typeof effort.reasoningEffort, "string", model.slug + " reasoning effort must use desktop field names");
    assert.equal(typeof effort.description, "string", model.slug + " reasoning effort must include description");
  }
  assert.ok(Array.isArray(model.service_tiers), model.slug + " must include raw service_tiers");
  assert.ok(Array.isArray(model.serviceTiers), model.slug + " must include desktop serviceTiers");
  assert.ok(Array.isArray(model.additional_speed_tiers), model.slug + " must include raw speed tiers");
  assert.ok(Array.isArray(model.additionalSpeedTiers), model.slug + " must include desktop speed tiers");
  assert.ok(Array.isArray(model.input_modalities), model.slug + " must include raw input_modalities");
  assert.ok(Array.isArray(model.available_in_plans), model.slug + " must include available_in_plans");
  for (const plan of DEFAULT_AVAILABLE_PLANS) {
    assert.ok(model.available_in_plans.includes(plan), model.slug + " must include plan " + plan);
  }
}

function assertCodexCliCanReadCatalog(catalogPath) {
  const command = resolveCodexCommand();
  if (!command) return;
  const configHome = fs.mkdtempSync(path.join(tempRoot, "codex-home-"));
  fs.writeFileSync(path.join(configHome, "config.toml"), [
    "model_provider = \"custom\"",
    "model = \"deepseek-v4-pro\"",
    "disable_response_storage = true",
    "model_reasoning_effort = \"xhigh\"",
    "model_catalog_json = " + tomlLiteral(catalogPath),
    "",
    "[model_providers.custom]",
    "name = \"DeepSeek\"",
    "wire_api = \"responses\"",
    "requires_openai_auth = true",
    "base_url = \"http://127.0.0.1:8787/v1\"",
  ].join("\n") + "\n", "utf8");
  const invocation = process.platform === "win32" && /\.(?:cmd|bat)$/i.test(command)
    ? { command: process.env.ComSpec || "cmd.exe", args: ["/d", "/c", "call", command] }
    : { command, args: [] };
  const stdout = execFileSync(invocation.command, invocation.args.concat(["debug", "models"]), {
    encoding: "utf8",
    env: { ...process.env, CODEX_HOME: configHome },
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
    maxBuffer: 16 * 1024 * 1024,
    timeout: 15000,
  });
  const catalog = JSON.parse(String(stdout || "").replace(/^\uFEFF/, ""));
  const slugs = Array.isArray(catalog.models) ? catalog.models.map((model) => model && model.slug) : [];
  assert.ok(slugs.includes("deepseek-v4-flash"), "Codex CLI must read deepseek-v4-flash from generated catalog");
  assert.ok(slugs.includes("deepseek-v4-pro"), "Codex CLI must read deepseek-v4-pro from generated catalog");
}

function assertBuildCatalogSeedReusesPackagedSeedWithoutNative() {
  const seedPath = path.join(__dirname, "..", "src", "codex", "catalog-seed.c6");
  if (!fs.existsSync(seedPath)) return;
  const before = fs.readFileSync(seedPath);
  execFileSync(process.execPath, [path.join(__dirname, "..", "src", "codex", "build-catalog-seed.js")], {
    cwd: path.join(__dirname, ".."),
    encoding: "utf8",
    env: {
      ...process.env,
      CODESEEX_CATALOG_SKIP_PRIVATE: "1",
      CODESEEX_CATALOG_SKIP_NATIVE: "1",
    },
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
    timeout: 15000,
  });
  const after = fs.readFileSync(seedPath);
  assert.equal(after.compare(before), 0, "build:catalog-seed must not degrade the packaged seed when private/native catalogs are unavailable");
}

function resolveCodexCommand() {
  const candidates = [];
  if (process.platform === "win32") {
    if (process.env.LOCALAPPDATA) {
      candidates.push(path.join(process.env.LOCALAPPDATA, "OpenAI", "Codex", "bin", "codex.exe"));
    }
    if (process.env.APPDATA) {
      candidates.push(path.join(process.env.APPDATA, "npm", "codex.cmd"));
    }
  }
  candidates.push("codex");
  for (const candidate of candidates) {
    try {
      if (candidate === "codex" || fs.statSync(candidate).isFile()) {
        if (canRunCodexCommand(candidate)) return candidate;
      }
    } catch {}
  }
  return "";
}

function canRunCodexCommand(command) {
  const { spawnSync } = require("node:child_process");
  const invocation = process.platform === "win32" && /\.(?:cmd|bat)$/i.test(command)
    ? { command: process.env.ComSpec || "cmd.exe", args: ["/d", "/c", "call", command] }
    : { command, args: [] };
  const result = spawnSync(invocation.command, invocation.args.concat(["--version"]), {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
    timeout: 5000,
  });
  return result.status === 0;
}

function tomlLiteral(value) {
  return "'" + String(value || "").replace(/\\/g, "/").replace(/'/g, "''") + "'";
}
