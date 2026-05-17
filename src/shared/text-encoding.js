const iconv = require("iconv-lite");

// ── Encoding detection ────────────────────────────────────────────

function detectEncoding(buffer, headers = {}) {
  // 1. HTTP Content-Type charset
  const contentType = String(headers["content-type"] || headers.get ? headers.get("content-type") || "" : "");
  const match = contentType.match(/charset=([^;\s]+)/i);
  if (match) {
    const charset = match[1].toLowerCase().replace(/^["']|["']$/g, "");
    return charset;
  }

  // 2. HTML <meta charset>
  if (Buffer.isBuffer(buffer)) {
    const head = buffer.slice(0, 1024).toString("latin1");
    const metaMatch = head.match(/<meta[^>]+charset=["']?([^"'>\s]+)/i);
    if (metaMatch) return metaMatch[1].toLowerCase();
  }

  // 3. BOM sniffing
  if (Buffer.isBuffer(buffer)) {
    if (buffer[0] === 0xFF && buffer[1] === 0xFE) return "utf-16le";
    if (buffer[0] === 0xFE && buffer[1] === 0xFF) return "utf-16be";
    if (buffer[0] === 0xEF && buffer[1] === 0xBB && buffer[2] === 0xBF) return "utf-8";
  }

  return "";
}

function decodeBuffer(buffer, encoding) {
  if (!encoding || encoding === "utf-8" || encoding === "utf8") return buffer.toString("utf8");
  const normalized = encoding.toLowerCase().replace(/_|-/g, "");
  try {
    return iconv.decode(buffer, normalized);
  } catch {
    return buffer.toString("utf8");
  }
}

// ── Mojibake repair ────────────────────────────────────────────────

// GBK common chars that produce recognizable garbage when double-encoded as UTF-8
const GBK_MOJIBAKE_CHARS = new Set("鎼妯绋杩鐢闄瀷搴鏂湪涓乏绔鍚诲懡潰枃欢綔锝".split(""));
// Big5 common chars (traditional Chinese) double-encoded
const BIG5_MOJIBAKE_CHARS = new Set("隞亙峕撟潛𧋦".split("").concat([
  "\u96A0", "\u871C", "\u795E", "\u8A2D", "\u5099", "\u5546", "\u5E97"
].map(s => String.fromCharCode(s.charCodeAt(0))))); // placeholder
// Shift-JIS common chars
const SHIFT_JIS_MOJIBAKE_PATTERN = /[\u0080-\u00FF]{3,}/g;
// EUC-KR common chars
const EUC_KR_MOJIBAKE_CHARS = new Set("뗐럊룙쒵샗".split(""));

function repairMojibakeText(value) {
  const text = String(value || "");
  if (!looksLikeMojibake(text)) return text;
  const repaired = Buffer.from(iconv.encode(text, "gbk")).toString("utf8");
  return scoreMojibake(repaired) < scoreMojibake(text) ? repaired : text;
}

function repairBig5MojibakeText(value) {
  const text = String(value || "");
  if (!looksLikeBig5Mojibake(text)) return text;
  try {
    const repaired = Buffer.from(iconv.encode(text, "big5")).toString("utf8");
    return scoreBig5Mojibake(repaired) < scoreBig5Mojibake(text) ? repaired : text;
  } catch {
    return text;
  }
}

function repairAnyMojibake(value) {
  let text = String(value || "");
  text = repairMojibakeText(text);
  text = repairBig5MojibakeText(text);
  // Shift-JIS detection: if high ratio of latin-1 bytes suggests double-encoding
  if (looksLikeShiftJISMojibake(text)) {
    try {
      const repaired = Buffer.from(iconv.encode(text, "shift_jis")).toString("utf8");
      if (scoreShiftJISMojibake(repaired) < scoreShiftJISMojibake(text)) text = repaired;
    } catch {}
  }
  return text;
}

function looksLikeMojibake(text) {
  return scoreMojibake(text) >= 2;
}

function looksLikeBig5Mojibake(text) {
  return scoreBig5Mojibake(text) >= 2;
}

function looksLikeShiftJISMojibake(text) {
  return scoreShiftJISMojibake(text) >= 2;
}

function scoreMojibake(text) {
  const source = String(text || "");
  let score = 0;
  score += (source.match(/[�锟]/g) || []).length * 3;
  // Use Set for lookup speed
  for (let i = 0; i < source.length; i++) {
    if (GBK_MOJIBAKE_CHARS.has(source[i])) score += 1;
  }
  // Consecutive mojibake chars
  let run = 0;
  for (let i = 0; i < source.length; i++) {
    if (GBK_MOJIBAKE_CHARS.has(source[i])) {
      run += 1;
      if (run >= 2) score += 2;
    } else {
      run = 0;
    }
  }
  return score;
}

function scoreBig5Mojibake(text) {
  const source = String(text || "");
  let score = 0;
  score += (source.match(/[�]/g) || []).length * 3;
  // Big5 double-encoding produces specific Unicode ranges
  for (let i = 0; i < source.length; i++) {
    const cp = source.charCodeAt(i);
    if (cp >= 0x5000 && cp <= 0x9FFF && /[\u4E00-\u9FFF]/.test(source[i])) {
      score += 1;
    }
  }
  return score;
}

function scoreShiftJISMojibake(text) {
  const source = String(text || "");
  if (source.length < 10) return 0;
  // Shift-JIS double-encoding typically produces byte values in 0x80-0xFF range
  // as individual characters like Ã, Â, etc.
  const overhead = (source.match(/[\u0080-\u00FF]/g) || []).length;
  if (overhead / source.length > 0.25) return 3;
  return 0;
}

module.exports = {
  detectEncoding,
  decodeBuffer,
  repairMojibakeText,
  repairBig5MojibakeText,
  repairAnyMojibake,
};
