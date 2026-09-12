//! Observation and boundary mapping of a native DeepSeek Responses SSE stream.
//!
//! Native transport must not borrow Chat compatibility, must not append a
//! `[DONE]` sentinel, and must never rewrite the provider's own tool protocol.
//! Two boundaries are mapped here, and both stay client-facing only:
//! response identity, and the reasoning presentation Codex renders. The
//! inspector always observes the untouched upstream frames, so the provider
//! copy remains authoritative for tool continuations.

use codeseex_core::config::WebSearchBackend;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const MAX_INSPECTED_SSE_FRAME_BYTES: usize = 256 * 1024;
/// A frame too large to inspect is still the provider's own frame, so it is
/// forwarded as it arrived rather than dropped. Past this hard cap the relay
/// stops buffering a frame that never terminated, so a truncated or hostile
/// stream cannot grow without bound.
const MAX_RELAYED_SSE_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_RETAINED_NATIVE_OUTPUT_ITEMS: usize = 128;
const MAX_RETAINED_NATIVE_OUTPUT_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeToolPlan {
    pub(crate) tools: Vec<Value>,
    /// True means the request needs the native hosted tool loop, which executes
    /// CodeSeeX-hosted tools (local web search) inside the native transport.
    pub(crate) requires_local_execution: bool,
    pub(crate) uses_official_web_search: bool,
}

/// A provider output group is immutable protocol data. The coordinator may
/// append one complete, ordered set of outputs to it, but must never split or
/// reorder its calls. This is the boundary proven by the live mixed-tool
/// probe: DeepSeek rejects a partial group with HTTP 400.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeToolCallGroup {
    pub(crate) provider_output: Vec<Value>,
    pub(crate) calls: Vec<NativeToolCall>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeToolCall {
    pub(crate) call_id: String,
    pub(crate) name: String,
    pub(crate) input: String,
    pub(crate) kind: NativeToolCallKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeToolCallKind {
    Function,
    Custom,
}

/// Parses only the two provider tool-call item shapes CodeSeeX has verified
/// against DeepSeek Responses. Unknown output items remain provider-owned;
/// unknown/malformed *tool calls* fail closed rather than being dropped.
pub(crate) fn native_tool_call_group_from_response(
    response: &Value,
) -> Result<Option<NativeToolCallGroup>, String> {
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| "Native Responses response did not contain an output array.".to_owned())?;
    let mut call_ids = BTreeSet::new();
    let mut calls = Vec::new();
    for item in output {
        let Some(call) = native_tool_call_from_output_item(item)? else {
            continue;
        };
        if !call_ids.insert(call.call_id.clone()) {
            return Err(
                "Native Responses returned duplicate tool call identifiers in one output group."
                    .to_owned(),
            );
        }
        calls.push(call);
    }
    if calls.is_empty() {
        return Ok(None);
    }
    Ok(Some(NativeToolCallGroup {
        provider_output: output.clone(),
        calls,
    }))
}

/// Builds the exact full-replay continuation for a single provider tool group.
/// The caller supplies output items in the provider call order; every call
/// must have exactly one matching output with its verified native type.
#[allow(dead_code)] // Used by the local-tool native continuation slice.
pub(crate) fn append_complete_native_tool_group(
    authoritative_input: &[Value],
    group: &NativeToolCallGroup,
    output_items: &[Value],
) -> Result<Vec<Value>, String> {
    if output_items.len() != group.calls.len() {
        return Err("Native tool continuation requires one result for every call in the provider output group."
            .to_owned());
    }
    for (call, output) in group.calls.iter().zip(output_items) {
        let expected_type = match call.kind {
            NativeToolCallKind::Function => "function_call_output",
            NativeToolCallKind::Custom => "custom_tool_call_output",
        };
        if output.get("type").and_then(Value::as_str) != Some(expected_type)
            || output.get("call_id").and_then(Value::as_str) != Some(call.call_id.as_str())
            || output.get("output").and_then(Value::as_str).is_none()
        {
            return Err(
                "Native tool continuation output did not exactly match the provider call group."
                    .to_owned(),
            );
        }
    }
    let mut input = Vec::with_capacity(
        authoritative_input.len() + group.provider_output.len() + output_items.len(),
    );
    input.extend_from_slice(authoritative_input);
    input.extend(group.provider_output.iter().cloned());
    input.extend(output_items.iter().cloned());
    Ok(input)
}

#[allow(dead_code)] // Used by the local-tool native continuation slice.
pub(crate) fn native_tool_output_item(call: &NativeToolCall, output: impl Into<String>) -> Value {
    let item_type = match call.kind {
        NativeToolCallKind::Function => "function_call_output",
        NativeToolCallKind::Custom => "custom_tool_call_output",
    };
    json!({
        "type": item_type,
        "call_id": call.call_id,
        "output": output.into()
    })
}

fn native_tool_call_from_output_item(item: &Value) -> Result<Option<NativeToolCall>, String> {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
    let (kind, input_field) = match item_type {
        "function_call" => (NativeToolCallKind::Function, "arguments"),
        "custom_tool_call" => (NativeToolCallKind::Custom, "input"),
        _ => return Ok(None),
    };
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Native tool call did not contain a call identifier.".to_owned())?;
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Native tool call did not contain a tool name.".to_owned())?;
    if kind == NativeToolCallKind::Custom && name != "apply_patch" {
        // A custom grammar CodeSeeX does not coordinate (for example a tool a
        // newer Codex declares) stays provider-owned: the item is forwarded to
        // the client untouched and CodeSeeX claims no output for it.
        return Ok(None);
    }
    if item.get("status").and_then(Value::as_str) != Some("completed") {
        return Err("Native tool call was not completed in the provider response.".to_owned());
    }
    let input = item
        .get(input_field)
        .and_then(Value::as_str)
        .ok_or_else(|| "Native tool call did not contain its verified input field.".to_owned())?;
    Ok(Some(NativeToolCall {
        call_id: call_id.to_owned(),
        name: name.to_owned(),
        input: input.to_owned(),
        kind,
    }))
}

/// Converts the existing, Chat-shaped tool definitions into native Responses
/// definitions without deciding how calls are executed. In particular, this
/// keeps CodeSeeX local search and DeepSeek official search mutually exclusive.
///
/// Declarations the endpoint already owns natively (`namespace`, `tool_search`)
/// are validated and forwarded verbatim. The verified endpoint accepts the
/// grouping and echoes the namespace back on the call item, so flattening or
/// rebuilding those declarations would change the tool identity the Codex
/// client resolves. Unknown shapes still fail closed.
/// Plans the tool list for one native request.
///
/// CodeSeeX translates the declarations it owns (CodeSeeX-hosted search,
/// `apply_patch`, plain functions) and forwards everything else exactly as the
/// client sent it. It never rejects a turn because it does not recognise a
/// declaration: the provider is the one that decides what it accepts, and a
/// newer Codex must not be able to break the transport by adding a tool.
pub(crate) fn plan_native_tools(
    chat_tool_definitions: &[Value],
    web_search_backend: WebSearchBackend,
) -> NativeToolPlan {
    let mut tools = Vec::new();
    let mut names = BTreeSet::new();
    let mut requires_local_execution = false;
    let mut saw_local_web_search = false;
    let mut saw_provider_web_search = false;

    for definition in chat_tool_definitions {
        if is_provider_web_search_definition(definition) {
            saw_provider_web_search = true;
            if web_search_backend == WebSearchBackend::Official {
                // Normalize to the one provider-owned definition appended
                // below. This keeps a client-provided native declaration from
                // creating a duplicate official search capability.
                continue;
            }
            // Codex can advertise its provider-native web-search declaration
            // even when the user selected CodeSeeX local search. It is safe to
            // consume that declaration only if the local function definition
            // is also present in this request. The later validation refuses a
            // missing local function instead of silently switching backend.
            continue;
        }
        let Some(name) = tool_name(definition).or_else(|| provider_native_identity(definition))
        else {
            // A declaration CodeSeeX cannot even name is still the client's own
            // declaration. Forward it untouched instead of failing the turn.
            tools.push(definition.clone());
            continue;
        };
        if matches!(name, "web_search" | "web_search_preview") {
            saw_local_web_search = true;
            if web_search_backend == WebSearchBackend::Official {
                continue;
            }
            // CodeSeeX-hosted local search is executed by the native hosted
            // tool loop; ownership never changes silently.
            requires_local_execution = true;
        } else if is_codeseex_local_tool(name) {
            // Workspace tools (list_directory, read_file_range,
            // workspace_search, vision_analyze) are callable by the Codex
            // client itself, so a native request keeps them in the provider
            // tool list instead of falling back to Chat compatibility.
        }
        let native = native_definition_from_chat(definition, name);
        if names.insert(tool_identity(&native).to_owned()) {
            tools.push(native);
        }
    }

    let uses_official_web_search = web_search_backend == WebSearchBackend::Official
        && (saw_provider_web_search || saw_local_web_search);
    if uses_official_web_search {
        // `web_search` is provider-owned in this mode. It replaces the
        // CodeSeeX function with the schema verified against DeepSeek's
        // Responses endpoint; it never coexists with the local function.
        if names.insert("web_search".to_owned()) {
            tools.push(json!({ "type": "web_search" }));
        }
    } else if saw_provider_web_search || saw_local_web_search {
        // CodeSeeX owns local web search in this mode: the declaration asks for
        // the CodeSeeX-hosted function instead of silently switching to
        // provider search. The native hosted tool loop executes that call.
        requires_local_execution = true;
    }

    NativeToolPlan {
        tools,
        requires_local_execution,
        uses_official_web_search,
    }
}

/// A grouped declaration is a named entry that owns nested tools: the
/// `namespace` grouping Codex sends and the provider `mcp` shape.
fn grouped_declaration_name(entry: &Value) -> Option<String> {
    let nested = entry.get("tools").and_then(Value::as_array)?;
    if nested.is_empty() {
        return None;
    }
    entry
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// Where one grouped declaration lives inside a native request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupedToolSite {
    /// An entry of the request top-level `tools` array.
    Tools(usize),
    /// An entry of one `input` item nested `tools` array.
    Input { item: usize, tool: usize },
}

fn grouped_tool_occurrences(object: &Map<String, Value>) -> Vec<(String, GroupedToolSite)> {
    let mut occurrences = Vec::new();
    if let Some(tools) = object.get("tools").and_then(Value::as_array) {
        for (index, entry) in tools.iter().enumerate() {
            if let Some(name) = grouped_declaration_name(entry) {
                occurrences.push((name, GroupedToolSite::Tools(index)));
            }
        }
    }
    if let Some(items) = object.get("input").and_then(Value::as_array) {
        for (item_index, item) in items.iter().enumerate() {
            let Some(tools) = item.get("tools").and_then(Value::as_array) else {
                continue;
            };
            for (tool_index, entry) in tools.iter().enumerate() {
                if let Some(name) = grouped_declaration_name(entry) {
                    occurrences.push((
                        name,
                        GroupedToolSite::Input {
                            item: item_index,
                            tool: tool_index,
                        },
                    ));
                }
            }
        }
    }
    occurrences
}

fn grouped_tool_entry(object: &Map<String, Value>, site: GroupedToolSite) -> Option<&Value> {
    match site {
        GroupedToolSite::Tools(index) => object.get("tools")?.as_array()?.get(index),
        GroupedToolSite::Input { item, tool } => object
            .get("input")?
            .as_array()?
            .get(item)?
            .get("tools")?
            .as_array()?
            .get(tool),
    }
}

fn nested_tools_of(object: &Map<String, Value>, site: GroupedToolSite) -> Vec<Value> {
    grouped_tool_entry(object, site)
        .and_then(|entry| entry.get("tools"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Nested tools are merged by identity so a later declaration only contributes
/// the tools the first one did not already carry.
fn nested_tool_identity(entry: &Value) -> String {
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty());
    match name {
        Some(name) => {
            let kind = entry
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("function");
            format!("{kind}\u{1f}{name}")
        }
        // An entry without a callable name cannot be compared by name; keep it
        // intact and only collapse byte-identical duplicates.
        None => entry.to_string(),
    }
}

fn merge_nested_tools(canonical: &mut Vec<Value>, additional: Vec<Value>) {
    let mut seen = canonical
        .iter()
        .map(nested_tool_identity)
        .collect::<BTreeSet<_>>();
    for entry in additional {
        if seen.insert(nested_tool_identity(&entry)) {
            canonical.push(entry);
        }
    }
}

fn set_grouped_tools(object: &mut Map<String, Value>, site: GroupedToolSite, tools: Vec<Value>) {
    let entry = match site {
        GroupedToolSite::Tools(index) => object
            .get_mut("tools")
            .and_then(Value::as_array_mut)
            .and_then(|tools| tools.get_mut(index)),
        GroupedToolSite::Input { item, tool } => object
            .get_mut("input")
            .and_then(Value::as_array_mut)
            .and_then(|items| items.get_mut(item))
            .and_then(|item| item.get_mut("tools"))
            .and_then(Value::as_array_mut)
            .and_then(|tools| tools.get_mut(tool)),
    };
    if let Some(entry) = entry.and_then(Value::as_object_mut) {
        entry.insert("tools".to_owned(), Value::Array(tools));
    }
}

fn remove_grouped_entries(
    object: &mut Map<String, Value>,
    top_level: Vec<usize>,
    per_item: BTreeMap<usize, Vec<usize>>,
) {
    if !top_level.is_empty() {
        if let Some(tools) = object.get_mut("tools").and_then(Value::as_array_mut) {
            // Deepest index first so the remaining indexes stay valid.
            for index in top_level.into_iter().rev() {
                if index < tools.len() {
                    tools.remove(index);
                }
            }
        }
    }
    for (item_index, mut nested_indexes) in per_item {
        nested_indexes.sort_unstable();
        let tools = object
            .get_mut("input")
            .and_then(Value::as_array_mut)
            .and_then(|items| items.get_mut(item_index))
            .and_then(|item| item.get_mut("tools"))
            .and_then(Value::as_array_mut);
        let Some(tools) = tools else {
            continue;
        };
        for index in nested_indexes.into_iter().rev() {
            if index < tools.len() {
                tools.remove(index);
            }
        }
    }
}

/// Restores the one-declaration-per-name rule the verified endpoint enforces.
///
/// The provider rejects a repeated grouped tool name with
/// `Duplicate namespace name '<name>' in input[<n>].tools[0]. Namespace names must be unique.`
/// Codex replays the thread `tool_search_output` items verbatim, so a namespace
/// discovered in an earlier turn is declared again next to the namespace this
/// turn `tools` already carry, and the two declarations can disagree because
/// the app dynamic tool list changes between restarts.
///
/// The first declaration stays authoritative and every later nested tool is
/// folded into it before the repeated entry is removed, so the request keeps
/// every callable tool and only the repeated name disappears. A request without
/// a repeated name is forwarded byte-for-byte.
pub(crate) fn reconcile_grouped_tool_namespaces(payload: &mut Value) -> Vec<String> {
    let Some(object) = payload.as_object_mut() else {
        return Vec::new();
    };
    let occurrences = grouped_tool_occurrences(object);
    if occurrences.len() < 2 {
        return Vec::new();
    }
    let mut first_site: BTreeMap<String, GroupedToolSite> = BTreeMap::new();
    let mut merged_tools: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut merged_names: Vec<String> = Vec::new();
    let mut repeated: Vec<GroupedToolSite> = Vec::new();
    for (name, site) in occurrences {
        if let Some(canonical) = merged_tools.get_mut(&name) {
            merge_nested_tools(canonical, nested_tools_of(object, site));
            if !merged_names.contains(&name) {
                merged_names.push(name);
            }
            repeated.push(site);
            continue;
        }
        first_site.insert(name.clone(), site);
        merged_tools.insert(name, nested_tools_of(object, site));
    }
    if merged_names.is_empty() {
        return Vec::new();
    }
    for name in &merged_names {
        let (Some(site), Some(tools)) = (first_site.get(name).copied(), merged_tools.get(name))
        else {
            continue;
        };
        set_grouped_tools(object, site, tools.clone());
    }
    let mut top_level = Vec::new();
    let mut per_item: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for site in repeated {
        match site {
            GroupedToolSite::Tools(index) => top_level.push(index),
            GroupedToolSite::Input { item, tool } => per_item.entry(item).or_default().push(tool),
        }
    }
    remove_grouped_entries(object, top_level, per_item);
    merged_names
}

/// The provider requires every forwarded function declaration to carry a
/// parameter schema of `type: "object"`. Codex's deferred app tools can declare
/// a top-level `oneOf` union with no `type` at all, which is valid JSON Schema
/// but is rejected with `Invalid schema for function ... got 'type: null'`.
/// Every branch of such a union is an object schema, so declaring the object type
/// keeps the union intact instead of replacing the tool's contract, and the
/// declaration is repaired in both places the provider validates: the `tools`
/// this turn declares and the `tool_search_output` declarations Codex replays.
///
/// A schema whose `type` is present is never rewritten. Returns the names whose
/// schema had to be completed.
pub(crate) fn repair_provider_tool_schemas(payload: &mut Value) -> Vec<String> {
    let mut repaired = BTreeSet::new();
    if let Some(tools) = payload.get_mut("tools").and_then(Value::as_array_mut) {
        repair_tool_declarations(tools, &mut repaired);
    }
    if let Some(items) = payload.get_mut("input").and_then(Value::as_array_mut) {
        for item in items {
            if let Some(tools) = item.get_mut("tools").and_then(Value::as_array_mut) {
                repair_tool_declarations(tools, &mut repaired);
            }
        }
    }
    repaired.into_iter().collect()
}

fn repair_tool_declarations(tools: &mut [Value], repaired: &mut BTreeSet<String>) {
    for tool in tools {
        if let Some(nested) = tool.get_mut("tools").and_then(Value::as_array_mut) {
            repair_tool_declarations(nested, repaired);
        }
        if tool.get("type").and_then(Value::as_str) != Some("function") {
            continue;
        }
        let has_parameters = tool.get("parameters").is_some();
        let has_input_schema = tool.get("input_schema").is_some();
        let mut changed = false;
        if has_parameters {
            changed |= repair_object_schema(tool.get_mut("parameters"));
        }
        if has_input_schema {
            changed |= repair_object_schema(tool.get_mut("input_schema"));
        }
        if !has_parameters && !has_input_schema {
            // A function without any declared schema takes no arguments; the
            // provider refuses to infer that on its own.
            if let Some(object) = tool.as_object_mut() {
                object.insert(
                    "parameters".to_owned(),
                    json!({ "type": "object", "properties": {} }),
                );
            }
            changed = true;
        }
        if changed {
            if let Some(name) = tool.get("name").and_then(Value::as_str) {
                repaired.insert(name.to_owned());
            }
        }
    }
}

fn repair_object_schema(schema: Option<&mut Value>) -> bool {
    let Some(schema) = schema else {
        return false;
    };
    let Some(object) = schema.as_object_mut() else {
        *schema = json!({ "type": "object", "properties": {} });
        return true;
    };
    if object.get("type").is_some_and(|value| !value.is_null()) {
        return false;
    }
    object.insert("type".to_owned(), Value::String("object".to_owned()));
    true
}

/// Rewrites only the provider response identity at the local lifecycle
/// boundary. Output-item ids and tool call ids have a different scope and must
/// remain untouched so later full replay stays valid.
pub(crate) fn rewrite_provider_response_identity(
    payload: &mut Value,
    provider_response_id: &str,
    local_response_id: &str,
) -> bool {
    if provider_response_id == local_response_id {
        return false;
    }
    let mut changed = false;
    if payload
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|value| value == provider_response_id)
    {
        payload["id"] = Value::String(local_response_id.to_owned());
        changed = true;
    }
    if payload
        .pointer("/response/id")
        .and_then(Value::as_str)
        .is_some_and(|value| value == provider_response_id)
    {
        payload["response"]["id"] = Value::String(local_response_id.to_owned());
        changed = true;
    }
    if payload
        .get("response_id")
        .and_then(Value::as_str)
        .is_some_and(|value| value == provider_response_id)
    {
        payload["response_id"] = Value::String(local_response_id.to_owned());
        changed = true;
    }
    changed
}

fn tool_name(definition: &Value) -> Option<&str> {
    definition
        .pointer("/function/name")
        .and_then(Value::as_str)
        .or_else(|| definition.get("name").and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
}

/// Provider-native grouped declarations CodeSeeX forwards untouched.
///
/// The Responses endpoint owns these shapes: it accepts the grouping and, for
/// `namespace`, echoes the namespace on the returned `function_call` item so
/// the client can still resolve which tool ran. Flattening a namespace would
/// strand that grouping, and dropping the declaration would silently remove
/// capability, so the only safe translation is none at all.
fn is_provider_native_grouped_type(declared_type: &str) -> bool {
    matches!(declared_type, "namespace" | "tool_search")
}

/// The provider-facing declaration for one client tool.
///
/// Declarations CodeSeeX owns are translated; anything else (including a grouped
/// namespace, an unknown type, or a custom grammar CodeSeeX does not know) is
/// forwarded verbatim so the provider decides whether it is acceptable.
fn native_definition_from_chat(definition: &Value, name: &str) -> Value {
    let declared_type = definition
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();

    // Grouped declarations are provider-owned and forwarded verbatim.
    if is_provider_native_grouped_type(declared_type) {
        return definition.clone();
    }

    // An already-native custom declaration carries the grammar the provider
    // needs, so it is forwarded verbatim instead of being rebuilt without it.
    if declared_type == "custom" {
        return definition.clone();
    }

    if name == "apply_patch" {
        let description = definition
            .pointer("/function/description")
            .or_else(|| definition.get("description"))
            .and_then(Value::as_str)
            .unwrap_or("Apply one complete native apply_patch document.");
        return json!({
            "type": "custom",
            "name": "apply_patch",
            "description": description
        });
    }

    if matches!(declared_type, "web_search" | "web_search_2025_08_26") {
        return json!({ "type": "web_search" });
    }
    if declared_type != "function" {
        // Unknown declaration type: pass it through and let the provider rule.
        return definition.clone();
    }
    let function = definition.get("function").unwrap_or(definition);
    let description = function
        .get("description")
        .or_else(|| definition.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let parameters = function
        .get("parameters")
        .or_else(|| function.get("input_schema"))
        .or_else(|| definition.get("parameters"))
        .or_else(|| definition.get("input_schema"))
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
    let mut native = json!({
        "type": "function",
        "name": name,
        "description": description,
        "parameters": parameters
    });
    if let Some(strict) = function
        .get("strict")
        .or_else(|| definition.get("strict"))
        .and_then(Value::as_bool)
    {
        native["strict"] = Value::Bool(strict);
    }
    native
}

fn is_provider_web_search_definition(definition: &Value) -> bool {
    matches!(
        definition.get("type").and_then(Value::as_str),
        Some("web_search" | "web_search_2025_08_26")
    )
}

/// Provider-owned declarations that carry no callable name of their own. The
/// declaration type is their identity: `tool_search` is the single search entry
/// point the client resolves, and the endpoint never names it.
fn provider_native_identity(definition: &Value) -> Option<&'static str> {
    match definition.get("type").and_then(Value::as_str) {
        Some("tool_search") => Some("tool_search"),
        _ => None,
    }
}

fn tool_identity(definition: &Value) -> &str {
    definition
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| definition.pointer("/function/name").and_then(Value::as_str))
        .or_else(|| provider_native_identity(definition))
        .unwrap_or_default()
}

fn is_codeseex_local_tool(name: &str) -> bool {
    matches!(
        name,
        "web_search"
            | "list_directory"
            | "read_file_range"
            | "workspace_search"
            | "vision_analyze"
            | "image_gen"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeResponseTerminal {
    Completed,
    Failed,
    Incomplete,
}

/// Local lifecycle result selected only after the raw upstream stream has
/// closed. A provider terminal event wins over a late local cancellation; an
/// unterminated stream is never treated as completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeStreamFinalization {
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct NativeResponseStreamInspection {
    pub(crate) terminal: Option<NativeResponseTerminal>,
    pub(crate) final_usage: Option<Value>,
    pub(crate) provider_response_id_hash: Option<String>,
    pub(crate) event_count: usize,
    pub(crate) sequence_count: usize,
    pub(crate) sequences_strictly_increasing: bool,
    pub(crate) saw_done_sentinel: bool,
    pub(crate) oversized_frame_ignored: bool,
    /// Raw provider output items are retained only in RAM until the stream
    /// ends. They are used to recognise CodeSeeX-hosted calls and to count the
    /// provider's tool calls; no item content is put into diagnostics.
    pub(crate) output_items: Vec<Value>,
    pub(crate) output_items_bytes: usize,
    pub(crate) output_items_incomplete: bool,
}

pub(crate) fn native_stream_finalization(
    inspection: &NativeResponseStreamInspection,
    locally_cancelled: bool,
) -> NativeStreamFinalization {
    match inspection.terminal {
        Some(NativeResponseTerminal::Completed) => NativeStreamFinalization::Completed,
        Some(NativeResponseTerminal::Failed | NativeResponseTerminal::Incomplete) => {
            NativeStreamFinalization::Failed
        }
        None if locally_cancelled => NativeStreamFinalization::Interrupted,
        None => NativeStreamFinalization::Failed,
    }
}

/// Bounded SSE parser for side-channel accounting. `observe_bytes` never
/// returns transformed data and callers must forward their original bytes.
#[derive(Debug, Default)]
pub(crate) struct NativeResponseSseInspector {
    buffer: Vec<u8>,
    last_sequence: Option<u64>,
    inspection: NativeResponseStreamInspection,
}

impl NativeResponseSseInspector {
    /// Records that one frame could not be inspected. The frame itself is still
    /// forwarded; only CodeSeeX's own accounting is incomplete.
    pub(crate) fn mark_uninspectable_frame(&mut self) {
        self.inspection.oversized_frame_ignored = true;
        self.inspection.output_items_incomplete = true;
    }

    pub(crate) fn observe_bytes(&mut self, bytes: &[u8]) {
        if self.inspection.oversized_frame_ignored {
            return;
        }
        self.buffer.extend_from_slice(bytes);
        while let Some((index, delimiter_len)) = find_sse_frame_delimiter(&self.buffer) {
            if index > MAX_INSPECTED_SSE_FRAME_BYTES {
                self.buffer.drain(..index + delimiter_len);
                self.mark_uninspectable_frame();
                self.buffer.clear();
                break;
            }
            let frame = self.buffer.drain(..index).collect::<Vec<_>>();
            self.buffer.drain(..delimiter_len);
            self.inspect_frame(&frame);
        }
        if self.buffer.len() > MAX_INSPECTED_SSE_FRAME_BYTES {
            self.buffer.clear();
            self.mark_uninspectable_frame();
        }
    }

    /// Call once when the upstream stream closes. A final event is permitted
    /// to omit the trailing blank line, so it is inspected if still bounded.
    pub(crate) fn finish(&mut self) {
        if !self.inspection.oversized_frame_ignored && !self.buffer.is_empty() {
            let frame = std::mem::take(&mut self.buffer);
            self.inspect_frame(&frame);
        }
    }

    pub(crate) fn inspection(&self) -> &NativeResponseStreamInspection {
        &self.inspection
    }

    fn inspect_frame(&mut self, frame: &[u8]) {
        if frame.len() > MAX_INSPECTED_SSE_FRAME_BYTES {
            self.inspection.oversized_frame_ignored = true;
            self.inspection.output_items_incomplete = true;
            return;
        }
        let frame = match std::str::from_utf8(frame) {
            Ok(frame) => frame,
            Err(_) => {
                self.inspection.output_items_incomplete = true;
                return;
            }
        };
        let event_name = frame
            .lines()
            .find_map(|line| line.strip_prefix("event:").map(str::trim));
        let data = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        let data = data.trim();
        if data.is_empty() {
            return;
        }
        if data == "[DONE]" {
            self.inspection.saw_done_sentinel = true;
            return;
        }
        let payload = match serde_json::from_str::<Value>(data) {
            Ok(payload) => payload,
            Err(_) => {
                self.inspection.output_items_incomplete = true;
                return;
            }
        };
        self.inspection.event_count += 1;
        if let Some(sequence) = payload.get("sequence_number").and_then(Value::as_u64) {
            self.inspection.sequence_count += 1;
            if self
                .last_sequence
                .is_some_and(|previous| sequence <= previous)
            {
                self.inspection.sequences_strictly_increasing = false;
            } else if self.inspection.sequence_count == 1 {
                self.inspection.sequences_strictly_increasing = true;
            }
            self.last_sequence = Some(sequence);
        }
        if let Some(provider_id) = response_id_from_event(&payload) {
            self.inspection.provider_response_id_hash = Some(hash_identifier(provider_id));
        }
        if let Some(usage) = payload
            .pointer("/response/usage")
            .or_else(|| payload.get("usage"))
        {
            // Responses events contain snapshots, not deltas. Replacing is
            // therefore intentional and prevents token/cost multiplication.
            self.inspection.final_usage = Some(usage.clone());
        }
        let event_kind = event_name
            .or_else(|| payload.get("type").and_then(Value::as_str))
            .unwrap_or_default();
        if event_kind == "response.output_item.done" {
            match payload.get("item") {
                Some(item) => self.retain_output_item(item),
                None => self.inspection.output_items_incomplete = true,
            }
        } else if event_kind == "response.completed"
            && self.inspection.output_items.is_empty()
            && !self.inspection.output_items_incomplete
        {
            // Some compatible providers include the complete output only in
            // the terminal response. Accept that equivalent form, but never
            // merge it with already observed item-done events.
            if let Some(items) = payload
                .pointer("/response/output")
                .and_then(Value::as_array)
            {
                for item in items {
                    self.retain_output_item(item);
                }
            }
        }
        self.inspection.terminal = match event_kind {
            "response.completed" => Some(NativeResponseTerminal::Completed),
            "response.failed" => Some(NativeResponseTerminal::Failed),
            "response.incomplete" => Some(NativeResponseTerminal::Incomplete),
            _ => self.inspection.terminal,
        };
    }

    fn retain_output_item(&mut self, item: &Value) {
        if self.inspection.output_items_incomplete {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(item) else {
            self.inspection.output_items_incomplete = true;
            return;
        };
        if self.inspection.output_items.len() >= MAX_RETAINED_NATIVE_OUTPUT_ITEMS
            || self
                .inspection
                .output_items_bytes
                .saturating_add(bytes.len())
                > MAX_RETAINED_NATIVE_OUTPUT_BYTES
        {
            self.inspection.output_items_incomplete = true;
            return;
        }
        self.inspection.output_items_bytes += bytes.len();
        self.inspection.output_items.push(item.clone());
    }
}

/// Streaming counterpart to the inspector. It buffers only up to a complete
/// SSE event, observes the untouched upstream event, and rewrites the narrow
/// response-id boundary when one is present. All other event bytes, including
/// sequence numbers and tool/output ids, pass through unchanged.
#[derive(Debug)]
pub(crate) struct NativeResponseSseRelay {
    buffer: Vec<u8>,
    inspector: NativeResponseSseInspector,
    provider_response_id: Option<String>,
    local_response_id: String,
    present_reasoning_summary: bool,
    sequence_offset: u64,
    reasoning: ReasoningSummaryMirror,
}

impl NativeResponseSseRelay {
    pub(crate) fn new(local_response_id: impl Into<String>) -> Self {
        Self {
            buffer: Vec::new(),
            inspector: NativeResponseSseInspector::default(),
            provider_response_id: None,
            local_response_id: local_response_id.into(),
            present_reasoning_summary: false,
            sequence_offset: 0,
            reasoning: ReasoningSummaryMirror::default(),
        }
    }

    /// Presents provider `reasoning_text` as the summary Codex renders. The
    /// provider copy of every item is still what the inspector retains and what
    /// tool continuations are rebuilt from.
    pub(crate) fn with_reasoning_summary_presentation(mut self, enabled: bool) -> Self {
        self.present_reasoning_summary = enabled;
        self
    }

    pub(crate) fn relay_bytes(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut ready = Vec::new();
        self.buffer.extend_from_slice(bytes);
        while let Some((index, delimiter_len)) = find_sse_frame_delimiter(&self.buffer) {
            let frame = self.buffer.drain(..index).collect::<Vec<_>>();
            let delimiter = self.buffer.drain(..delimiter_len).collect::<Vec<_>>();
            if frame.len() > MAX_INSPECTED_SSE_FRAME_BYTES {
                // Too large to inspect, but the client stream must not lose it:
                // forward the provider frame unchanged and record that this
                // response could not be accounted for as one bounded group.
                self.inspector.mark_uninspectable_frame();
                ready.push(append_delimiter(frame, &delimiter));
                continue;
            }
            self.inspector.observe_bytes(&frame);
            self.inspector.observe_bytes(&delimiter);
            self.relay_frame(frame, delimiter, &mut ready);
        }
        if self.buffer.len() > MAX_INSPECTED_SSE_FRAME_BYTES {
            // The frame is still incomplete, so nothing can be inspected yet.
            // Record that this response will not be accounted for as one
            // bounded group even though the bytes stay buffered.
            self.inspector.mark_uninspectable_frame();
            // Past the hard cap it can neither be inspected nor completed, and
            // holding it would grow without bound.
            if self.buffer.len() > MAX_RELAYED_SSE_FRAME_BYTES {
                self.buffer.clear();
            }
        }
        ready
    }

    /// Flushes the trailing frame, if the upstream omitted its blank line. The
    /// presentation may add summary frames for that item, so every frame is
    /// returned instead of only the first.
    pub(crate) fn finish(&mut self) -> Vec<Vec<u8>> {
        let remainder = (!self.buffer.is_empty()).then(|| std::mem::take(&mut self.buffer));
        if let Some(bytes) = remainder.as_ref() {
            self.inspector.observe_bytes(bytes);
        }
        self.inspector.finish();
        // A bounded terminal SSE event may omit its trailing blank line. It
        // can still carry a provider response id, so apply the same narrow
        // identity rewrite before forwarding it.
        let mut ready = Vec::new();
        if let Some(frame) = remainder {
            self.relay_frame(frame, Vec::new(), &mut ready);
        }
        ready
    }

    pub(crate) fn inspection(&self) -> &NativeResponseStreamInspection {
        self.inspector.inspection()
    }

    fn relay_frame(&mut self, frame: Vec<u8>, delimiter: Vec<u8>, out: &mut Vec<Vec<u8>>) {
        let Ok(text) = std::str::from_utf8(&frame) else {
            out.push(append_delimiter(frame, &delimiter));
            return;
        };
        let data = text
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        let Ok(mut payload) = serde_json::from_str::<Value>(data.trim()) else {
            out.push(append_delimiter(frame, &delimiter));
            return;
        };
        if self.provider_response_id.is_none() {
            self.provider_response_id = response_id_from_event(&payload).map(str::to_owned);
        }
        let identity_rewritten =
            self.provider_response_id
                .as_deref()
                .is_some_and(|provider_response_id| {
                    rewrite_provider_response_identity(
                        &mut payload,
                        provider_response_id,
                        &self.local_response_id,
                    )
                });
        let base_sequence = payload.get("sequence_number").and_then(Value::as_u64);
        let event_type = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut injected = Vec::new();
        if self.present_reasoning_summary {
            if let Some(base_sequence) = base_sequence {
                self.mirror_reasoning_event(&mut payload, base_sequence, &mut injected);
            }
        }
        // These two carry the reasoning item itself, so the presentation edits
        // the payload and the frame has to be re-serialized even when the
        // response identity did not change.
        let mutated = self.present_reasoning_summary
            && matches!(
                event_type.as_str(),
                "response.output_item.added"
                    | "response.output_item.done"
                    | "response.completed"
            );
        let suppressed = self.suppress_provider_reasoning(&event_type, &payload);
        if !identity_rewritten && injected.is_empty() && !mutated && !suppressed {
            out.push(append_delimiter(frame, &delimiter));
            return;
        }
        if !suppressed {
            if let Some(base_sequence) = base_sequence {
                payload["sequence_number"] = json!(base_sequence + self.sequence_offset);
            }
            let serialized =
                serde_json::to_string(&payload).unwrap_or_else(|_| data.trim().to_owned());
            out.push(append_delimiter(
                rewrite_sse_data_lines(text, &serialized),
                &delimiter,
            ));
        }
        let injected_count = injected.len() as u64;
        for (index, mut event) in injected.into_iter().enumerate() {
            let Some(base_sequence) = base_sequence else {
                continue;
            };
            event["sequence_number"] = json!(base_sequence + self.sequence_offset + 1 + index as u64);
            let event_name = event
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            out.push(sse_frame(&event_name, &event));
        }
        self.sequence_offset += injected_count;
    }

    /// Re-announces provider `reasoning_text` as summary parts for the client.
    ///
    /// DeepSeek's native stream carries thinking as `reasoning_text` content,
    /// while Codex renders thinking from `summary_text`. The summary is an
    /// addition, never a replacement: the provider's `content` stays on the
    /// item, so the same history can be replayed either through CodeSeeX or on
    /// a direct connection. The mirrored summary is dropped again by the
    /// request boundary before anything reaches upstream.
    fn mirror_reasoning_event(
        &mut self,
        payload: &mut Value,
        _base_sequence: u64,
        injected: &mut Vec<Value>,
    ) {
        let event_type = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let output_index = payload
            .get("output_index")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let response_id = self.local_response_id.clone();
        match event_type.as_str() {
            "response.output_item.added" => {
                if payload.pointer("/item/type").and_then(Value::as_str) != Some("reasoning") {
                    return;
                }
                let Some(item_id) = payload
                    .pointer("/item/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                else {
                    return;
                };
                self.reasoning.begin(&item_id);
                let provider_summary = payload
                    .pointer("/item/summary")
                    .and_then(Value::as_array)
                    .is_some_and(|summary| !summary.is_empty());
                if provider_summary {
                    self.reasoning.mark_provider_summary(&item_id);
                }
                // The provider's own `content` is kept: the summary added below
                // is a presentation for Codex, not a replacement, so a history
                // that carries both shapes is still replayable on a direct
                // connection, which only understands `reasoning_text`.
            }
            "response.reasoning_summary_part.added"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_summary_part.done" => {
                // The provider writes the summary itself; leave that item alone.
                if let Some(item_id) = payload.get("item_id").and_then(Value::as_str) {
                    self.reasoning.mark_provider_summary(item_id);
                }
            }
            "response.content_part.added" => {
                let is_reasoning_text = payload.pointer("/part/type").and_then(Value::as_str)
                    == Some("reasoning_text");
                let Some(item_id) = payload
                    .get("item_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                else {
                    return;
                };
                if !is_reasoning_text {
                    return;
                }
                self.reasoning.begin(&item_id);
                if self.reasoning.provider_writes_summary(&item_id) {
                    return;
                }
                if self.present_reasoning_summary && !self.reasoning.part_announced {
                    self.reasoning.part_announced = true;
                    injected.push(reasoning_summary_part_added(
                        &response_id,
                        &item_id,
                        output_index,
                    ));
                }
            }
            "response.reasoning_text.delta" => {
                let Some(item_id) = payload
                    .get("item_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                else {
                    return;
                };
                let delta = payload
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.reasoning.begin(&item_id);
                if self.reasoning.provider_writes_summary(&item_id) {
                    return;
                }
                if !self.present_reasoning_summary {
                    return;
                }
                self.reasoning.push_delta(&item_id, delta);
                if !self.reasoning.part_announced {
                    self.reasoning.part_announced = true;
                    injected.push(reasoning_summary_part_added(
                        &response_id,
                        &item_id,
                        output_index,
                    ));
                }
                if !delta.is_empty() {
                    injected.push(reasoning_summary_text_delta(
                        &response_id,
                        &item_id,
                        output_index,
                        delta,
                    ));
                }
            }
            "response.reasoning_text.done" => {
                let Some(item_id) = payload
                    .get("item_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                else {
                    return;
                };
                self.reasoning.begin(&item_id);
                if self.reasoning.provider_writes_summary(&item_id) {
                    return;
                }
                if !self.present_reasoning_summary {
                    return;
                }
                self.finish_reasoning_summary(&response_id, &item_id, output_index, injected);
            }
            "response.output_item.done" => {
                let item_id = payload
                    .pointer("/item/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let is_reasoning =
                    payload.pointer("/item/type").and_then(Value::as_str) == Some("reasoning");
                let Some(item_id) = item_id else {
                    return;
                };
                if !is_reasoning {
                    return;
                }
                if self.reasoning.provider_writes_summary(&item_id) {
                    self.reasoning.finish_item(&item_id);
                    return;
                }
                let summary_empty = payload
                    .pointer("/item/summary")
                    .and_then(Value::as_array)
                    .map(Vec::is_empty)
                    .unwrap_or(true);
                if !summary_empty {
                    self.reasoning.finish_item(&item_id);
                    return;
                }
                let Some(text) = self.reasoning.text(&item_id).map(str::to_owned) else {
                    self.reasoning.finish_item(&item_id);
                    return;
                };
                if !self.present_reasoning_summary {
                    self.reasoning.finish_item(&item_id);
                    return;
                }
                self.finish_reasoning_summary(&response_id, &item_id, output_index, injected);
                payload["item"]["summary"] = Value::Array(vec![reasoning_summary_part(&text)]);
                self.reasoning.finish_item(&item_id);
            }
            "response.completed" => {
                let texts = self.reasoning.texts.clone();
                let Some(items) = payload
                    .pointer_mut("/response/output")
                    .and_then(Value::as_array_mut)
                else {
                    return;
                };
                for item in items.iter_mut() {
                    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
                        continue;
                    }
                    if !self.present_reasoning_summary {
                        continue;
                    }
                    let summary_empty = item
                        .get("summary")
                        .and_then(Value::as_array)
                        .map(Vec::is_empty)
                        .unwrap_or(true);
                    if !summary_empty {
                        continue;
                    }
                    let Some(item_id) = item.get("id").and_then(Value::as_str).map(str::to_owned)
                    else {
                        continue;
                    };
                    if self.reasoning.provider_writes_summary(&item_id) {
                        continue;
                    }
                    let Some(text) = texts.get(&item_id).filter(|text| !text.is_empty()) else {
                        continue;
                    };
                    item["summary"] = Value::Array(vec![reasoning_summary_part(text)]);
                }
            }
            _ => {}
        }
    }

    fn finish_reasoning_summary(
        &mut self,
        response_id: &str,
        item_id: &str,
        output_index: u64,
        injected: &mut Vec<Value>,
    ) {
        if !self.reasoning.part_announced {
            self.reasoning.part_announced = true;
            injected.push(reasoning_summary_part_added(
                response_id,
                item_id,
                output_index,
            ));
        }
        if self.reasoning.text_done {
            return;
        }
        self.reasoning.text_done = true;
        let Some(text) = self.reasoning.text(item_id).map(str::to_owned) else {
            return;
        };
        injected.push(reasoning_summary_text_done(
            response_id,
            item_id,
            output_index,
            &text,
        ));
        injected.push(reasoning_summary_part_done(
            response_id,
            item_id,
            output_index,
            &text,
        ));
    }

    /// Whether the client-facing presentation replaces this provider frame.
    ///
    /// Only reasoning content CodeSeeX presents as a summary is dropped. A
    /// provider that writes its own summary keeps its stream intact.
    fn suppress_provider_reasoning(&self, event_type: &str, payload: &Value) -> bool {
        if !self.present_reasoning_summary {
            return false;
        }
        match event_type {
            "response.content_part.added" | "response.content_part.done" => {
                payload.pointer("/part/type").and_then(Value::as_str) == Some("reasoning_text")
                    && payload
                        .get("item_id")
                        .and_then(Value::as_str)
                        .is_some_and(|item_id| !self.reasoning.provider_writes_summary(item_id))
            }
            "response.reasoning_text.delta" | "response.reasoning_text.done" => payload
                .get("item_id")
                .and_then(Value::as_str)
                .is_some_and(|item_id| !self.reasoning.provider_writes_summary(item_id)),
            _ => false,
        }
    }
}

/// Client-facing display mirror of provider reasoning.
///
/// Only item ids and the mirrored text live here; provider items are never
/// rebuilt from this state.
#[derive(Debug, Default)]
struct ReasoningSummaryMirror {
    active_item_id: Option<String>,
    part_announced: bool,
    text_done: bool,
    texts: BTreeMap<String, String>,
    /// Items where the provider writes the summary itself. Those keep their own
    /// presentation; CodeSeeX never rewrites or duplicates them.
    provider_summary_items: BTreeSet<String>,
}

impl ReasoningSummaryMirror {
    const MAX_TRACKED_ITEMS: usize = 8;

    fn begin(&mut self, item_id: &str) {
        if self.active_item_id.as_deref() != Some(item_id) {
            self.active_item_id = Some(item_id.to_owned());
            self.part_announced = false;
            self.text_done = false;
        }
        self.texts.entry(item_id.to_owned()).or_default();
        while self.texts.len() > Self::MAX_TRACKED_ITEMS {
            let Some(oldest) = self.texts.keys().next().cloned() else {
                break;
            };
            self.texts.remove(&oldest);
        }
    }

    fn push_delta(&mut self, item_id: &str, delta: &str) {
        if let Some(text) = self.texts.get_mut(item_id) {
            text.push_str(delta);
        }
    }

    fn text(&self, item_id: &str) -> Option<&str> {
        self.texts
            .get(item_id)
            .map(String::as_str)
            .filter(|text| !text.is_empty())
    }

    fn finish_item(&mut self, item_id: &str) {
        if self.active_item_id.as_deref() == Some(item_id) {
            self.active_item_id = None;
            self.part_announced = false;
            self.text_done = false;
        }
    }

    fn mark_provider_summary(&mut self, item_id: &str) {
        self.provider_summary_items.insert(item_id.to_owned());
        while self.provider_summary_items.len() > Self::MAX_TRACKED_ITEMS {
            let Some(oldest) = self.provider_summary_items.iter().next().cloned() else {
                break;
            };
            self.provider_summary_items.remove(&oldest);
        }
    }

    fn provider_writes_summary(&self, item_id: &str) -> bool {
        self.provider_summary_items.contains(item_id)
    }
}

fn reasoning_summary_part(text: &str) -> Value {
    json!({ "type": "summary_text", "text": text })
}

/// Non-streaming counterpart of the relay's reasoning presentation.
///
/// A provider that answers without streaming carries its thinking in the same
/// `reasoning_text` content parts, with an empty `summary`. The client copy
/// gains the summary shape Codex renders while keeping the provider's own
/// content; the request boundary drops the added summary before the item is
/// replayed upstream.
pub(crate) fn present_reasoning_summary_in_response(response: &mut Value, add_summary: bool) {
    if !add_summary {
        return;
    }
    let Some(items) = response.get_mut("output").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items.iter_mut() {
        if !item.is_object() || item.get("type").and_then(Value::as_str) != Some("reasoning") {
            continue;
        }
        let summary_empty = item
            .get("summary")
            .and_then(Value::as_array)
            .map(Vec::is_empty)
            .unwrap_or(true);
        if !summary_empty {
            continue;
        }
        let text = item
            .get("content")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter(|part| {
                        part.get("type").and_then(Value::as_str) == Some("reasoning_text")
                    })
                    .map(|part| {
                        part.get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                    })
                    .collect::<String>()
            })
            .unwrap_or_default();
        if !text.is_empty() {
            item["summary"] = Value::Array(vec![reasoning_summary_part(&text)]);
        }
    }
}

fn reasoning_summary_part_added(response_id: &str, item_id: &str, output_index: u64) -> Value {
    json!({
        "type": "response.reasoning_summary_part.added",
        "response_id": response_id,
        "item_id": item_id,
        "output_index": output_index,
        "summary_index": 0,
        "part": { "type": "summary_text", "text": "" }
    })
}

fn reasoning_summary_text_delta(
    response_id: &str,
    item_id: &str,
    output_index: u64,
    delta: &str,
) -> Value {
    json!({
        "type": "response.reasoning_summary_text.delta",
        "response_id": response_id,
        "item_id": item_id,
        "output_index": output_index,
        "summary_index": 0,
        "delta": delta
    })
}

fn reasoning_summary_text_done(
    response_id: &str,
    item_id: &str,
    output_index: u64,
    text: &str,
) -> Value {
    json!({
        "type": "response.reasoning_summary_text.done",
        "response_id": response_id,
        "item_id": item_id,
        "output_index": output_index,
        "summary_index": 0,
        "text": text
    })
}

fn reasoning_summary_part_done(
    response_id: &str,
    item_id: &str,
    output_index: u64,
    text: &str,
) -> Value {
    json!({
        "type": "response.reasoning_summary_part.done",
        "response_id": response_id,
        "item_id": item_id,
        "output_index": output_index,
        "summary_index": 0,
        "part": { "type": "summary_text", "text": text }
    })
}

/// Renders one locally mirrored SSE event. Upstream frames keep their original
/// bytes, fields and line endings.
fn sse_frame(event: &str, payload: &Value) -> Vec<u8> {
    let data = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_owned());
    let mut rendered = String::with_capacity(event.len() + data.len() + 16);
    rendered.push_str("event: ");
    rendered.push_str(event);
    rendered.push_str("\ndata: ");
    rendered.push_str(&data);
    rendered.push_str("\n\n");
    rendered.into_bytes()
}

/// Preserve every non-data SSE field byte-for-byte (`event`, `id`, `retry`,
/// comments, extensions and original line endings). The response-id boundary
/// only requires changing JSON carried by `data:`; rebuilding an entire event
/// would accidentally change SSE resume and reconnect behaviour.
fn rewrite_sse_data_lines(frame: &str, serialized_data: &str) -> Vec<u8> {
    let mut rendered = String::with_capacity(frame.len());
    let mut replaced = false;
    for raw_line in frame.split_inclusive('\n') {
        let (line, ending) = if let Some(line) = raw_line.strip_suffix("\r\n") {
            (line, "\r\n")
        } else if let Some(line) = raw_line.strip_suffix('\n') {
            (line, "\n")
        } else {
            (raw_line, "")
        };
        if line.strip_prefix("data:").is_some() {
            if !replaced {
                rendered.push_str("data: ");
                rendered.push_str(serialized_data);
                rendered.push_str(ending);
                replaced = true;
            }
            continue;
        }
        rendered.push_str(raw_line);
    }
    rendered.into_bytes()
}

fn append_delimiter(mut frame: Vec<u8>, delimiter: &[u8]) -> Vec<u8> {
    frame.extend_from_slice(delimiter);
    frame
}

fn response_id_from_event(payload: &Value) -> Option<&str> {
    payload
        .pointer("/response/id")
        .and_then(Value::as_str)
        .or_else(|| payload.get("response_id").and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
}

fn hash_identifier(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{:x}", digest)
}

fn find_sse_frame_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .enumerate()
        .find_map(|(index, window)| (window == b"\n\n").then_some((index, 2)));
    let crlf = buffer
        .windows(4)
        .enumerate()
        .find_map(|(index, window)| (window == b"\r\n\r\n").then_some((index, 4)));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_function(name: &str) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": name,
                "description": format!("{name} description"),
                "parameters": { "type": "object", "properties": {} }
            }
        })
    }

    #[test]
    fn local_web_search_stays_local_and_is_never_replaced_by_provider_search() {
        let plan = plan_native_tools(
            &[
                chat_function("web_search"),
                chat_function("workspace_search"),
            ],
            WebSearchBackend::Local,
        )
;

        assert!(plan.requires_local_execution);
        assert!(!plan.uses_official_web_search);
        assert_eq!(plan.tools.len(), 2);
        assert_eq!(plan.tools[0]["name"], "web_search");
        assert!(plan.tools.iter().all(|tool| tool["type"] != "web_search"));
    }

    #[test]
    fn official_web_search_replaces_only_the_local_web_function_without_double_dispatch() {
        let plan = plan_native_tools(
            &[
                chat_function("web_search"),
                chat_function("workspace_search"),
                chat_function("external_lookup"),
            ],
            WebSearchBackend::Official,
        )
;

        assert!(!plan.requires_local_execution);
        assert!(plan.uses_official_web_search);
        assert_eq!(
            plan.tools
                .iter()
                .filter(|tool| tool["type"] == "web_search")
                .count(),
            1
        );
        assert!(!plan.tools.iter().any(|tool| tool["name"] == "web_search"));
        assert!(plan
            .tools
            .iter()
            .any(|tool| tool["name"] == "workspace_search"));
        assert!(plan
            .tools
            .iter()
            .any(|tool| tool["name"] == "external_lookup"));
    }

    #[test]
    fn official_web_search_also_replaces_the_preview_alias() {
        let plan = plan_native_tools(
            &[chat_function("web_search_preview")],
            WebSearchBackend::Official,
        )
;

        assert!(!plan.requires_local_execution);
        assert!(plan.uses_official_web_search);
        assert_eq!(plan.tools, vec![json!({ "type": "web_search" })]);
    }

    #[test]
    fn official_backend_without_a_web_search_request_does_not_claim_search_ownership() {
        let plan = plan_native_tools(
            &[chat_function("workspace_search")],
            WebSearchBackend::Official,
        )
;

        assert!(!plan.requires_local_execution);
        assert!(!plan.uses_official_web_search);
        assert_eq!(plan.tools[0]["name"], "workspace_search");
    }

    #[test]
    fn provider_native_web_search_maps_to_the_existing_local_function() {
        let plan = plan_native_tools(
            &[chat_function("web_search"), json!({ "type": "web_search" })],
            WebSearchBackend::Local,
        )
;

        assert!(plan.requires_local_execution);
        assert!(!plan.uses_official_web_search);
        assert_eq!(
            plan.tools
                .iter()
                .filter(|tool| tool["name"] == "web_search")
                .count(),
            1
        );
        assert!(plan.tools.iter().all(|tool| tool["type"] != "web_search"));
    }

    #[test]
    fn provider_native_web_search_without_the_local_function_asks_for_the_hosted_executor() {
        let plan =
            plan_native_tools(&[json!({ "type": "web_search" })], WebSearchBackend::Local);

        assert!(plan.requires_local_execution);
        assert!(!plan.uses_official_web_search);
        assert!(plan.tools.is_empty());
    }

    #[test]
    fn apply_patch_is_converted_to_provider_custom_schema_without_parameter_wrapper() {
        let plan =
            plan_native_tools(&[chat_function("apply_patch")], WebSearchBackend::Local);

        assert!(!plan.requires_local_execution);
        assert_eq!(
            plan.tools,
            vec![json!({
                "type": "custom",
                "name": "apply_patch",
                "description": "apply_patch description"
            })]
        );
    }

    #[test]
    fn parses_the_verified_function_and_custom_tool_item_shapes_without_reordering() {
        let function = json!({
            "id": "fc_item",
            "type": "function_call",
            "status": "completed",
            "call_id": "call_function",
            "name": "workspace_search",
            "arguments": "{\"query\":\"needle\"}"
        });
        let custom = json!({
            "id": "ctc_item",
            "type": "custom_tool_call",
            "status": "completed",
            "call_id": "call_patch",
            "name": "apply_patch",
            "input": "*** Begin Patch\n*** End Patch"
        });
        let response = json!({
            "output": [
                { "id": "reasoning_1", "type": "reasoning", "status": "completed" },
                function,
                custom
            ]
        });

        let group = native_tool_call_group_from_response(&response)
            .unwrap()
            .expect("tool group");

        assert_eq!(
            group.provider_output,
            response["output"].as_array().expect("output array").clone()
        );
        assert_eq!(group.calls.len(), 2);
        assert_eq!(group.calls[0].call_id, "call_function");
        assert_eq!(group.calls[0].name, "workspace_search");
        assert_eq!(group.calls[0].input, r#"{"query":"needle"}"#);
        assert_eq!(group.calls[0].kind, NativeToolCallKind::Function);
        assert_eq!(group.calls[1].call_id, "call_patch");
        assert_eq!(group.calls[1].input, "*** Begin Patch\n*** End Patch");
        assert_eq!(group.calls[1].kind, NativeToolCallKind::Custom);
    }

    #[test]
    fn native_tool_continuation_requires_the_complete_provider_call_group_in_order() {
        let response = json!({
            "output": [
                {
                    "id": "fc_item",
                    "type": "function_call",
                    "status": "completed",
                    "call_id": "call_function",
                    "name": "workspace_search",
                    "arguments": "{}"
                },
                {
                    "id": "ctc_item",
                    "type": "custom_tool_call",
                    "status": "completed",
                    "call_id": "call_patch",
                    "name": "apply_patch",
                    "input": "*** Begin Patch\n*** End Patch"
                }
            ]
        });
        let group = native_tool_call_group_from_response(&response)
            .unwrap()
            .expect("tool group");
        let outputs = group
            .calls
            .iter()
            .map(|call| native_tool_output_item(call, format!("result: {}", call.name)))
            .collect::<Vec<_>>();
        let prefix = vec![json!({ "type": "message", "role": "user", "content": [] })];

        let continuation = append_complete_native_tool_group(&prefix, &group, &outputs).unwrap();
        assert_eq!(continuation[0], prefix[0]);
        assert_eq!(continuation[1], response["output"][0]);
        assert_eq!(continuation[2], response["output"][1]);
        assert_eq!(continuation[3]["type"], "function_call_output");
        assert_eq!(continuation[4]["type"], "custom_tool_call_output");

        assert!(append_complete_native_tool_group(&prefix, &group, &outputs[..1]).is_err());
        assert!(append_complete_native_tool_group(
            &prefix,
            &group,
            &[outputs[1].clone(), outputs[0].clone()]
        )
        .is_err());
        assert!(append_complete_native_tool_group(
            &prefix,
            &group,
            &[
                json!({ "type": "function_call_output", "call_id": "call_function", "output": "ok" }),
                json!({ "type": "function_call_output", "call_id": "call_patch", "output": "wrong kind" })
            ]
        )
        .is_err());
    }

    #[test]
    fn provider_owned_web_search_does_not_create_a_local_pending_tool_group() {
        let response = json!({
            "output": [{
                "id": "ws_item",
                "type": "web_search_call",
                "status": "completed"
            }]
        });

        assert_eq!(
            native_tool_call_group_from_response(&response).unwrap(),
            None
        );
    }

    #[test]
    fn malformed_calls_fail_closed_but_an_uncoordinated_custom_call_stays_provider_owned() {
        let missing_result_input = json!({
            "output": [{
                "type": "function_call",
                "status": "completed",
                "call_id": "call_function",
                "name": "workspace_search"
            }]
        });
        assert!(native_tool_call_group_from_response(&missing_result_input).is_err());

        // A custom grammar CodeSeeX does not coordinate (for example a tool a
        // newer Codex declares) is not an error: the item stays provider-owned
        // and is forwarded to the client untouched.
        let unsupported_custom = json!({
            "output": [{
                "type": "custom_tool_call",
                "status": "completed",
                "call_id": "call_custom",
                "name": "other_custom",
                "input": "unsafe"
            }]
        });
        assert_eq!(
            native_tool_call_group_from_response(&unsupported_custom).unwrap(),
            None
        );
    }

    #[test]
    fn unknown_tool_shapes_are_forwarded_instead_of_rejected() {
        let declaration = json!({ "type": "computer_use", "name": "computer" });
        let plan = plan_native_tools(&[declaration.clone()], WebSearchBackend::Local);

        assert_eq!(
            plan.tools,
            vec![declaration],
            "an unknown declaration is forwarded verbatim so the provider decides"
        );
        assert!(!plan.requires_local_execution);
    }

    #[test]
    fn codex_namespace_tools_keep_their_grouping_on_the_native_transport() {
        // Shape captured from a live Codex client: a namespace groups
        // short-named functions, and the endpoint echoes the namespace back on
        // the call item so the client can still resolve which tool ran.
        let namespace = json!({
            "type": "namespace",
            "name": "multi_agent_v1",
            "description": "Tools for spawning and managing sub-agents.",
            "tools": [
                {
                    "type": "function",
                    "name": "close_agent",
                    "description": "Close an agent.",
                    "strict": false,
                    "parameters": {
                        "type": "object",
                        "properties": { "target": { "type": "string" } },
                        "required": ["target"],
                        "additionalProperties": false
                    }
                },
                {
                    "type": "custom",
                    "name": "apply_patch",
                    "description": "Apply one complete patch document.",
                    "format": {
                        "type": "grammar",
                        "syntax": "lark",
                        "definition": "start: /.+/"
                    }
                }
            ]
        });

        let plan = plan_native_tools(
            &[chat_function("exec_command"), namespace.clone()],
            WebSearchBackend::Local,
        )
;

        assert!(!plan.requires_local_execution);
        assert!(!plan.uses_official_web_search);
        assert_eq!(plan.tools.len(), 2);
        assert_eq!(plan.tools[0]["name"], "exec_command");
        assert_eq!(plan.tools[1], namespace);
        assert_eq!(plan.tools[1]["tools"][0]["strict"], json!(false));
        assert_eq!(plan.tools[1]["tools"][1]["format"]["syntax"], json!("lark"));
    }

    #[test]
    fn repeated_namespace_declarations_merge_into_the_first_one() {
        let namespace = |tools: Value| {
            json!({
                "type": "namespace",
                "name": "codex_app",
                "description": "Tools provided by the Codex app.",
                "tools": tools
            })
        };
        let mut payload = json!({
            "tools": [namespace(json!([
                { "type": "function", "name": "fork_thread", "parameters": { "type": "object" } }
            ]))],
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "replayed namespace" }]
                },
                {
                    "type": "tool_search_output",
                    "call_id": "call_discovered",
                    "tools": [namespace(json!([
                        { "type": "function", "name": "fork_thread", "parameters": { "type": "object" } },
                        { "type": "function", "name": "read_thread", "parameters": { "type": "object" } }
                    ]))]
                }
            ]
        });

        let merged = reconcile_grouped_tool_namespaces(&mut payload);

        assert_eq!(merged, vec!["codex_app".to_owned()]);
        let declared = payload["tools"][0]["tools"].as_array().unwrap();
        assert_eq!(declared.len(), 2);
        assert_eq!(declared[0]["name"], "fork_thread");
        assert_eq!(declared[1]["name"], "read_thread");
        assert_eq!(payload["input"][1]["tools"], json!([]));
        assert_eq!(payload["input"][0]["type"], "message");
    }

    #[test]
    fn repeated_namespaces_inside_input_items_merge_without_losing_flat_tools() {
        let namespace = |tools: Value| {
            json!({
                "type": "namespace",
                "name": "codex_app",
                "description": "Tools provided by the Codex app.",
                "tools": tools
            })
        };
        let mut payload = json!({
            "input": [
                { "type": "message", "role": "user", "content": [] },
                {
                    "type": "tool_search_output",
                    "call_id": "call_one",
                    "tools": [namespace(json!([
                        { "type": "function", "name": "fork_thread", "parameters": { "type": "object" } }
                    ]))]
                },
                {
                    "type": "tool_search_output",
                    "call_id": "call_two",
                    "tools": [
                        namespace(json!([
                            { "type": "function", "name": "fork_thread", "parameters": { "type": "object" } },
                            { "type": "function", "name": "read_thread", "parameters": { "type": "object" } }
                        ])),
                        { "type": "function", "name": "exec_command", "parameters": { "type": "object" } }
                    ]
                }
            ]
        });

        let merged = reconcile_grouped_tool_namespaces(&mut payload);

        assert_eq!(merged, vec!["codex_app".to_owned()]);
        let first = payload["input"][1]["tools"][0]["tools"].as_array().unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[1]["name"], "read_thread");
        let second = payload["input"][2]["tools"].as_array().unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0]["name"], "exec_command");
    }

    #[test]
    fn tool_schema_repair_completes_a_union_parameters_schema() {
        let mut payload = json!({
            "tools": [{
                "type": "namespace",
                "name": "codex_app",
                "description": "Tools provided by the Codex app.",
                "tools": [{
                    "type": "function",
                    "name": "automation_update",
                    "parameters": {
                        "oneOf": [{ "$ref": "#/$defs/__schema0" }],
                        "$defs": { "__schema0": { "type": "object", "properties": {} } }
                    }
                }]
            }]
        });

        let repaired = repair_provider_tool_schemas(&mut payload);

        assert_eq!(repaired, vec!["automation_update".to_owned()]);
        let parameters = &payload["tools"][0]["tools"][0]["parameters"];
        assert_eq!(parameters["type"], "object");
        assert_eq!(parameters["oneOf"][0]["$ref"], "#/$defs/__schema0");
        assert_eq!(parameters["$defs"]["__schema0"]["type"], "object");
    }

    #[test]
    fn tool_schema_repair_leaves_declared_types_alone() {
        let payload = json!({
            "tools": [
                { "type": "function", "name": "shell_command", "parameters": { "type": "object", "properties": {} } },
                { "type": "function", "name": "odd_tool", "parameters": { "type": "string" } },
                { "type": "custom", "name": "apply_patch", "format": { "type": "grammar" } }
            ]
        });
        let mut forwarded = payload.clone();

        assert!(repair_provider_tool_schemas(&mut forwarded).is_empty());
        assert_eq!(forwarded, payload);
    }

    #[test]
    fn tool_schema_repair_covers_replayed_declarations_and_missing_schemas() {
        let mut payload = json!({
            "input": [
                { "type": "message", "role": "user", "content": [] },
                {
                    "type": "tool_search_output",
                    "call_id": "call_discovered",
                    "tools": [
                        { "type": "function", "name": "automation_update", "parameters": { "oneOf": [] } },
                        { "type": "function", "name": "null_schema_tool", "parameters": null },
                        { "type": "function", "name": "no_schema_tool" }
                    ]
                }
            ]
        });

        let repaired = repair_provider_tool_schemas(&mut payload);

        assert_eq!(
            repaired,
            vec![
                "automation_update".to_owned(),
                "no_schema_tool".to_owned(),
                "null_schema_tool".to_owned()
            ]
        );
        let tools = payload["input"][1]["tools"].as_array().unwrap();
        assert_eq!(
            tools[0]["parameters"],
            json!({ "oneOf": [], "type": "object" })
        );
        assert_eq!(
            tools[1]["parameters"],
            json!({ "type": "object", "properties": {} })
        );
        assert_eq!(
            tools[2]["parameters"],
            json!({ "type": "object", "properties": {} })
        );
        assert_eq!(payload["input"][0]["type"], "message");
    }

    #[test]
    fn distinct_grouped_declarations_are_forwarded_byte_for_byte() {
        let payload = json!({
            "tools": [{
                "type": "namespace",
                "name": "codex_app",
                "description": "Tools provided by the Codex app.",
                "tools": [{ "type": "function", "name": "fork_thread", "parameters": { "type": "object" } }]
            }],
            "input": [
                { "type": "message", "role": "user", "content": [] },
                {
                    "type": "tool_search_output",
                    "call_id": "call_discovered",
                    "tools": [{
                        "type": "namespace",
                        "name": "mcp__node_repl",
                        "description": "Tools provided by the node_repl server.",
                        "tools": [{ "type": "function", "name": "js", "parameters": { "type": "object" } }]
                    }]
                }
            ]
        });
        let mut forwarded = payload.clone();

        assert!(reconcile_grouped_tool_namespaces(&mut forwarded).is_empty());
        assert_eq!(forwarded, payload);
    }

    #[test]
    fn provider_native_tool_search_declarations_pass_through_without_a_callable_name() {
        let declaration = json!({
            "type": "tool_search",
            "execution": "client",
            "description": "Search for deferred tools.",
            "parameters": {
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"],
                "additionalProperties": false
            }
        });

        let plan = plan_native_tools(&[declaration.clone()], WebSearchBackend::Local);

        assert!(!plan.requires_local_execution);
        assert_eq!(plan.tools, vec![declaration]);
    }

    #[test]
    fn native_custom_apply_patch_keeps_the_grammar_the_provider_needs() {
        let declaration = json!({
            "type": "custom",
            "name": "apply_patch",
            "description": "Apply one complete patch document.",
            "format": {
                "type": "grammar",
                "syntax": "lark",
                "definition": "start: /.+/"
            }
        });

        let plan = plan_native_tools(&[declaration.clone()], WebSearchBackend::Local);

        assert!(!plan.requires_local_execution);
        assert_eq!(plan.tools, vec![declaration]);
    }

    #[test]
    fn malformed_or_unowned_grouped_declarations_are_forwarded() {
        let cases = [
            (
                json!({ "type": "namespace", "name": "mcp__node_repl" }),
                "did not declare a non-empty nested tool list",
            ),
            (
                json!({ "type": "namespace", "name": "mcp__node_repl", "tools": [] }),
                "did not declare a non-empty nested tool list",
            ),
            (
                json!({
                    "type": "namespace",
                    "name": "mcp__node_repl",
                    "tools": [{ "type": "web_search" }]
                }),
                "unsupported nested tool type 'web_search'",
            ),
            (
                json!({
                    "type": "namespace",
                    "name": "mcp__node_repl",
                    "tools": [{ "type": "function", "name": "  " }]
                }),
                "nested tool without a callable name",
            ),
            (
                json!({ "type": "tool_search", "parameters": { "type": "object" } }),
                "did not declare its execution mode",
            ),
            (
                json!({ "type": "tool_search", "execution": "client" }),
                "did not declare its parameters schema",
            ),
            (
                json!({ "type": "custom", "name": "other_custom" }),
                "cannot safely translate tool 'other_custom'",
            ),
        ];

        for (declaration, what) in cases {
            let plan = plan_native_tools(&[declaration.clone()], WebSearchBackend::Local);
            assert_eq!(
                plan.tools,
                vec![declaration.clone()],
                "a declaration CodeSeeX cannot translate ({what}) is forwarded verbatim"
            );
        }
    }

    #[test]
    fn response_identity_mapping_never_rewrites_tool_or_output_item_ids() {
        let mut payload = json!({
            "type": "response.output_item.done",
            "response_id": "resp_provider",
            "response": { "id": "resp_provider", "previous_response_id": null },
            "item": { "id": "msg_provider", "call_id": "call_provider" }
        });

        assert!(rewrite_provider_response_identity(
            &mut payload,
            "resp_provider",
            "resp_local"
        ));
        assert_eq!(payload["response_id"], "resp_local");
        assert_eq!(payload["response"]["id"], "resp_local");
        assert_eq!(payload["item"]["id"], "msg_provider");
        assert_eq!(payload["item"]["call_id"], "call_provider");
        assert!(!rewrite_provider_response_identity(
            &mut payload,
            "resp_provider",
            "resp_local"
        ));
    }

    #[test]
    fn relay_only_rewrites_response_identity_and_keeps_native_sse_protocol_facts() {
        let mut relay = NativeResponseSseRelay::new("resp_local");
        let first = concat!(
            ": keepalive\r\n",
            "id: upstream-event\r\n",
            "retry: 1500\r\n",
            "event: response.created\r\n",
            "data: {\"type\":\"response.created\",\"sequence_number\":1,",
            "\"response\":{\"id\":\"resp_provider\"}}\r\n\r\n"
        );
        let second = concat!(
            "event: response.output_item.done\r\n",
            "data: {\"type\":\"response.output_item.done\",\"sequence_number\":2,",
            "\"response_id\":\"resp_provider\",\"item\":{\"id\":\"msg_provider\",\"call_id\":\"call_provider\"}}\r\n\r\n"
        );

        assert!(relay.relay_bytes(&first.as_bytes()[..31]).is_empty());
        let mut ready = relay.relay_bytes(&first.as_bytes()[31..]);
        ready.extend(relay.relay_bytes(second.as_bytes()));
        assert_eq!(ready.len(), 2);
        assert!(!ready
            .iter()
            .any(|frame| frame.windows(6).any(|part| part == b"[DONE]")));

        let first_body = String::from_utf8(ready.remove(0)).unwrap();
        assert!(first_body.contains("\"id\":\"resp_local\""));
        assert!(first_body.contains("\"sequence_number\":1"));
        assert!(first_body.contains(": keepalive\r\n"));
        assert!(first_body.contains("id: upstream-event\r\n"));
        assert!(first_body.contains("retry: 1500\r\n"));
        assert!(first_body.contains("event: response.created\r\n"));
        assert!(first_body.ends_with("\r\n\r\n"));
        let second_body = String::from_utf8(ready.remove(0)).unwrap();
        assert!(second_body.contains("\"response_id\":\"resp_local\""));
        assert!(second_body.contains("\"id\":\"msg_provider\""));
        assert!(second_body.contains("\"call_id\":\"call_provider\""));
        assert!(second_body.contains("\"sequence_number\":2"));
        assert_eq!(relay.inspection().sequence_count, 2);
        assert!(relay.inspection().sequences_strictly_increasing);
    }

    #[test]
    fn observes_fragmented_native_sse_without_rewriting_or_accumulating_usage() {
        let mut inspector = NativeResponseSseInspector::default();
        let first = concat!(
            "event: response.created\r\n",
            "data: {\"type\":\"response.created\",\"sequence_number\":1,",
            "\"response\":{\"id\":\"resp_provider\",\"usage\":{\"input_tokens\":5}}}\r\n\r\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":2,\"response_id\":\"resp_provider\"}\n\n"
        );
        let terminal = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"sequence_number\":3,",
            "\"response\":{\"id\":\"resp_provider\",\"usage\":{\"input_tokens\":9,\"output_tokens\":2}}}\n\n"
        );

        // Splitting in the middle of JSON mimics reqwest chunk boundaries.
        inspector.observe_bytes(&first.as_bytes()[..79]);
        inspector.observe_bytes(&first.as_bytes()[79..]);
        inspector.observe_bytes(terminal.as_bytes());
        inspector.finish();

        let observed = inspector.inspection();
        assert_eq!(observed.terminal, Some(NativeResponseTerminal::Completed));
        assert_eq!(observed.event_count, 3);
        assert_eq!(observed.sequence_count, 3);
        assert!(observed.sequences_strictly_increasing);
        assert_eq!(observed.final_usage.as_ref().unwrap()["input_tokens"], 9);
        assert_eq!(observed.final_usage.as_ref().unwrap()["output_tokens"], 2);
        assert_eq!(
            observed.provider_response_id_hash.as_deref(),
            Some("5241e1b55519ba7c41cabb46af6f8692b82bfd5cd2a2665c8341434994a272d9")
        );
        assert!(!observed.saw_done_sentinel);
    }

    #[test]
    fn retains_completed_output_items_in_provider_event_order_for_tool_continuation() {
        let mut inspector = NativeResponseSseInspector::default();
        let stream = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"sequence_number\":1,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"status\":\"completed\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"sequence_number\":2,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_shell\",\"name\":\"shell_command\",\"arguments\":\"{}\",\"status\":\"completed\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"sequence_number\":3,\"response\":{\"id\":\"resp_provider\",\"status\":\"completed\"}}\n\n"
        );
        inspector.observe_bytes(stream.as_bytes());
        inspector.finish();

        let observed = inspector.inspection();
        assert_eq!(observed.output_items.len(), 2);
        assert_eq!(observed.output_items[0]["type"], "reasoning");
        assert_eq!(observed.output_items[1]["call_id"], "call_shell");
        assert!(observed.output_items_bytes > 0);
        assert!(!observed.output_items_incomplete);
    }

    #[test]
    fn detects_non_monotonic_sequence_and_terminal_failure_without_throwing() {
        let mut inspector = NativeResponseSseInspector::default();
        inspector.observe_bytes(
            br#"event: response.created
data: {"type":"response.created","sequence_number":3,"response_id":"resp_provider"}

event: response.failed
data: {"type":"response.failed","sequence_number":3,"response_id":"resp_provider"}

"#,
        );
        inspector.finish();

        let observed = inspector.inspection();
        assert_eq!(observed.terminal, Some(NativeResponseTerminal::Failed));
        assert_eq!(observed.sequence_count, 2);
        assert!(!observed.sequences_strictly_increasing);
    }

    #[test]
    fn cancelled_or_unterminated_native_stream_is_never_finalized_as_completed() {
        let mut relay = NativeResponseSseRelay::new("resp_local");
        relay.relay_bytes(
            br#"event: response.created
data: {"type":"response.created","sequence_number":1,"response":{"id":"resp_provider"}}

event: response.output_text.delta
data: {"type":"response.output_text.delta","sequence_number":2,"response_id":"resp_provider","delta":"partial"}
"#,
        );

        let trailing = relay
            .finish()
            .into_iter()
            .next()
            .expect("unterminated final SSE event");
        assert!(trailing.starts_with(b"event: response.output_text.delta"));
        assert_eq!(relay.inspection().terminal, None);
        assert_eq!(
            native_stream_finalization(relay.inspection(), true),
            NativeStreamFinalization::Interrupted
        );
        assert_eq!(
            native_stream_finalization(relay.inspection(), false),
            NativeStreamFinalization::Failed
        );

        let completed = NativeResponseStreamInspection {
            terminal: Some(NativeResponseTerminal::Completed),
            ..NativeResponseStreamInspection::default()
        };
        assert_eq!(
            native_stream_finalization(&completed, true),
            NativeStreamFinalization::Completed
        );
        let incomplete = NativeResponseStreamInspection {
            terminal: Some(NativeResponseTerminal::Incomplete),
            ..NativeResponseStreamInspection::default()
        };
        assert_eq!(
            native_stream_finalization(&incomplete, false),
            NativeStreamFinalization::Failed
        );
    }

    #[test]
    fn unterminated_terminal_frame_still_rewrites_provider_response_identity() {
        let mut relay = NativeResponseSseRelay::new("resp_local");
        relay.relay_bytes(
            br#"event: response.created
data: {"type":"response.created","response":{"id":"resp_provider"}}

"#,
        );
        relay.relay_bytes(
            br#"event: response.completed
data: {"type":"response.completed","response":{"id":"resp_provider","status":"completed"}}"#,
        );

        let trailing = String::from_utf8(
            relay
                .finish()
                .into_iter()
                .next()
                .expect("unterminated terminal frame"),
        )
        .unwrap();
        assert!(trailing.contains("resp_local"));
        assert!(!trailing.contains("resp_provider"));
        assert_eq!(
            relay.inspection().terminal,
            Some(NativeResponseTerminal::Completed)
        );
    }

    #[test]
    fn done_sentinel_and_oversized_unframed_data_are_observation_only() {
        let mut inspector = NativeResponseSseInspector::default();
        inspector.observe_bytes(b"data: [DONE]\n\n");
        assert!(inspector.inspection().saw_done_sentinel);

        inspector.observe_bytes(&vec![b'x'; MAX_INSPECTED_SSE_FRAME_BYTES + 1]);
        inspector.finish();
        assert!(inspector.inspection().oversized_frame_ignored);
        assert!(inspector.inspection().output_items_incomplete);
    }

    #[test]
    fn relay_still_forwards_an_oversized_frame_that_never_terminated_below_the_hard_cap() {
        let mut relay = NativeResponseSseRelay::new("resp_local");
        let mut oversized = b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_provider\"},\"padding\":\"".to_vec();
        oversized.extend(std::iter::repeat_n(
            b'x',
            MAX_INSPECTED_SSE_FRAME_BYTES.saturating_sub(oversized.len()) + 1,
        ));

        let frames = relay.relay_bytes(&oversized);

        assert!(
            frames.is_empty(),
            "an unterminated frame is buffered until it completes or the hard cap is reached"
        );
        assert!(relay.inspection().oversized_frame_ignored);
        assert!(relay.inspection().output_items_incomplete);

        let trailing = relay.finish();
        assert_eq!(
            trailing.len(),
            1,
            "the buffered frame is flushed at the end of the stream instead of vanishing"
        );
    }

    #[test]
    fn an_oversized_frame_is_forwarded_and_the_stream_continues() {
        let mut relay = NativeResponseSseRelay::new("resp_local");
        let prefix = b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_provider\"},\"padding\":\"";
        let mut frame = prefix.to_vec();
        frame.extend(std::iter::repeat_n(
            b'x',
            MAX_INSPECTED_SSE_FRAME_BYTES.saturating_sub(frame.len()) + 1,
        ));
        frame.extend_from_slice(b"\"}\n\n");

        let frames = relay.relay_bytes(&frame);

        assert_eq!(
            frames.len(),
            1,
            "the provider frame is forwarded even though it cannot be inspected"
        );
        assert!(frames[0].starts_with(b"event: response.completed"));
        assert!(relay.inspection().oversized_frame_ignored);
        assert!(relay.inspection().output_items_incomplete);
        assert_eq!(relay.inspection().terminal, None);
        assert_eq!(
            relay
                .relay_bytes(b"event: response.completed\ndata: {}\n\n")
                .len(),
            1,
            "later frames are still relayed instead of truncating the client stream"
        );
    }

    fn sse_frame_text(event: &str, payload: &Value) -> String {
        format!(
            "event: {event}\ndata: {}\n\n",
            serde_json::to_string(payload).unwrap()
        )
    }

    fn sequence_numbers(frames: &[String]) -> Vec<u64> {
        let mut numbers = Vec::new();
        for frame in frames {
            let mut cursor = 0_usize;
            while let Some(offset) = frame[cursor..].find("\"sequence_number\":") {
                let start = cursor + offset + "\"sequence_number\":".len();
                let digits = frame[start..]
                    .chars()
                    .take_while(|character| character.is_ascii_digit())
                    .collect::<String>();
                if let Ok(value) = digits.parse::<u64>() {
                    numbers.push(value);
                }
                cursor = start + digits.len().max(1);
                if cursor >= frame.len() {
                    break;
                }
            }
        }
        numbers
    }

    fn reasoning_text_frames() -> String {
        [
            sse_frame_text(
                "response.created",
                &json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": { "id": "resp_provider" }
                }),
            ),
            sse_frame_text(
                "response.output_item.added",
                &json!({
                    "type": "response.output_item.added",
                    "sequence_number": 2,
                    "response_id": "resp_provider",
                    "output_index": 0,
                    "item": {
                        "id": "rs_provider",
                        "type": "reasoning",
                        "status": "in_progress",
                        "content": [],
                        "summary": []
                    }
                }),
            ),
            sse_frame_text(
                "response.content_part.added",
                &json!({
                    "type": "response.content_part.added",
                    "sequence_number": 3,
                    "response_id": "resp_provider",
                    "item_id": "rs_provider",
                    "output_index": 0,
                    "content_index": 0,
                    "part": { "type": "reasoning_text", "text": "" }
                }),
            ),
            sse_frame_text(
                "response.reasoning_text.delta",
                &json!({
                    "type": "response.reasoning_text.delta",
                    "sequence_number": 4,
                    "response_id": "resp_provider",
                    "item_id": "rs_provider",
                    "output_index": 0,
                    "content_index": 0,
                    "delta": "think"
                }),
            ),
            sse_frame_text(
                "response.reasoning_text.done",
                &json!({
                    "type": "response.reasoning_text.done",
                    "sequence_number": 5,
                    "response_id": "resp_provider",
                    "item_id": "rs_provider",
                    "output_index": 0,
                    "content_index": 0,
                    "text": "think"
                }),
            ),
            sse_frame_text(
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done",
                    "sequence_number": 6,
                    "response_id": "resp_provider",
                    "output_index": 0,
                    "item": {
                        "id": "rs_provider",
                        "type": "reasoning",
                        "status": "completed",
                        "content": [{ "type": "reasoning_text", "text": "think" }],
                        "summary": [],
                        "encrypted_content": "blob"
                    }
                }),
            ),
            sse_frame_text(
                "response.completed",
                &json!({
                    "type": "response.completed",
                    "sequence_number": 7,
                    "response": {
                        "id": "resp_provider",
                        "output": [{
                            "id": "rs_provider",
                            "type": "reasoning",
                            "content": [{ "type": "reasoning_text", "text": "think" }],
                            "summary": []
                        }]
                    }
                }),
            ),
        ]
        .concat()
    }

    #[test]
    fn relay_presents_provider_reasoning_as_a_codex_summary() {
        let mut relay = NativeResponseSseRelay::new("resp_local")
            .with_reasoning_summary_presentation(true);
        let ready = relay.relay_bytes(reasoning_text_frames().as_bytes());
        let bodies = ready
            .iter()
            .map(|frame| String::from_utf8(frame.clone()).unwrap())
            .collect::<Vec<_>>();
        let joined = bodies.concat();

        // Codex renders summary_text, but the provider's own reasoning_text
        // stays on the item: the summary is added, never a replacement, so the
        // persisted history is still replayable on a direct connection.
        assert!(!joined.contains("event: response.reasoning_text.delta"));
        assert!(joined.contains("event: response.reasoning_summary_part.added"));
        assert!(joined.contains("event: response.reasoning_summary_text.delta"));
        assert!(joined.contains("event: response.reasoning_summary_text.done"));
        assert!(joined.contains("event: response.reasoning_summary_part.done"));
        assert!(
            joined.contains("\"part\":{\"text\":\"think\",\"type\":\"summary_text\"}"),
            "mirrored summary parts must carry the provider text: {joined}"
        );

        let item_done = bodies
            .iter()
            .find(|body| body.contains("event: response.output_item.done"))
            .expect("item done frame");
        assert!(item_done.contains("\"summary\":[{\"text\":\"think\",\"type\":\"summary_text\"}]"));
        assert!(
            item_done.contains("\"type\":\"reasoning_text\""),
            "the provider's own content must survive the presentation: {item_done}"
        );
        let completed = bodies
            .iter()
            .find(|body| body.contains("event: response.completed"))
            .expect("completed frame");
        assert!(completed.contains("\"summary\":[{\"text\":\"think\",\"type\":\"summary_text\"}]"));
        assert!(
            completed.contains("\"type\":\"reasoning_text\""),
            "the completed snapshot must keep the provider content: {completed}"
        );

        let sequences = sequence_numbers(&bodies);
        assert!(
            sequences.windows(2).all(|pair| pair[0] < pair[1]),
            "injected events must keep sequence numbers strictly increasing: {sequences:?}"
        );
        // The presentation never fabricates provider facts.
        assert!(!joined.contains("[DONE]"));
    }

    #[test]
    fn relay_reasoning_presentation_is_off_by_default() {
        let mut relay = NativeResponseSseRelay::new("resp_local");
        let ready = relay.relay_bytes(reasoning_text_frames().as_bytes());
        let joined = ready
            .iter()
            .map(|frame| String::from_utf8(frame.clone()).unwrap())
            .collect::<Vec<_>>()
            .concat();

        assert!(!joined.contains("reasoning_summary"));
        assert!(joined.contains("\"type\":\"reasoning_text\""));
    }

    #[test]
    fn non_streaming_reasoning_is_left_alone_when_the_summary_switch_is_off() {
        let mut response = json!({
            "id": "resp_provider",
            "output": [{
                "id": "rs_1",
                "type": "reasoning",
                "content": [{ "type": "reasoning_text", "text": "step one" }],
                "summary": []
            }]
        });

        present_reasoning_summary_in_response(&mut response, false);

        assert_eq!(response["output"][0]["summary"], json!([]));
        assert_eq!(
            response["output"][0]["content"][0]["type"],
            json!("reasoning_text")
        );
    }

    #[test]
    fn non_streaming_reasoning_is_presented_as_a_codex_summary() {
        let mut response = json!({
            "id": "resp_provider",
            "output": [
                {
                    "id": "rs_1",
                    "type": "reasoning",
                    "content": [{ "type": "reasoning_text", "text": "step one" }],
                    "summary": []
                },
                {
                    "id": "rs_2",
                    "type": "reasoning",
                    "content": [{ "type": "reasoning_text", "text": "other" }],
                    "summary": [{ "type": "summary_text", "text": "provider summary" }]
                },
                { "id": "msg_1", "type": "message", "role": "assistant", "content": [] }
            ]
        });

        present_reasoning_summary_in_response(&mut response, true);

        assert_eq!(
            response["output"][0]["summary"][0]["text"],
            json!("step one")
        );
        assert_eq!(
            response["output"][0]["content"][0]["type"],
            json!("reasoning_text"),
            "the provider's own content must survive the presentation"
        );
        assert_eq!(
            response["output"][0]["content"][0]["text"],
            json!("step one")
        );
        assert_eq!(
            response["output"][1]["summary"][0]["text"],
            json!("provider summary")
        );
        assert_eq!(response["output"][2]["type"], json!("message"));
    }

    #[test]
    fn relay_never_duplicates_a_provider_supplied_reasoning_summary() {
        let frames = [
            sse_frame_text(
                "response.created",
                &json!({
                    "type": "response.created",
                    "sequence_number": 1,
                    "response": { "id": "resp_provider" }
                }),
            ),
            sse_frame_text(
                "response.content_part.added",
                &json!({
                    "type": "response.content_part.added",
                    "sequence_number": 2,
                    "response_id": "resp_provider",
                    "item_id": "rs_provider",
                    "output_index": 0,
                    "content_index": 0,
                    "part": { "type": "summary_text", "text": "" }
                }),
            ),
            sse_frame_text(
                "response.reasoning_summary_text.delta",
                &json!({
                    "type": "response.reasoning_summary_text.delta",
                    "sequence_number": 3,
                    "response_id": "resp_provider",
                    "item_id": "rs_provider",
                    "output_index": 0,
                    "summary_index": 0,
                    "delta": "provider summary"
                }),
            ),
            sse_frame_text(
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done",
                    "sequence_number": 4,
                    "response_id": "resp_provider",
                    "output_index": 0,
                    "item": {
                        "id": "rs_provider",
                        "type": "reasoning",
                        "status": "completed",
                        "content": [],
                        "summary": [{ "type": "summary_text", "text": "provider summary" }]
                    }
                }),
            ),
        ]
        .concat();
        let mut relay = NativeResponseSseRelay::new("resp_local")
            .with_reasoning_summary_presentation(true);
        let ready = relay.relay_bytes(frames.as_bytes());
        let bodies = ready
            .iter()
            .map(|frame| String::from_utf8(frame.clone()).unwrap())
            .collect::<Vec<_>>();
        let joined = bodies.concat();

        assert_eq!(
            joined
                .matches("event: response.reasoning_summary_text.delta")
                .count(),
            1
        );
        assert_eq!(
            joined
                .matches("event: response.reasoning_summary_part.added")
                .count(),
            0
        );
        assert_eq!(sequence_numbers(&bodies), vec![1, 2, 3, 4]);
    }
}
