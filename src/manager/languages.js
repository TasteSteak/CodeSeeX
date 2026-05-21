const fs = require("node:fs");
const path = require("node:path");

const SYSTEM_LANGUAGE_ID = "system";
const DEFAULT_LANGUAGE_ID = SYSTEM_LANGUAGE_ID;
const FALLBACK_LANGUAGE_ID = "en_us";
const LANG_DIR = path.join(__dirname, "static", "lang");

function listLanguages(langDir = LANG_DIR) {
  if (Array.isArray(langDir)) return mergeLanguages(langDir);
  try {
    return fs.readdirSync(langDir, { withFileTypes: true })
      .filter((entry) => entry.isFile() && entry.name.toLowerCase().endsWith(".json"))
      .map((entry) => languageFromFile(langDir, entry.name))
      .filter(Boolean)
      .sort((left, right) => left.id.localeCompare(right.id));
  } catch {
    return [];
  }
}

function mergeLanguages(langDirs) {
  const byId = new Map();
  for (const dir of langDirs) {
    for (const language of listLanguages(dir)) byId.set(language.id, language);
  }
  return Array.from(byId.values()).sort((left, right) => left.id.localeCompare(right.id));
}

function languageFilePath(languageId, langDirs = [LANG_DIR]) {
  const id = languageIdFromFilename(languageId + ".json");
  if (!id) return null;
  for (const dir of Array.isArray(langDirs) ? langDirs : [langDirs]) {
    const filePath = path.join(dir, id + ".json");
    if (fs.existsSync(filePath)) return filePath;
  }
  return null;
}

function languageFromFile(langDir, filename) {
  const id = languageIdFromFilename(filename);
  if (!id) return null;
  const filePath = path.join(langDir, filename);
  const pack = readLanguagePack(filePath);
  if (!pack) return null;
  return {
    id,
    name: languageDisplayName(id, pack),
    url: "/lang/" + filename,
  };
}

function languageIdFromFilename(filename) {
  const basename = path.basename(String(filename || ""), ".json").toLowerCase();
  return /^[a-z0-9]+(?:_[a-z0-9]+)*$/.test(basename) ? basename : "";
}

function readLanguagePack(filePath) {
  try {
    const parsed = JSON.parse(fs.readFileSync(filePath, "utf8"));
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

function languageDisplayName(id, pack) {
  return stringField(pack.languageName)
    || stringField(pack.language_name)
    || stringField(pack.name)
    || id;
}

function stringField(value) {
  return typeof value === "string" && value.trim() ? value.trim() : "";
}

module.exports = {
  DEFAULT_LANGUAGE_ID,
  FALLBACK_LANGUAGE_ID,
  LANG_DIR,
  SYSTEM_LANGUAGE_ID,
  languageIdFromFilename,
  languageFilePath,
  listLanguages,
};
