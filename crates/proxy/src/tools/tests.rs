use super::*;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn definitions_follow_enabled_ids() {
    let definitions = upstream_tool_definitions(&["list_directory".to_owned()]);
    let names = definitions
        .iter()
        .filter_map(|definition| definition.pointer("/function/name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["apply_patch", "web_search", "list_directory"]);
}

#[test]
fn vision_analyze_definition_is_enabled_by_tool_id() {
    let definitions = upstream_tool_definitions(&["vision_analyze".to_owned()]);
    let vision = definitions
        .iter()
        .find(|definition| {
            definition.pointer("/function/name").and_then(Value::as_str) == Some("vision_analyze")
        })
        .expect("vision_analyze definition");

    assert_eq!(
        vision
            .pointer("/function/parameters/properties/image/type")
            .and_then(Value::as_str),
        Some("string")
    );
    assert_eq!(
        vision.pointer("/function/parameters/additionalProperties"),
        Some(&Value::Bool(false))
    );
}

#[test]
fn vision_module_exposes_native_image_gen_definition() {
    let definitions = upstream_tool_definitions(&["vision_analyze".to_owned()]);
    assert!(!definitions.iter().any(|definition| {
        definition.pointer("/function/name").and_then(Value::as_str) == Some("vision_generate")
    }));
    let image_gen = definitions
        .iter()
        .find(|definition| {
            definition.pointer("/function/name").and_then(Value::as_str) == Some("image_gen")
        })
        .expect("image_gen definition");

    assert_eq!(
        image_gen
            .pointer("/function/parameters/properties/prompt/type")
            .and_then(Value::as_str),
        Some("string")
    );
    assert!(image_gen
        .pointer("/function/parameters/properties/description")
        .is_none());
    assert!(image_gen
        .pointer("/function/parameters/properties/input")
        .is_none());
    assert!(image_gen
        .pointer("/function/parameters/required")
        .and_then(Value::as_array)
        .is_some_and(|required| required
            .iter()
            .any(|value| value.as_str() == Some("prompt"))));
    assert_eq!(
        image_gen.pointer("/function/parameters/additionalProperties"),
        Some(&Value::Bool(false))
    );
}

#[test]
fn apply_patch_definition_requires_paths_in_operation_headers() {
    let definitions = upstream_tool_definitions(&[]);
    let apply_patch = definitions
        .iter()
        .find(|definition| {
            definition.pointer("/function/name").and_then(Value::as_str) == Some("apply_patch")
        })
        .expect("apply_patch definition");
    let description = apply_patch
        .pointer("/function/parameters/properties/patch/description")
        .and_then(Value::as_str)
        .expect("patch parameter description");

    assert!(description.contains("*** Add File: path"));
    assert!(description.contains("*** Update File: path"));
    assert!(description.contains("*** Delete File: path"));
    assert!(description.contains("first line must be *** Begin Patch"));
    assert!(description.contains("final line must be *** End Patch"));
    assert!(description.contains("bare headers"));
    assert!(description.contains("standalone grammar lines"));
    assert!(description.contains("hunk-prefixed data lines"));
    assert!(description.contains("empty context line"));
    assert!(description.contains("single space"));
}

#[test]
fn executable_tool_checks_enabled_allowlist() {
    let enabled = vec!["list_directory".to_owned()];
    assert!(!is_executable_tool_enabled("apply_patch", &[]));
    assert!(is_executable_tool_enabled("web_search", &[]));
    assert!(is_executable_tool_enabled("list_directory", &enabled));
    assert!(is_executable_tool_enabled(
        "vision_analyze",
        &["vision_analyze".to_owned()]
    ));
    assert!(is_executable_tool_enabled(
        "vision_generate",
        &["vision_analyze".to_owned()]
    ));
    assert!(is_executable_tool_enabled(
        "image_gen",
        &["vision_analyze".to_owned()]
    ));
    assert!(!is_executable_tool_enabled("read_file_range", &enabled));
}

#[test]
fn malformed_tool_arguments_return_invalid_arguments() {
    let result = execute_tool_in_context(
        &ToolExecutionContext::default(),
        "list_directory",
        r#"{"path":"."#,
    );

    assert_eq!(
        result.get("error").and_then(Value::as_str),
        Some("invalid_arguments")
    );
}

#[tokio::test]
async fn vision_analyze_missing_config_returns_unavailable() {
    let data_dir = temp_workspace("vision-analyze-missing-config");
    fs::create_dir_all(&data_dir).expect("create data dir");
    let config = codeseex_core::AppConfig {
        data_dir: data_dir.clone(),
        ..Default::default()
    };
    let result = execute_tool_with_client(
        &reqwest::Client::new(),
        &config,
        &ToolExecutionContext::default(),
        &[],
        &[],
        "vision_analyze",
        r#"{"image_url":"https://example.com/image.png"}"#,
    )
    .await;

    assert_eq!(result.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(
        result.get("tool").and_then(Value::as_str),
        Some("vision_analyze")
    );
    assert_eq!(
        result.get("error").and_then(Value::as_str),
        Some("vision_unavailable")
    );
    let missing = result
        .get("missing_or_invalid")
        .and_then(Value::as_array)
        .expect("missing config");
    assert!(missing
        .iter()
        .any(|value| value.as_str() == Some("VISION_ANALYZE_URL")));
    assert!(missing
        .iter()
        .any(|value| value.as_str() == Some("VISION_ANALYZE_MODEL")));
    assert!(missing
        .iter()
        .any(|value| value.as_str() == Some("VISION_API_KEY")));

    let _ = fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn vision_generate_missing_config_returns_unavailable() {
    let data_dir = temp_workspace("vision-generate-missing-config");
    fs::create_dir_all(&data_dir).expect("create data dir");
    let config = codeseex_core::AppConfig {
        data_dir: data_dir.clone(),
        ..Default::default()
    };
    let result = execute_tool_with_client(
        &reqwest::Client::new(),
        &config,
        &ToolExecutionContext::default(),
        &[],
        &[],
        "vision_generate",
        r#"{"prompt":"A small product photo"}"#,
    )
    .await;

    assert_eq!(result.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(
        result.get("tool").and_then(Value::as_str),
        Some("vision_generate")
    );
    assert_eq!(
        result.get("error").and_then(Value::as_str),
        Some("vision_unavailable")
    );
    let missing = result
        .get("missing_or_invalid")
        .and_then(Value::as_array)
        .expect("missing config");
    assert!(missing
        .iter()
        .any(|value| value.as_str() == Some("VISION_GENERATE_URL")));
    assert!(missing
        .iter()
        .any(|value| value.as_str() == Some("VISION_GENERATE_MODEL")));
    assert!(missing
        .iter()
        .any(|value| value.as_str() == Some("VISION_API_KEY")));

    let _ = fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn image_gen_missing_config_reports_native_tool_name() {
    let data_dir = temp_workspace("image-gen-missing-config");
    fs::create_dir_all(&data_dir).expect("create data dir");
    let config = codeseex_core::AppConfig {
        data_dir: data_dir.clone(),
        ..Default::default()
    };
    let result = execute_tool_with_client(
        &reqwest::Client::new(),
        &config,
        &ToolExecutionContext::default(),
        &[],
        &[],
        "image_gen",
        r#"{"prompt":"A small product photo"}"#,
    )
    .await;

    assert_eq!(result.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(
        result.get("tool").and_then(Value::as_str),
        Some("image_gen")
    );
    assert_eq!(
        result.get("error").and_then(Value::as_str),
        Some("vision_unavailable")
    );

    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn read_file_range_supports_tail_and_text_output() {
    let root = temp_workspace("read-range-tail");
    fs::create_dir_all(&root).expect("create temp workspace");
    fs::write(root.join("notes.txt"), "one\ntwo\nthree\nfour\n").expect("write notes");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let tail = execute_tool_in_context(
        &context,
        "read_file_range",
        r#"{"path":"notes.txt","start":-2}"#,
    );

    assert_eq!(tail.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(tail.get("start").and_then(Value::as_u64), Some(3));
    assert_eq!(tail.get("end").and_then(Value::as_u64), Some(4));
    assert_eq!(
        tail.get("text").and_then(Value::as_str),
        Some("three\nfour")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_supports_explicit_tail_lines() {
    let root = temp_workspace("read-range-tail-lines");
    fs::create_dir_all(&root).expect("create temp workspace");
    fs::write(root.join("notes.txt"), "one\ntwo\nthree\nfour\n").expect("write notes");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let tail = execute_tool_in_context(
        &context,
        "read_file_range",
        r#"{"path":"notes.txt","tail_lines":2}"#,
    );

    assert_eq!(tail.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(tail.get("start").and_then(Value::as_u64), Some(3));
    assert_eq!(tail.get("end").and_then(Value::as_u64), Some(4));
    assert_eq!(
        tail.get("text").and_then(Value::as_str),
        Some("three\nfour")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_preserves_negative_line_range_semantics() {
    let root = temp_workspace("read-range-negative-window");
    fs::create_dir_all(&root).expect("create temp workspace");
    fs::write(root.join("notes.txt"), "one\ntwo\nthree\nfour\nfive\nsix\n").expect("write notes");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let from_end = execute_tool_in_context(
        &context,
        "read_file_range",
        r#"{"path":"notes.txt","start":-4,"count":2}"#,
    );
    assert_eq!(from_end.get("start").and_then(Value::as_u64), Some(3));
    assert_eq!(from_end.get("end").and_then(Value::as_u64), Some(4));
    assert_eq!(
        from_end.get("text").and_then(Value::as_str),
        Some("three\nfour")
    );

    let to_end = execute_tool_in_context(
        &context,
        "read_file_range",
        r#"{"path":"notes.txt","start":2,"end":-2}"#,
    );
    assert_eq!(to_end.get("start").and_then(Value::as_u64), Some(2));
    assert_eq!(to_end.get("end").and_then(Value::as_u64), Some(5));
    assert_eq!(
        to_end.get("text").and_then(Value::as_str),
        Some("two\nthree\nfour\nfive")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_supports_whole_file() {
    let root = temp_workspace("read-range-whole-file");
    fs::create_dir_all(&root).expect("create temp workspace");
    fs::write(root.join("notes.txt"), "one\ntwo\nthree\nfour\n").expect("write notes");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let read = execute_tool_in_context(
        &context,
        "read_file_range",
        r#"{"path":"notes.txt","whole_file":true}"#,
    );

    assert_eq!(read.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        read.get("whole_file_requested").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(read.get("start").and_then(Value::as_u64), Some(1));
    assert_eq!(read.get("end").and_then(Value::as_u64), Some(4));
    assert_eq!(
        read.get("text").and_then(Value::as_str),
        Some("one\ntwo\nthree\nfour")
    );
    assert_eq!(read.get("truncated").and_then(Value::as_bool), Some(false));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_strips_utf8_bom_from_visible_text() {
    let root = temp_workspace("read-range-bom");
    fs::create_dir_all(&root).expect("create temp workspace");
    fs::write(root.join("bom.txt"), "\u{feff}title\nbody\n").expect("write bom file");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let read = execute_tool_in_context(
        &context,
        "read_file_range",
        r#"{"path":"bom.txt","count":1}"#,
    );

    assert_eq!(read.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(read.get("has_bom").and_then(Value::as_bool), Some(true));
    assert_eq!(read.get("text").and_then(Value::as_str), Some("title"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_without_end_or_count_reads_toward_eof() {
    let root = temp_workspace("read-range-eof");
    fs::create_dir_all(&root).expect("create temp workspace");
    fs::write(root.join("notes.txt"), "one\ntwo\nthree\n").expect("write notes");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let read = execute_tool_in_context(
        &context,
        "read_file_range",
        r#"{"path":"notes.txt","start":2}"#,
    );

    assert_eq!(read.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(read.get("text").and_then(Value::as_str), Some("two\nthree"));
    assert_eq!(read.get("truncated").and_then(Value::as_bool), Some(false));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_empty_file_is_not_marked_truncated() {
    let root = temp_workspace("read-range-empty");
    fs::create_dir_all(&root).expect("create temp workspace");
    fs::write(root.join("empty.txt"), "").expect("write empty file");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let read = execute_tool_in_context(&context, "read_file_range", r#"{"path":"empty.txt"}"#);

    assert_eq!(read.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(read.get("start").and_then(Value::as_u64), Some(1));
    assert_eq!(read.get("end").and_then(Value::as_u64), Some(0));
    assert_eq!(read.get("text").and_then(Value::as_str), Some(""));
    assert_eq!(read.get("truncated").and_then(Value::as_bool), Some(false));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_rejects_binary_images() {
    let root = temp_workspace("read-range-binary-image");
    fs::create_dir_all(&root).expect("create temp workspace");
    fs::write(root.join("sample.png"), b"\x89PNG\r\n\x1a\nfake").expect("write png");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let read = execute_tool_in_context(&context, "read_file_range", r#"{"path":"sample.png"}"#);

    assert_eq!(read.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(
        read.get("error").and_then(Value::as_str),
        Some("binary_file_not_supported")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_rejects_binary_markers_without_extension_hint() {
    let root = temp_workspace("read-range-binary-marker");
    fs::create_dir_all(&root).expect("create temp workspace");
    fs::write(root.join("blob.dat"), b"abc\0def").expect("write binary-ish file");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let read = execute_tool_in_context(&context, "read_file_range", r#"{"path":"blob.dat"}"#);

    assert_eq!(read.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(
        read.get("error").and_then(Value::as_str),
        Some("binary_file_not_supported")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_pages_files_larger_than_the_old_whole_file_limit() {
    let root = temp_workspace("read-range-large-page");
    fs::create_dir_all(&root).expect("create temp workspace");
    fs::write(root.join("large.txt"), "line\n".repeat(300_000)).expect("write large text");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let read = execute_tool_in_context(
        &context,
        "read_file_range",
        r#"{"path":"large.txt","start":1,"count":2}"#,
    );

    assert_eq!(read.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(read.get("start").and_then(Value::as_u64), Some(1));
    assert_eq!(read.get("end").and_then(Value::as_u64), Some(2));
    assert_eq!(read.get("next_start").and_then(Value::as_u64), Some(3));
    assert_eq!(read.get("truncated").and_then(Value::as_bool), Some(true));
    assert_eq!(read.get("text").and_then(Value::as_str), Some("line\nline"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_continues_a_long_line_without_proxy_truncation() {
    let root = temp_workspace("read-range-long-line");
    fs::create_dir_all(&root).expect("create temp workspace");
    fs::write(root.join("long.txt"), "x".repeat(MAX_READ_TEXT_CHARS + 50))
        .expect("write long line");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let first = execute_tool_in_context(
        &context,
        "read_file_range",
        r#"{"path":"long.txt","count":1}"#,
    );
    assert_eq!(first.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        first.get("truncation_reason").and_then(Value::as_str),
        Some("output_limit")
    );
    assert_eq!(first.get("next_start").and_then(Value::as_u64), Some(1));
    assert_eq!(
        first.get("next_column").and_then(Value::as_u64),
        Some(MAX_READ_TEXT_CHARS as u64)
    );

    let second = execute_tool_in_context(
        &context,
        "read_file_range",
        &json!({
            "path": "long.txt",
            "start": 1,
            "count": 1,
            "start_column": MAX_READ_TEXT_CHARS
        })
        .to_string(),
    );
    assert_eq!(second.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        second.get("text").and_then(Value::as_str),
        Some("x".repeat(50).as_str())
    );
    assert_eq!(
        second.get("truncated").and_then(Value::as_bool),
        Some(false)
    );
    assert!(serialized_json_chars(&first) <= MAX_TOOL_RESULT_JSON_CHARS);
    assert!(serialized_json_chars(&second) <= MAX_TOOL_RESULT_JSON_CHARS);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_counts_unicode_columns_and_keeps_reversed_ranges_empty() {
    let root = temp_workspace("read-range-unicode-boundary");
    fs::create_dir_all(&root).expect("create temp workspace");
    fs::write(
        root.join("unicode.txt"),
        "中".repeat(MAX_READ_TEXT_CHARS.saturating_add(1)),
    )
    .expect("write unicode long line");
    fs::write(root.join("lines.txt"), "one\ntwo\nthree\nfour\n").expect("write lines");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let first = execute_tool_in_context(
        &context,
        "read_file_range",
        r#"{"path":"unicode.txt","count":1}"#,
    );
    assert_eq!(
        first
            .get("text")
            .and_then(Value::as_str)
            .map(|text| text.chars().count()),
        Some(MAX_READ_TEXT_CHARS)
    );
    assert_eq!(
        first.get("next_column").and_then(Value::as_u64),
        Some(MAX_READ_TEXT_CHARS as u64)
    );
    assert!(serialized_json_chars(&first) <= MAX_TOOL_RESULT_JSON_CHARS);

    let second = execute_tool_in_context(
        &context,
        "read_file_range",
        &json!({
            "path": "unicode.txt",
            "start": 1,
            "count": 1,
            "start_column": MAX_READ_TEXT_CHARS,
        })
        .to_string(),
    );
    assert_eq!(second.get("text").and_then(Value::as_str), Some("中"));
    assert_eq!(
        second.get("truncated").and_then(Value::as_bool),
        Some(false)
    );

    let reversed = execute_tool_in_context(
        &context,
        "read_file_range",
        r#"{"path":"lines.txt","start":4,"end":2}"#,
    );
    assert_eq!(reversed.get("text").and_then(Value::as_str), Some(""));
    assert_eq!(reversed.get("end").and_then(Value::as_u64), Some(3));
    assert_eq!(reversed.get("next_start"), Some(&Value::Null));
    assert_eq!(
        reversed.get("truncated").and_then(Value::as_bool),
        Some(false)
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
#[ignore = "writes a file just above the 64MiB scan ceiling; run explicitly as a pressure test"]
fn read_file_range_rejects_negative_ranges_above_the_scan_ceiling() {
    let root = temp_workspace("read-range-scan-ceiling");
    fs::create_dir_all(&root).expect("create temp workspace");
    let path = root.join("large.txt");
    let mut file = fs::File::create(&path).expect("create large text file");
    let chunk = b"line\n".repeat(16_384);
    let mut written = 0_u64;
    while written <= MAX_READ_SCAN_BYTES {
        let remaining = MAX_READ_SCAN_BYTES
            .saturating_add(1)
            .saturating_sub(written);
        let length = usize::try_from(remaining)
            .unwrap_or(chunk.len())
            .min(chunk.len());
        file.write_all(&chunk[..length]).expect("write text chunk");
        written = written.saturating_add(length as u64);
    }
    drop(file);

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let read = execute_tool_in_context(
        &context,
        "read_file_range",
        r#"{"path":"large.txt","start":-1}"#,
    );
    assert_eq!(read.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(
        read.get("error").and_then(Value::as_str),
        Some("file_scan_limit_exceeded")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_redacts_credential_values() {
    let root = temp_workspace("read-range-redact");
    fs::create_dir_all(&root).expect("create temp workspace");
    let secret = "sk-test-000000000000000000000001";
    fs::write(
        root.join(".env"),
        format!("DEEPSEEK_API_KEY={secret}\nSAFE_VALUE=visible\n"),
    )
    .expect("write env file");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let read = execute_tool_in_context(&context, "read_file_range", r#"{"path":".env"}"#);
    let text = read.get("text").and_then(Value::as_str).unwrap_or_default();
    assert!(!text.contains(secret));
    assert!(text.contains("DEEPSEEK_API_KEY=[REDACTED]"));
    assert_eq!(
        read.get("redacted_sensitive_values")
            .and_then(Value::as_u64),
        Some(1)
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn list_directory_pages_compact_entries_and_can_include_metadata() {
    let root = temp_workspace("list-metadata");
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(root.join("README.md"), "hello").expect("write readme");
    fs::write(root.join("src").join("main.rs"), "fn main() {}").expect("write main");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let listed =
        execute_tool_in_context(&context, "list_directory", r#"{"include_metadata":true}"#);

    assert_eq!(listed.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(listed.get("depth").and_then(Value::as_u64), Some(0));
    let entries = listed.get("entries").and_then(Value::as_array).unwrap();
    assert!(entries.iter().any(|entry| {
        entry.get("type").and_then(Value::as_str) == Some("file")
            && entry.get("name").and_then(Value::as_str) == Some("README.md")
            && entry.get("size_bytes").and_then(Value::as_u64) == Some(5)
            && entry.get("path").and_then(Value::as_str) == Some("README.md")
    }));
    assert!(entries.iter().any(|entry| {
        entry.get("type").and_then(Value::as_str) == Some("dir")
            && entry.get("name").and_then(Value::as_str) == Some("src")
    }));
    assert!(!entries
        .iter()
        .any(|entry| { entry.get("name").and_then(Value::as_str) == Some("main.rs") }));
    assert_eq!(
        listed
            .pointer("/summary/metadata_included")
            .and_then(Value::as_bool),
        Some(true)
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn list_directory_uses_compact_cursor_pages() {
    let root = temp_workspace("list-cursor-pages");
    fs::create_dir_all(&root).expect("create temp workspace");
    for index in 0..75 {
        fs::write(root.join(format!("{index:03}.txt")), "x").expect("write entry");
    }

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let first = execute_tool_in_context(&context, "list_directory", r#"{"path":"."}"#);
    let first_entries = first.get("entries").and_then(Value::as_array).unwrap();
    assert_eq!(first_entries.len(), DEFAULT_DIRECTORY_PAGE_SIZE);
    assert_eq!(first.get("next_cursor").and_then(Value::as_u64), Some(60));
    assert_eq!(
        first.get("truncation_reason").and_then(Value::as_str),
        Some("page_limit")
    );
    assert!(first_entries.iter().all(|entry| {
        entry
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| !path.contains(":/"))
    }));

    let second = execute_tool_in_context(&context, "list_directory", r#"{"path":".","cursor":60}"#);
    assert_eq!(
        second
            .get("entries")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(15)
    );
    assert_eq!(second.get("has_more").and_then(Value::as_bool), Some(false));
    assert!(serialized_json_chars(&first) <= MAX_TOOL_RESULT_JSON_CHARS);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn list_directory_stops_without_a_repeating_cursor_when_entry_names_fill_output_budget() {
    let root = temp_workspace("list-output-budget");
    fs::create_dir_all(&root).expect("create temp workspace");
    let long_name = "a".repeat(160);
    for index in 0..80 {
        fs::write(root.join(format!("{index:03}-{long_name}.txt")), "x").expect("write entry");
    }

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let listed = execute_tool_in_context(&context, "list_directory", r#"{"path":"."}"#);
    assert_eq!(listed.get("ok").and_then(Value::as_bool), Some(true));
    assert!(listed
        .get("entries")
        .and_then(Value::as_array)
        .is_some_and(|entries| !entries.is_empty() && entries.len() < DEFAULT_DIRECTORY_PAGE_SIZE));
    assert_eq!(listed.get("has_more").and_then(Value::as_bool), Some(false));
    assert!(listed.get("next_cursor").is_some_and(Value::is_null));
    assert_eq!(
        listed.get("truncation_reason").and_then(Value::as_str),
        Some("output_budget")
    );
    assert_eq!(
        listed.get("output_budget_reached").and_then(Value::as_bool),
        Some(true)
    );
    assert!(serialized_json_chars(&listed) <= MAX_TOOL_RESULT_JSON_CHARS);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn list_directory_does_not_offer_a_repeating_cursor_after_scan_limit() {
    let root = temp_workspace("list-scan-limit");
    fs::create_dir_all(&root).expect("create temp workspace");
    for index in 0..=MAX_DIRECTORY_SCAN_ENTRIES {
        fs::write(root.join(format!("{index:04}.txt")), "x").expect("write entry");
    }

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let listed = execute_tool_in_context(
        &context,
        "list_directory",
        &json!({ "path": ".", "cursor": MAX_DIRECTORY_SCAN_ENTRIES }).to_string(),
    );
    assert_eq!(listed.get("has_more").and_then(Value::as_bool), Some(false));
    assert!(listed.get("next_cursor").is_some_and(Value::is_null));
    assert_eq!(
        listed.get("truncation_reason").and_then(Value::as_str),
        Some("scan_limit")
    );
    assert_eq!(
        listed.get("scan_limit_reached").and_then(Value::as_bool),
        Some(true)
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_search_supports_include_and_exclude_globs() {
    let root = temp_workspace("search-globs");
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::create_dir_all(root.join("target")).expect("create target");
    fs::write(root.join("src").join("main.rs"), "needle\n").expect("write rs");
    fs::write(root.join("src").join("main.txt"), "needle\n").expect("write txt");
    fs::write(root.join("target").join("ignored.rs"), "needle\n").expect("write ignored");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let searched = execute_tool_in_context(
        &context,
        "workspace_search",
        r#"{"query":"needle","include":"src/*.rs","exclude":"target"}"#,
    );

    assert_eq!(searched.get("ok").and_then(Value::as_bool), Some(true));
    let matches = searched.get("matches").and_then(Value::as_array).unwrap();
    assert_eq!(matches.len(), 1);
    assert!(matches[0]
        .get("path")
        .and_then(Value::as_str)
        .unwrap()
        .eq("src/main.rs"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_search_defers_low_priority_dirs_from_broad_root() {
    let root = temp_workspace("search-deferred-dirs");
    let git_dir = root.join(".git");
    let ui_dir = root
        .join("apps")
        .join("ui")
        .join("public")
        .join("styles")
        .join("pages");
    fs::create_dir_all(&git_dir).expect("create git dir");
    fs::create_dir_all(&ui_dir).expect("create ui dir");
    for index in 0..(MAX_SEARCH_FILES + 20) {
        fs::write(git_dir.join(format!("{index:04}.txt")), "no match\n").expect("write noise");
    }
    fs::write(ui_dir.join("logs.css"), ".log-panel { display: flex; }\n")
        .expect("write target css");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let searched = execute_tool_in_context(
        &context,
        "workspace_search",
        r#"{"query":"log-panel","path":"."}"#,
    );

    assert_eq!(searched.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        searched
            .get("deferred_dirs_discovered")
            .and_then(Value::as_u64)
            .is_some_and(|count| count >= 1),
        "expected low-priority directories to be deferred: {searched}"
    );
    let matches = searched.get("matches").and_then(Value::as_array).unwrap();
    assert_eq!(
        matches.len(),
        1,
        "expected target match before low-priority directory consumes the search budget: {searched}"
    );
    assert!(matches[0]
        .get("path")
        .and_then(Value::as_str)
        .unwrap()
        .eq("apps/ui/public/styles/pages/logs.css"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_search_continues_into_deferred_dirs_by_default() {
    let root = temp_workspace("search-deferred-default-included");
    fs::create_dir_all(root.join(".git")).expect("create git dir");
    fs::create_dir_all(root.join("src")).expect("create src dir");
    fs::write(root.join("src").join("lib.rs"), "shared_needle in source\n").expect("write source");
    fs::write(
        root.join(".git").join("config"),
        "shared_needle in git metadata\n",
    )
    .expect("write git metadata");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let searched = execute_tool_in_context(
        &context,
        "workspace_search",
        r#"{"query":"shared_needle","path":".","max_results":10}"#,
    );

    assert_eq!(searched.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        searched
            .get("include_deferred_dirs")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        searched
            .get("deferred_dirs_searched")
            .and_then(Value::as_u64),
        Some(1),
        "expected default search to continue into deferred dirs: {searched}"
    );
    let paths = searched
        .get("matches")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter_map(|item| item.get("path").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(paths.contains(&"src/lib.rs"));
    assert!(paths.contains(&".git/config"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_search_can_search_explicit_low_priority_root() {
    let root = temp_workspace("search-explicit-low-priority-root");
    fs::create_dir_all(root.join("target")).expect("create target dir");
    fs::write(
        root.join("target").join("artifact.txt"),
        "needle in target\n",
    )
    .expect("write target file");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let searched = execute_tool_in_context(
        &context,
        "workspace_search",
        r#"{"query":"needle","path":"target"}"#,
    );

    assert_eq!(searched.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        searched
            .get("deferred_dirs_discovered")
            .and_then(Value::as_u64),
        Some(0)
    );
    let matches = searched.get("matches").and_then(Value::as_array).unwrap();
    assert_eq!(
        matches.len(),
        1,
        "expected explicit target search to work: {searched}"
    );
    assert!(matches[0]
        .get("path")
        .and_then(Value::as_str)
        .unwrap()
        .eq("target/artifact.txt"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_search_explicit_include_can_search_low_priority_dir() {
    let root = temp_workspace("search-explicit-low-priority-include");
    fs::create_dir_all(root.join("target")).expect("create target dir");
    fs::write(
        root.join("target").join("artifact.txt"),
        "needle in target\n",
    )
    .expect("write target file");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let searched = execute_tool_in_context(
        &context,
        "workspace_search",
        r#"{"query":"needle","include":"target/*"}"#,
    );

    assert_eq!(searched.get("ok").and_then(Value::as_bool), Some(true));
    let matches = searched.get("matches").and_then(Value::as_array).unwrap();
    assert_eq!(
        matches.len(),
        1,
        "expected explicit include to search matching low-priority directory: {searched}"
    );
    assert!(matches[0]
        .get("path")
        .and_then(Value::as_str)
        .unwrap()
        .eq("target/artifact.txt"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_search_supports_regex_mode() {
    let root = temp_workspace("search-regex");
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(root.join("src").join("main.rs"), "alpha_42\nalpha_x\n").expect("write rs");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let searched = execute_tool_in_context(
        &context,
        "workspace_search",
        r#"{"query":"alpha_\\d+","regex":true,"include":"src/*.rs"}"#,
    );

    assert_eq!(searched.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(searched.get("regex").and_then(Value::as_bool), Some(true));
    let matches = searched.get("matches").and_then(Value::as_array).unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].get("line").and_then(Value::as_u64), Some(1));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_search_bounds_result_size_and_redacts_snippets() {
    let root = temp_workspace("search-output-budget");
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create source dir");
    let secret = "sk-test-000000000000000000000002";
    for index in 0..40 {
        fs::write(
            src.join(format!("{index:02}.txt")),
            format!("API_KEY={secret}\nneedle {}\n", "x".repeat(1_200)),
        )
        .expect("write result file");
    }
    fs::write(root.join("large.txt"), "needle\n".repeat(300_000))
        .expect("write oversized search file");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let searched = execute_tool_in_context(
        &context,
        "workspace_search",
        r#"{"query":"needle","path":"."}"#,
    );
    let matches = searched.get("matches").and_then(Value::as_array).unwrap();
    let serialized = searched.to_string();
    assert!(!matches.is_empty());
    assert!(
        matches.len() < 40,
        "output budget should page broad matches"
    );
    assert!(searched
        .get("truncation_reasons")
        .and_then(Value::as_array)
        .is_some_and(|reasons| {
            reasons.iter().any(|reason| reason == "output_budget")
                && reasons.iter().any(|reason| reason == "large_files_skipped")
        }));
    assert_eq!(
        searched.get("files_skipped_large").and_then(Value::as_u64),
        Some(1)
    );
    assert!(!serialized.contains(secret));
    assert!(searched
        .get("redacted_sensitive_values")
        .and_then(Value::as_u64)
        .is_some_and(|count| count > 0));
    assert!(serialized_json_chars(&searched) <= MAX_TOOL_RESULT_JSON_CHARS);

    let _ = fs::remove_dir_all(root);
}

#[test]
#[ignore = "writes 33MiB to exercise the workspace search aggregate scan ceiling"]
fn workspace_search_stops_at_its_total_scan_budget() {
    let root = temp_workspace("search-scan-budget");
    fs::create_dir_all(&root).expect("create temp workspace");
    let content = "x".repeat(MAX_SEARCH_FILE_BYTES as usize);
    for index in 0..33 {
        fs::write(root.join(format!("{index:02}.txt")), &content).expect("write search file");
    }

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let searched = execute_tool_in_context(
        &context,
        "workspace_search",
        r#"{"query":"needle","path":"."}"#,
    );
    assert_eq!(searched.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        searched.get("files_scanned").and_then(Value::as_u64),
        Some(32)
    );
    assert_eq!(
        searched.get("bytes_scanned").and_then(Value::as_u64),
        Some(MAX_SEARCH_BYTES)
    );
    assert_eq!(
        searched
            .get("matches")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0)
    );
    assert!(searched
        .get("truncation_reasons")
        .and_then(Value::as_array)
        .is_some_and(|reasons| reasons.iter().any(|reason| reason == "scan_byte_limit")));
    assert!(serialized_json_chars(&searched) <= MAX_TOOL_RESULT_JSON_CHARS);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_search_reports_invalid_regex() {
    let root = temp_workspace("search-invalid-regex");
    fs::create_dir_all(&root).expect("create temp workspace");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let searched = execute_tool_in_context(
        &context,
        "workspace_search",
        r#"{"query":"(","regex":true}"#,
    );

    assert_eq!(searched.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(
        searched.get("error").and_then(Value::as_str),
        Some("invalid_regex")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_search_full_access_can_search_absolute_outside_workspace() {
    let root = temp_workspace("search-full-access-root");
    let outside = temp_workspace("search-full-access-outside");
    fs::create_dir_all(&root).expect("create root");
    fs::create_dir_all(&outside).expect("create outside");
    fs::write(outside.join("outside.txt"), "needle outside\n").expect("write outside file");
    let outside = fs::canonicalize(&outside).expect("canonical outside");

    let context = ToolExecutionContext::new(vec![root.clone()], true);
    let searched = execute_tool_in_context(
        &context,
        "workspace_search",
        &json!({
            "query": "needle",
            "path": outside.to_string_lossy()
        })
        .to_string(),
    );

    assert_eq!(searched.get("ok").and_then(Value::as_bool), Some(true));
    let matches = searched.get("matches").and_then(Value::as_array).unwrap();
    assert_eq!(matches.len(), 1);
    assert!(matches[0]
        .get("path")
        .and_then(Value::as_str)
        .unwrap()
        .ends_with("/outside.txt"));

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside);
}

#[test]
fn read_only_tools_execute_inside_workspace_and_reject_escape() {
    let root = temp_workspace("read-only-tools");
    let outside = root.parent().expect("temp workspace parent").join(format!(
        "{}-outside.txt",
        root.file_name().unwrap().to_string_lossy()
    ));
    fs::create_dir_all(&root).expect("create temp workspace");
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/core\"]\n",
    )
    .expect("write Cargo.toml");
    fs::write(root.join("README.md"), "CodeSeeX smoke file\n").expect("write README.md");
    fs::write(&outside, "outside").expect("write outside file");

    std::env::set_var("CODESEEX_WORKSPACE_ROOT", &root);

    let listed = execute_tool("list_directory", r#"{"path":".","depth":0}"#);
    assert_eq!(listed.get("ok").and_then(Value::as_bool), Some(true));
    assert!(listed.to_string().contains("Cargo.toml"));

    let listed_virtual_root = execute_tool("list_directory", r#"{"path":"/","depth":0}"#);
    assert_eq!(
        listed_virtual_root.get("ok").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        listed_virtual_root.get("path").and_then(Value::as_str),
        Some(".")
    );

    let read = execute_tool("read_file_range", r#"{"path":"Cargo.toml","count":2}"#);
    assert_eq!(read.get("ok").and_then(Value::as_bool), Some(true));
    assert!(read.to_string().contains("[workspace]"));

    let read_with_virtual_root =
        execute_tool("read_file_range", r#"{"path":"/README.md","count":1}"#);
    assert_eq!(
        read_with_virtual_root.get("ok").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        read_with_virtual_root.get("path").and_then(Value::as_str),
        Some("README.md")
    );

    let searched = execute_tool(
        "workspace_search",
        r#"{"query":"CodeSeeX","path":"README.md"}"#,
    );
    assert_eq!(searched.get("ok").and_then(Value::as_bool), Some(true));
    assert!(searched.to_string().contains("CodeSeeX"));

    let escape_args = json!({
        "path": format!("../{}", outside.file_name().unwrap().to_string_lossy())
    })
    .to_string();
    let escaped = execute_tool("read_file_range", &escape_args);
    assert_eq!(
        escaped.get("error").and_then(Value::as_str),
        Some("path_outside_workspace")
    );

    std::env::remove_var("CODESEEX_WORKSPACE_ROOT");
    let missing_root = execute_tool("list_directory", r#"{"path":"."}"#);
    assert_eq!(
        missing_root.get("error").and_then(Value::as_str),
        Some("workspace_root_not_configured")
    );
    let _ = fs::remove_file(outside);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_only_tools_follow_request_file_access_scope() {
    let root = temp_workspace("request-scope-root");
    let outside_root = temp_workspace("request-scope-outside");
    fs::create_dir_all(&root).expect("create scoped root");
    fs::create_dir_all(&outside_root).expect("create outside root");
    fs::write(root.join("inside.txt"), "inside\n").expect("write inside file");
    let outside_file = outside_root.join("outside.txt");
    fs::write(&outside_file, "outside\n").expect("write outside file");
    let root = fs::canonicalize(&root).expect("canonical root");
    let outside_file = fs::canonicalize(&outside_file).expect("canonical outside file");

    let restricted = ToolExecutionContext::new(vec![root.clone()], false);
    let outside_args = json!({ "path": outside_file.to_string_lossy(), "count": 1 }).to_string();
    let rejected = execute_tool_in_context(&restricted, "read_file_range", &outside_args);
    assert_eq!(
        rejected.get("error").and_then(Value::as_str),
        Some("path_outside_workspace")
    );

    let full_access = ToolExecutionContext::new(vec![root.clone()], true);
    let allowed = execute_tool_in_context(&full_access, "read_file_range", &outside_args);
    assert_eq!(allowed.get("ok").and_then(Value::as_bool), Some(true));
    assert!(allowed.to_string().contains("outside"));

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside_root);
}

fn temp_workspace(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    std::env::temp_dir().join(format!("codeseex-{label}-{nanos}"))
}
