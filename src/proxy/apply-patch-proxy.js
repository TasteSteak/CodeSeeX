const fs = require("node:fs");
const path = require("node:path");

function executeApplyPatchProxy(patch, options = {}) {
  const started = Date.now();
  try {
    const rootDir = path.resolve(options.rootDir || process.cwd());
    const operations = parsePatch(patch);
    const summaries = [];
    for (const operation of operations) {
      summaries.push(applyOperation(operation, rootDir));
    }
    return {
      ok: true,
      output: renderSuccess(summaries),
      summaries,
      duration_seconds: roundDuration(started),
    };
  } catch (error) {
    return {
      ok: false,
      output: "Patch failed: " + (error && error.message ? error.message : String(error)),
      duration_seconds: roundDuration(started),
    };
  }
}

function parsePatch(patch) {
  const lines = normalizePatchText(patch).split("\n");
  while (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  if (lines[0] !== "*** Begin Patch") throw new Error("patch must start with *** Begin Patch");
  if (lines[lines.length - 1] !== "*** End Patch") throw new Error("patch must end with *** End Patch");

  const operations = [];
  let index = 1;
  while (index < lines.length - 1) {
    const line = lines[index];
    if (line.startsWith("*** Add File: ")) {
      const filePath = line.slice("*** Add File: ".length).trim();
      index += 1;
      const contentLines = [];
      while (index < lines.length - 1 && !isOperationHeader(lines[index])) {
        if (!lines[index].startsWith("+")) throw new Error("add file lines must start with + for " + filePath);
        contentLines.push(lines[index].slice(1));
        index += 1;
      }
      operations.push({ type: "add", path: filePath, content: contentLines.join("\n") + (contentLines.length > 0 ? "\n" : "") });
      continue;
    }

    if (line.startsWith("*** Delete File: ")) {
      operations.push({ type: "delete", path: line.slice("*** Delete File: ".length).trim() });
      index += 1;
      continue;
    }

    if (line.startsWith("*** Update File: ")) {
      const filePath = line.slice("*** Update File: ".length).trim();
      index += 1;
      let moveTo = null;
      const hunkLines = [];
      while (index < lines.length - 1 && !isOperationHeader(lines[index])) {
        if (lines[index].startsWith("*** Move to: ")) {
          moveTo = lines[index].slice("*** Move to: ".length).trim();
        } else if (lines[index] !== "*** End of File") {
          hunkLines.push(lines[index]);
        }
        index += 1;
      }
      operations.push({ type: "update", path: filePath, moveTo, hunks: parseHunks(hunkLines) });
      continue;
    }

    if (!line.trim()) {
      index += 1;
      continue;
    }
    throw new Error("unsupported patch line: " + line);
  }

  if (operations.length === 0) throw new Error("patch contains no operations");
  return operations;
}

function parseHunks(lines) {
  const hunks = [];
  let current = null;
  for (const line of lines) {
    if (line.startsWith("@@")) {
      if (current) hunks.push(current);
      current = { header: line.slice(2).trim(), lines: [] };
      continue;
    }
    if (!current) current = { header: "", lines: [] };
    current.lines.push(line);
  }
  if (current) hunks.push(current);
  return hunks;
}

function applyOperation(operation, rootDir) {
  if (operation.type === "add") return applyAdd(operation, rootDir);
  if (operation.type === "delete") return applyDelete(operation, rootDir);
  return applyUpdate(operation, rootDir);
}

function applyAdd(operation, rootDir) {
  const target = resolvePatchPath(operation.path, rootDir);
  if (fs.existsSync(target)) throw new Error("file already exists: " + operation.path);
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, operation.content, "utf8");
  return { action: "A", path: operation.path, added: countLines(operation.content), deleted: 0 };
}

function applyDelete(operation, rootDir) {
  const target = resolvePatchPath(operation.path, rootDir);
  if (!fs.existsSync(target)) throw new Error("file does not exist: " + operation.path);
  const content = fs.readFileSync(target, "utf8");
  fs.unlinkSync(target);
  return { action: "D", path: operation.path, added: 0, deleted: countLines(content) };
}

function applyUpdate(operation, rootDir) {
  const source = resolvePatchPath(operation.path, rootDir);
  if (!fs.existsSync(source)) throw new Error("file does not exist: " + operation.path);
  const original = normalizeFileText(fs.readFileSync(source, "utf8"));
  let lines = splitLines(original);
  let added = 0;
  let deleted = 0;
  let cursor = 0;

  for (const hunk of operation.hunks) {
    const oldLines = [];
    const newLines = [];
    for (const line of hunk.lines) {
      if (line.startsWith(" ")) {
        oldLines.push(line.slice(1));
        newLines.push(line.slice(1));
      } else if (line.startsWith("-")) {
        oldLines.push(line.slice(1));
        deleted += 1;
      } else if (line.startsWith("+")) {
        newLines.push(line.slice(1));
        added += 1;
      } else if (line === "\\ No newline at end of file") {
        continue;
      } else {
        throw new Error("invalid update hunk line for " + operation.path + ": " + line);
      }
    }

    const matchIndex = findHunk(lines, oldLines, cursor, hunk.header);
    if (matchIndex < 0) throw new Error("could not find patch context in " + operation.path);
    lines = lines.slice(0, matchIndex).concat(newLines, lines.slice(matchIndex + oldLines.length));
    cursor = matchIndex + newLines.length;
  }

  const outputPath = operation.moveTo ? resolvePatchPath(operation.moveTo, rootDir) : source;
  fs.mkdirSync(path.dirname(outputPath), { recursive: true });
  fs.writeFileSync(outputPath, lines.join("\n") + "\n", "utf8");
  if (operation.moveTo && outputPath !== source) fs.unlinkSync(source);
  return {
    action: operation.moveTo ? "R" : "M",
    path: operation.path,
    move_to: operation.moveTo || undefined,
    added,
    deleted,
  };
}

function findHunk(lines, oldLines, startIndex, header) {
  if (oldLines.length === 0) return Math.min(startIndex, lines.length);
  let found = findSubsequence(lines, oldLines, startIndex);
  if (found >= 0) return found;
  found = findSubsequence(lines, oldLines, 0);
  if (found >= 0) return found;
  // Try stripping BOM from first line (defensive)
  const bomLines = lines.map((line, i) => i === 0 ? line.replace(/^\uFEFF/, "") : line);
  const bomOld = oldLines.map((line, i) => i === 0 ? line.replace(/^\uFEFF/, "") : line);
  found = findSubsequence(bomLines, bomOld, startIndex);
  if (found >= 0) return found;
  found = findSubsequence(bomLines, bomOld, 0);
  if (found >= 0) return found;

  const relaxedOld = oldLines.map((line) => line.trimEnd());
  const relaxedLines = lines.map((line) => line.trimEnd());
  found = findSubsequence(relaxedLines, relaxedOld, startIndex);
  if (found >= 0) return found;
  found = findSubsequence(relaxedLines, relaxedOld, 0);
  if (found >= 0) return found;

  const anchor = String(header || "").trim();
  if (anchor) {
    const anchorIndex = lines.findIndex((line, index) => index >= startIndex && line.includes(anchor));
    if (anchorIndex >= 0) return anchorIndex;
  }
  return -1;
}

function findSubsequence(lines, pattern, startIndex) {
  const start = Math.max(0, Number(startIndex) || 0);
  for (let index = start; index <= lines.length - pattern.length; index += 1) {
    let matched = true;
    for (let offset = 0; offset < pattern.length; offset += 1) {
      if (lines[index + offset] !== pattern[offset]) {
        matched = false;
        break;
      }
    }
    if (matched) return index;
  }
  return -1;
}

function resolvePatchPath(filePath, rootDir) {
  if (!filePath || typeof filePath !== "string") throw new Error("patch path is required");
  const normalized = filePath.replace(/\\/g, path.sep);
  const root = path.resolve(rootDir);
  const target = path.resolve(path.isAbsolute(normalized) ? normalized : path.join(root, normalized));
  if (!isInsidePath(target, root)) throw new Error("patch path escapes workspace: " + filePath);
  return target;
}

function isInsidePath(target, root) {
  const relative = path.relative(root, target);
  return relative === "" || (relative && !relative.startsWith("..") && !path.isAbsolute(relative));
}

function renderSuccess(summaries) {
  const lines = ["Success. Updated the following files:"];
  for (const summary of summaries) {
    const stat = " +" + summary.added + " -" + summary.deleted;
    if (summary.action === "R") lines.push("R " + summary.path + " -> " + summary.move_to + stat);
    else lines.push(summary.action + " " + summary.path + stat);
  }
  return lines.join("\n");
}

function countLines(text) {
  const value = normalizeFileText(text);
  if (!value) return 0;
  return value.endsWith("\n") ? value.split("\n").length - 1 : value.split("\n").length;
}

function splitLines(text) {
  const value = normalizeFileText(text);
  const lines = value.split("\n");
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

function isOperationHeader(line) {
  return line.startsWith("*** Add File: ") || line.startsWith("*** Update File: ") || line.startsWith("*** Delete File: ");
}

function normalizePatchText(value) {
  return String(value || "").replace(/\r\n/g, "\n").replace(/\r/g, "\n");
}

function normalizeFileText(value) {
  return String(value || "").replace(/^\uFEFF/, "").replace(/\r\n/g, "\n").replace(/\r/g, "\n");
}

function roundDuration(started) {
  return Math.round((Date.now() - started) / 10) / 100;
}

module.exports = {
  executeApplyPatchProxy,
  parsePatch,
};
