use super::*;
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
fn legacy_visual_search_enabled_id_exposes_vision_analyze() {
    let definitions = upstream_tool_definitions(&["visual_search".to_owned()]);
    assert!(definitions.iter().any(|definition| {
        definition.pointer("/function/name").and_then(Value::as_str) == Some("vision_analyze")
    }));
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
    assert!(description.contains("Bare headers"));
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
    assert!(is_executable_tool_enabled(
        "visual_search",
        &["visual_search".to_owned()]
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
    let mut config = codeseex_core::AppConfig::default();
    config.data_dir = data_dir.clone();
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
    let mut config = codeseex_core::AppConfig::default();
    config.data_dir = data_dir.clone();
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
    let mut config = codeseex_core::AppConfig::default();
    config.data_dir = data_dir.clone();
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
    assert_eq!(read.get("whole_file").and_then(Value::as_bool), Some(true));
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
    assert_eq!(
        read.pointer("/lines/0/text").and_then(Value::as_str),
        Some("title")
    );

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
fn list_directory_defaults_to_current_directory_with_metadata_summary() {
    let root = temp_workspace("list-metadata");
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(root.join("README.md"), "hello").expect("write readme");
    fs::write(root.join("src").join("main.rs"), "fn main() {}").expect("write main");

    let context = ToolExecutionContext::new(vec![root.clone()], false);
    let listed = execute_tool_in_context(&context, "list_directory", r#"{}"#);

    assert_eq!(listed.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(listed.get("depth").and_then(Value::as_u64), Some(0));
    let entries = listed.get("entries").and_then(Value::as_array).unwrap();
    assert!(entries.iter().any(|entry| {
        entry.get("type").and_then(Value::as_str) == Some("file")
            && entry.get("name").and_then(Value::as_str) == Some("README.md")
            && entry.get("size_bytes").and_then(Value::as_u64) == Some(5)
            && entry.get("extension").and_then(Value::as_str) == Some("md")
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
            .pointer("/summary/extensions/md")
            .and_then(Value::as_u64),
        Some(1)
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
        .ends_with("/src/main.rs"));

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
    let root_text = absolute_display_path(&root);
    assert!(listed.to_string().contains("Cargo.toml"));

    let listed_virtual_root = execute_tool("list_directory", r#"{"path":"/","depth":0}"#);
    assert_eq!(
        listed_virtual_root.get("ok").and_then(Value::as_bool),
        Some(true)
    );
    let root_path_text = root_text.replace('\\', "/");
    assert_eq!(
        listed_virtual_root.get("path").and_then(Value::as_str),
        Some(root_path_text.as_str())
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
    let readme_text = absolute_display_path(&root.join("README.md"));
    assert_eq!(
        read_with_virtual_root.get("path").and_then(Value::as_str),
        Some(readme_text.as_str())
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
