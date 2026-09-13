//! Read/write access to Codex's own `config.toml`.
//!
//! CodeSeeX keeps the upstream override in a `[codeseex]` table there instead of
//! in its own config, so the address lives next to the Codex provider it
//! complements. Writes go through `toml_edit` so the rest of Codex's file -
//! comments, ordering and formatting - is preserved.

use std::fs;
use std::io;
use std::path::Path;

use toml_edit::DocumentMut;

/// Table CodeSeeX owns inside Codex's `config.toml`.
pub const TABLE: &str = "codeseex";
/// Key holding the upstream base URL inside [`TABLE`].
pub const UPSTREAM_KEY: &str = "upstream_base_url";
/// Top-level Codex key naming the model catalog file.
pub const MODEL_CATALOG_KEY: &str = "model_catalog_json";

/// `[codeseex] upstream_base_url` from Codex's config, when it is set.
pub fn read_upstream_base_url() -> Option<String> {
    let path = crate::codex_auth::resolve_codex_config_path()?;
    read_upstream_base_url_from(&path)
}

/// Parses `[codeseex] upstream_base_url` out of a Codex-style `config.toml`.
pub fn read_upstream_base_url_from(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let document: toml::Value = toml::from_str(text).ok()?;
    document
        .get(TABLE)?
        .get(UPSTREAM_KEY)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// `model_catalog_json` from a Codex-style `config.toml`, when it is set.
pub fn read_model_catalog_json_from(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let document: toml::Value = toml::from_str(text).ok()?;
    document
        .get(MODEL_CATALOG_KEY)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Keeps Codex's `model_catalog_json` pointed at the catalog this build writes.
///
/// The caller passes the Codex config path it already resolved, so a test with a
/// throwaway data directory can never rewrite the user's real Codex file. Only an
/// existing value that already names a CodeSeeX catalog file is replaced; a path
/// the user pointed somewhere else is left alone.
pub fn sync_model_catalog_json_at(config_path: &Path, catalog_path: &Path) -> io::Result<bool> {
    let path = config_path;
    let desired = catalog_path.to_string_lossy().to_string();
    let Some(current) = read_model_catalog_json_from(path) else {
        return Ok(false);
    };
    if current == desired || !current.ends_with("model-catalog.json") {
        return Ok(false);
    }

    let text = fs::read_to_string(path).unwrap_or_default();
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut document = text.parse::<DocumentMut>().map_err(io::Error::other)?;
    document[MODEL_CATALOG_KEY] = toml_edit::value(desired);
    let mut output = document.to_string();
    if !output.ends_with('\n') {
        output.push('\n');
    }
    write_atomic(path, &output)?;
    Ok(true)
}

/// Writes `[codeseex] upstream_base_url` into Codex's config. A blank value
/// removes the key again (and the table once it is empty). Returns whether the
/// file changed: an unchanged value is a no-op, so a settings save does not
/// rewrite Codex's config for nothing.
pub fn write_upstream_base_url(value: &str) -> io::Result<bool> {
    let path = crate::codex_auth::codex_config_write_path().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "Codex home could not be resolved")
    })?;
    upsert_upstream_base_url(&path, value)
}

/// Format-preserving upsert of `[codeseex] upstream_base_url` in `path`.
///
/// A blank value removes the key, and so does the official endpoint: it is the
/// default, so pinning it would only add a redundant line to Codex's config.
pub fn upsert_upstream_base_url(path: &Path, value: &str) -> io::Result<bool> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut document = text.parse::<DocumentMut>().map_err(io::Error::other)?;

    let trimmed = value.trim();
    let normalized = if trimmed.is_empty() {
        String::new()
    } else {
        crate::urls::normalize_base_url(trimmed)
    };
    let desired = if normalized == crate::urls::normalize_base_url("") {
        ""
    } else {
        normalized.as_str()
    };
    let current = document
        .get(TABLE)
        .and_then(|item| item.get(UPSTREAM_KEY))
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let unchanged = match (current, desired.is_empty()) {
        (Some(current), false) => current == desired,
        (None, true) => true,
        _ => false,
    };
    if unchanged {
        return Ok(false);
    }

    if desired.is_empty() {
        if let Some(table) = document.get_mut(TABLE).and_then(|item| item.as_table_mut()) {
            table.remove(UPSTREAM_KEY);
        }
        let table_is_empty = document
            .get(TABLE)
            .and_then(|item| item.as_table())
            .is_some_and(|table| table.is_empty());
        if table_is_empty {
            document.remove(TABLE);
        }
        let mut output = document.to_string();
        if !output.ends_with('\n') {
            output.push('\n');
        }
        write_atomic(path, &output)?;
        return Ok(true);
    }

    if document.get(TABLE).is_some() {
        // The table already exists somewhere in the file: update it where it is.
        document[TABLE][UPSTREAM_KEY] = toml_edit::value(desired);
        let mut output = document.to_string();
        if !output.ends_with('\n') {
            output.push('\n');
        }
        write_atomic(path, &output)?;
        return Ok(true);
    }

    // Brand-new table: place it next to the Codex provider it complements rather
    // than appending after whatever tables the user keeps below.
    let block = codeseex_table_block(desired);
    let offset = codeseex_insert_offset(text);
    let mut output = String::with_capacity(text.len() + block.len() + 4);
    output.push_str(&text[..offset]);
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    output.push('\n');
    output.push_str(&block);
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(&text[offset..]);
    // The spliced text must still parse before it replaces the file.
    output.parse::<DocumentMut>().map_err(io::Error::other)?;
    write_atomic(path, &output)?;
    Ok(true)
}

fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let temp = path.with_extension("toml.codeseex.tmp");
    fs::write(&temp, text)?;
    fs::rename(&temp, path)
}

/// Serialized `[codeseex]` table for `value`, produced by the same editor that
/// formats an in-place update so quoting and spacing always match.
fn codeseex_table_block(value: &str) -> String {
    let literal = toml_edit::value(value)
        .as_value()
        .map(|value| value.to_string())
        .unwrap_or_default();
    format!("[{TABLE}]\n{UPSTREAM_KEY} = {literal}\n")
}

/// Where a brand-new `[codeseex]` table belongs.
///
/// Appending at the end of the file is wrong: a user's config can keep hundreds
/// of unrelated tables after their provider block. The override goes right after
/// the last `[model_providers.*]` table it complements; without one it goes after
/// the top-level keys, before the first table header.
fn codeseex_insert_offset(text: &str) -> usize {
    let mut offset = 0usize;
    let mut first_table: Option<usize> = None;
    let mut provider_end: Option<usize> = None;
    let mut inside_provider = false;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with('[') {
            if first_table.is_none() {
                first_table = Some(offset);
            }
            if inside_provider {
                provider_end = Some(offset);
                inside_provider = false;
            }
            if line.trim_start().starts_with("[model_providers.") {
                inside_provider = true;
            }
        }
        offset += line.len();
    }
    if inside_provider {
        provider_end = Some(text.len());
    }
    provider_end.or(first_table).unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_config(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codeseex-codex-config-{label}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir.join("config.toml")
    }

    #[test]
    fn reads_upstream_and_ignores_blank_or_missing() {
        let path = temp_config("read");
        std::fs::write(
            &path,
            "model_provider = \"custom\"\n\n[codeseex]\nupstream_base_url = \"https://relay.example.com/v1\"\n",
        )
        .expect("write");
        assert_eq!(
            read_upstream_base_url_from(&path).as_deref(),
            Some("https://relay.example.com/v1")
        );

        std::fs::write(&path, "[codeseex]\nupstream_base_url = \"   \"\n").expect("write");
        assert_eq!(read_upstream_base_url_from(&path), None);

        std::fs::write(&path, "model = \"deepseek-flash\"\n").expect("write");
        assert_eq!(read_upstream_base_url_from(&path), None);

        let _ = std::fs::remove_dir_all(path.parent().unwrap_or(Path::new(".")));
    }

    /// A dev build and a packaged build keep the catalog in different data
    /// directories, so Codex's recorded path has to follow this build's catalog.
    #[test]
    fn sync_model_catalog_json_follows_this_builds_catalog() {
        let path = temp_config("catalog-sync");
        std::fs::write(
            &path,
            "model = \"deepseek-flash\"\nmodel_catalog_json = 'C:\\old\\model-catalog.json'\n\n[model_providers.custom]\nname = \"x\"\n",
        )
        .expect("write");

        let changed = sync_model_catalog_json_at(&path, Path::new(r"C:\new\model-catalog.json"))
            .expect("sync");
        assert!(changed);
        assert_eq!(
            read_model_catalog_json_from(&path).as_deref(),
            Some(r"C:\new\model-catalog.json")
        );
        // Stable when it already points at this catalog.
        assert!(
            !sync_model_catalog_json_at(&path, Path::new(r"C:\new\model-catalog.json"))
                .expect("sync")
        );

        // A path the user aimed at their own file is never rewritten.
        std::fs::write(&path, "model_catalog_json = 'D:\\mine\\own.json'\n").expect("write");
        assert!(
            !sync_model_catalog_json_at(&path, Path::new(r"C:\new\model-catalog.json"))
                .expect("sync")
        );
        assert_eq!(
            read_model_catalog_json_from(&path).as_deref(),
            Some(r"D:\mine\own.json")
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap_or(Path::new(".")));
    }

    /// The rest of Codex's file must survive an upstream edit untouched, and a
    /// repeated save with the same value must not rewrite it at all.
    #[test]
    fn upsert_preserves_the_rest_of_the_file() {
        let path = temp_config("upsert");
        std::fs::write(
            &path,
            "# keep me\nmodel = \"deepseek-flash\"\n\n[projects.'c:\\work']\ntrust_level = \"trusted\"\n",
        )
        .expect("write");

        assert!(upsert_upstream_base_url(&path, "https://relay.example.com/v1").expect("upsert"));
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("# keep me"));
        assert!(text.contains("[projects.'c:\\work']"));
        // No provider table: the override lands after the top-level keys, before
        // the first table, not at the end of the file.
        assert!(
            text.find("upstream_base_url").expect("upstream key")
                < text.find("[projects.'c:\\work']").expect("projects table"),
            "{text}"
        );
        assert_eq!(
            read_upstream_base_url_from(&path).as_deref(),
            Some("https://relay.example.com/v1")
        );

        // Re-saving the same value is a no-op.
        assert!(!upsert_upstream_base_url(&path, "https://relay.example.com/v1").expect("upsert"));

        // A blank value removes the key and the now-empty table.
        assert!(upsert_upstream_base_url(&path, "   ").expect("clear"));
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(!text.contains("[codeseex]"));
        assert!(text.contains("# keep me"));

        // The official endpoint is the default, so it is never written either.
        assert!(!upsert_upstream_base_url(&path, "https://api.deepseek.com").expect("official"));
        assert!(!std::fs::read_to_string(&path)
            .expect("read")
            .contains("[codeseex]"));
        // A path on the official host normalises to the same default.
        assert!(
            !upsert_upstream_base_url(&path, "https://api.deepseek.com/v1").expect("official v1")
        );
        assert!(!std::fs::read_to_string(&path)
            .expect("read")
            .contains("[codeseex]"));

        let _ = std::fs::remove_dir_all(path.parent().unwrap_or(Path::new(".")));
    }

    /// A brand-new table goes next to the Codex provider it complements, not at
    /// the end of a file that keeps unrelated tables below.
    #[test]
    fn upsert_places_the_new_table_after_the_provider_block() {
        let path = temp_config("placement");
        std::fs::write(
            &path,
            "model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"DeepSeek\"\nbase_url = \"http://127.0.0.1:8787/v1\"\n\n[projects.'c:\\work']\ntrust_level = \"trusted\"\n",
        )
        .expect("write");

        assert!(upsert_upstream_base_url(&path, "https://relay.example.com/v1").expect("upsert"));
        let text = std::fs::read_to_string(&path).expect("read");
        let provider = text
            .find("[model_providers.custom]")
            .expect("provider table");
        let codeseex = text.find("[codeseex]").expect("codeseex table");
        let projects = text.find("[projects.'c:\\work']").expect("projects table");
        assert!(provider < codeseex && codeseex < projects, "{text}");
        assert!(
            text.contains("base_url = \"http://127.0.0.1:8787/v1\""),
            "{text}"
        );
        assert_eq!(
            read_upstream_base_url_from(&path).as_deref(),
            Some("https://relay.example.com/v1")
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap_or(Path::new(".")));
    }
}
