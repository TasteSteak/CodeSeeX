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
pub fn upsert_upstream_base_url(path: &Path, value: &str) -> io::Result<bool> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut document = text.parse::<DocumentMut>().map_err(io::Error::other)?;

    let desired = value.trim();
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
    } else {
        document[TABLE][UPSTREAM_KEY] = toml_edit::value(desired);
    }

    let mut output = document.to_string();
    if !output.ends_with('\n') {
        output.push('\n');
    }
    write_atomic(path, &output)?;
    Ok(true)
}

fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let temp = path.with_extension("toml.codeseex.tmp");
    fs::write(&temp, text)?;
    fs::rename(&temp, path)
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

        let _ = std::fs::remove_dir_all(path.parent().unwrap_or(Path::new(".")));
    }
}
