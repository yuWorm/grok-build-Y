//! Third-party Responses JSON sanitizer.
//!
//! `async-openai`'s Responses types treat `output_text.annotations` and
//! output-item `id` as required. xAI always sends them; OpenAI and many
//! gateways omit empty `annotations` or item ids. This module fills those
//! holes on the deserialize-failure path so the typed parse can succeed.
//!
//! Invoked only after a first `from_str` fails, so a well-formed xAI event
//! never runs this code. Filling missing fields is additive: a payload that
//! already has `annotations` / `id` is left unchanged.
//!
//! Driver ids below are the Pi-style names for the three existing
//! [`crate::ApiBackend`]s. Sampling still uses [`SamplingClient`]; new
//! native protocols (Gemini, Bedrock) would plug in here later.

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use xai_grok_sampling_types::{ApiBackend, ConversationItem, ConversationResponse, ToolCall, rs};

use crate::events::{SamplingChannel, SamplingEvent};

/// Pi-style driver id for a Grok `ApiBackend`.
#[allow(dead_code)]
pub fn driver_id_for_backend(backend: ApiBackend) -> &'static str {
    match backend {
        ApiBackend::ChatCompletions => "openai-completions",
        ApiBackend::Responses => "openai-responses",
        ApiBackend::Messages => "anthropic-messages",
    }
}

/// Output-item `type` values that the Responses schema requires an `id` on.
/// Deltas (`response.output_text.delta`, etc.) use other type strings and
/// are not touched.
const OUTPUT_ITEMS_REQUIRING_ID: &[&str] = &[
    "message",
    "reasoning",
    "function_call",
    "web_search_call",
    "file_search_call",
    "code_interpreter_call",
    "image_generation_call",
    "computer_call",
    "mcp_call",
    "mcp_list_tools",
    "custom_tool_call",
    "local_shell_call",
];

/// Patch a Responses SSE payload in place. Returns whether any field was
/// added.
pub(crate) fn sanitize_response_event_json(value: &mut Value) -> bool {
    let mut changed = false;
    let mut next_id = 0u64;
    walk(value, &mut changed, &mut next_id);
    changed
}

fn walk(value: &mut Value, changed: &mut bool, next_id: &mut u64) {
    match value {
        Value::Object(map) => {
            patch_object(map, changed, next_id);
            for child in map.values_mut() {
                walk(child, changed, next_id);
            }
        }
        Value::Array(arr) => {
            for child in arr {
                walk(child, changed, next_id);
            }
        }
        _ => {}
    }
}

fn patch_object(map: &mut Map<String, Value>, changed: &mut bool, next_id: &mut u64) {
    let Some(ty) = map.get("type").and_then(Value::as_str).map(str::to_owned) else {
        return;
    };
    if ty == "output_text" && !map.contains_key("annotations") {
        map.insert("annotations".into(), Value::Array(Vec::new()));
        *changed = true;
    }
    if OUTPUT_ITEMS_REQUIRING_ID.contains(&ty.as_str()) && !map.contains_key("id") {
        let id = format!("compat_{next_id}");
        *next_id += 1;
        map.insert("id".into(), Value::String(id));
        *changed = true;
    }
}

/// Fill Responses `usage` sub-objects Codex often omits so `response.completed`
/// can deserialize.
pub(crate) fn fill_response_usage_defaults(value: &mut Value) -> bool {
    let Some(usage) = value
        .pointer_mut("/response/usage")
        .and_then(Value::as_object_mut)
    else {
        return false;
    };
    let mut changed = false;
    if !usage.contains_key("input_tokens_details") {
        usage.insert(
            "input_tokens_details".into(),
            serde_json::json!({ "cached_tokens": 0 }),
        );
        changed = true;
    }
    if !usage.contains_key("output_tokens_details") {
        usage.insert(
            "output_tokens_details".into(),
            serde_json::json!({ "reasoning_tokens": 0 }),
        );
        changed = true;
    }
    changed
}

/// ChatGPT Codex Responses (`chatgpt.com/backend-api/codex`).
pub(crate) fn is_codex_responses_url(base_url: &str) -> bool {
    base_url.contains("chatgpt.com/backend-api/codex")
}

/// GROK_COMPAT_HOOK: call after grok fills typed Responses defaults.
pub(crate) fn prepare_codex_create_response(base_url: &str, req: &mut rs::CreateResponse) {
    if is_codex_responses_url(base_url) {
        adapt_codex_create_response(req);
    }
}

/// GROK_COMPAT_HOOK: last mutation of a serialized Responses body.
pub(crate) fn prepare_codex_request_json(base_url: &str, body: &mut Value) {
    if is_codex_responses_url(base_url) {
        sanitize_codex_request_json(body);
    }
}

/// GROK_COMPAT_HOOK: xAI-only request extras (`stream_tool_calls`, doom-loop).
pub(crate) fn allows_xai_request_extensions(base_url: &str) -> bool {
    !is_codex_responses_url(base_url)
}

/// GROK_COMPAT_HOOK: ChatGPT Codex session-affinity headers (no `x-grok-*`).
pub(crate) fn apply_codex_session_headers(
    mut builder: reqwest::RequestBuilder,
    session_id: &str,
) -> reqwest::RequestBuilder {
    if !session_id.is_empty() {
        builder = builder
            .header("session-id", session_id)
            .header("session_id", session_id)
            .header("conversation_id", session_id)
            .header("x-client-request-id", session_id);
    }
    builder
}

/// GROK_COMPAT_HOOK: lenient JSON on the Responses deserialize-failure path.
///
/// Fill missing `id` / `annotations` first; only then drop items the typed
/// schema still cannot parse (encrypted reasoning, extra types).
pub(crate) fn prepare_lenient_response_json(event_name: &str, value: &mut Value) {
    if let Some(obj) = value.as_object_mut()
        && !obj.contains_key("type")
        && event_name.starts_with("response.")
    {
        obj.insert("type".into(), Value::String(event_name.to_owned()));
    }
    sanitize_response_event_json(value);
    fill_response_usage_defaults(value);
    if let Some(output) = value
        .pointer_mut("/response/output")
        .and_then(|v| v.as_array_mut())
    {
        output.retain(|item| serde_json::from_value::<rs::OutputItem>(item.clone()).is_ok());
    }
}

/// Codex/OpenAI often stream `output_text.delta` but leave `response.output`
/// empty on the terminal snapshot. Fold streamed text into the assistant item
/// so the turn is not classified empty and retried.
pub(crate) fn inject_streaming_text_fallback(items: &mut Vec<ConversationItem>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(assistant) = items.iter_mut().rev().find_map(|item| match item {
        ConversationItem::Assistant(a) => Some(a),
        _ => None,
    }) {
        if assistant.content.is_empty() {
            assistant.content = std::sync::Arc::<str>::from(text);
        }
        return;
    }
    items.push(ConversationItem::assistant(text));
}

pub(crate) fn inject_streaming_tool_fallback(
    items: &mut Vec<ConversationItem>,
    streamed_tools: &BTreeMap<u32, (String, String, String)>,
) {
    if streamed_tools.is_empty() {
        return;
    }
    let tools: Vec<ToolCall> = streamed_tools
        .values()
        .map(|(id, name, arguments)| ToolCall {
            id: std::sync::Arc::<str>::from(id.as_str()),
            name: name.clone(),
            arguments: std::sync::Arc::<str>::from(arguments.as_str()),
        })
        .collect();
    if let Some(assistant) = items.iter_mut().rev().find_map(|item| match item {
        ConversationItem::Assistant(a) => Some(a),
        _ => None,
    }) {
        if assistant.tool_calls.is_empty() {
            assistant.tool_calls = tools;
        }
        return;
    }
    items.push(ConversationItem::assistant_tool_calls(tools));
}

pub(crate) fn has_streamed_visible_output(
    text: &str,
    streamed_tools: &BTreeMap<u32, (String, String, String)>,
) -> bool {
    !text.is_empty() || !streamed_tools.is_empty()
}

/// Visible assistant text / tool calls must not be resampled.
pub(crate) fn marks_replay_unsafe(event: &SamplingEvent) -> bool {
    matches!(
        event,
        SamplingEvent::ToolCallDelta { .. }
            | SamplingEvent::ChannelToken {
                channel: SamplingChannel::Text,
                ..
            }
    )
}

/// Streamed text that never landed in the terminal snapshot is still a reply.
pub(crate) fn accept_empty_snapshot(response: &ConversationResponse) -> bool {
    response.message_chunks_emitted > 0
}

/// Keys Pi's `openai-codex-responses` body is allowed to send. Everything
/// else (notably `max_output_tokens`, sampling knobs, xAI extras) 400s
/// on the ChatGPT Codex backend.
const CODEX_BODY_KEYS: &[&str] = &[
    "model",
    "store",
    "stream",
    "instructions",
    "input",
    "text",
    "include",
    "prompt_cache_key",
    "tool_choice",
    "parallel_tool_calls",
    "tools",
    "reasoning",
    "service_tier",
];

/// Pi-shaped typed request: hoist system → `instructions`, drop ChatGPT-
/// rejected sampling fields, keep reasoning only when an effort is set.
pub(crate) fn adapt_codex_create_response(req: &mut rs::CreateResponse) {
    let mut system_chunks = Vec::new();
    if let rs::InputParam::Items(items) = &mut req.input {
        items.retain(|item| {
            let Some(text) = system_message_text(item) else {
                return true;
            };
            if !text.is_empty() {
                system_chunks.push(text);
            }
            false
        });
    }
    req.instructions = Some(if system_chunks.is_empty() {
        "You are a helpful assistant.".to_owned()
    } else {
        system_chunks.join("\n\n")
    });
    req.store = Some(false);
    req.parallel_tool_calls = Some(true);
    if req.tool_choice.is_none() {
        req.tool_choice = Some(rs::ToolChoiceParam::Mode(rs::ToolChoiceOptions::Auto));
    }
    req.temperature = None;
    req.top_p = None;
    req.top_logprobs = None;
    req.max_output_tokens = None;
    req.max_tool_calls = None;
    req.truncation = None;
    req.background = None;
    req.conversation = None;
    req.metadata = None;
    req.previous_response_id = None;
    req.prompt = None;
    req.prompt_cache_retention = None;
    req.safety_identifier = None;
    req.stream_options = None;
    req.reasoning = match req.reasoning.take() {
        Some(reasoning) if reasoning.effort.is_some() => Some(rs::Reasoning {
            effort: reasoning.effort,
            summary: Some(rs::ReasoningSummary::Auto),
        }),
        _ => None,
    };
}

/// Last-line filter after serialize + extra-field splice. `apply_response_defaults`
/// and `stream_tool_calls` must not be able to put a rejected key back.
pub(crate) fn sanitize_codex_request_json(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    obj.retain(|key, _| CODEX_BODY_KEYS.contains(&key.as_str()));
    if !obj.contains_key("instructions") {
        obj.insert(
            "instructions".into(),
            Value::String("You are a helpful assistant.".into()),
        );
    }
    obj.insert("store".into(), Value::Bool(false));
    obj.insert("parallel_tool_calls".into(), Value::Bool(true));

    match obj.get("text") {
        Some(text) if text.get("format").is_some() => {
            if let Some(text_obj) = obj.get_mut("text").and_then(Value::as_object_mut) {
                text_obj.retain(|key, _| key == "format" || key == "verbosity");
                text_obj
                    .entry("verbosity")
                    .or_insert_with(|| Value::String("low".into()));
            }
        }
        _ => {
            obj.insert("text".into(), serde_json::json!({ "verbosity": "low" }));
        }
    }

    let include = obj
        .entry("include")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(items) = include.as_array_mut() {
        items.retain(|v| v.as_str() == Some("reasoning.encrypted_content"));
        if !items
            .iter()
            .any(|v| v.as_str() == Some("reasoning.encrypted_content"))
        {
            items.push(Value::String("reasoning.encrypted_content".into()));
        }
    }

    let effort = obj
        .get("reasoning")
        .and_then(|r| r.get("effort"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    match effort {
        Some(effort) => {
            obj.insert(
                "reasoning".into(),
                serde_json::json!({ "effort": effort, "summary": "auto" }),
            );
        }
        None => {
            obj.remove("reasoning");
        }
    }

    if obj
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        obj.remove("tools");
    }

    if let Some(input) = obj.get_mut("input").and_then(Value::as_array_mut) {
        transform_codex_input_items(input);
    }
}

const CODEX_INTERRUPTED_TOOL_OUTPUT: &str =
    "[No tool output recorded: the tool call was interrupted before it produced a result.]";
const CODEX_ORPHAN_OUTPUT_LIMIT: usize = 16_000;

fn item_type(item: &Value) -> Option<&str> {
    item.get("type").and_then(Value::as_str)
}

fn item_call_id(item: &Value) -> Option<&str> {
    item.get("call_id").and_then(Value::as_str)
}

fn is_tool_call_type(ty: &str) -> bool {
    matches!(ty, "function_call" | "custom_tool_call")
}

fn is_tool_output_type(ty: &str) -> bool {
    matches!(ty, "function_call_output" | "custom_tool_call_output")
}

fn output_type_for_call(ty: &str) -> &'static str {
    match ty {
        "custom_tool_call" => "custom_tool_call_output",
        _ => "function_call_output",
    }
}

fn matching_output_type(call_ty: &str, output_ty: &str) -> bool {
    matches!(
        (call_ty, output_ty),
        ("function_call", "function_call_output") | ("custom_tool_call", "custom_tool_call_output")
    )
}

fn orphan_output_to_message(item: &Value, call_id: &str) -> Value {
    let tool_name = item.get("name").and_then(Value::as_str).unwrap_or("tool");
    let mut text = match item.get("output") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    if text.len() > CODEX_ORPHAN_OUTPUT_LIMIT {
        text.truncate(CODEX_ORPHAN_OUTPUT_LIMIT);
        text.push_str("\n...[truncated]");
    }
    serde_json::json!({
        "type": "message",
        "role": "assistant",
        "content": format!("[Previous {tool_name} result; call_id={call_id}]: {text}")
    })
}

/// Oh My Pi `filterInput` + `repairToolCallPairs`: drop `item_reference`,
/// strip client item ids, and pair orphaned function calls/outputs so the
/// ChatGPT Codex backend does not 400 mid-turn.
fn transform_codex_input_items(input: &mut Vec<Value>) {
    input.retain(|item| item_type(item) != Some("item_reference"));
    for item in input.iter_mut() {
        if let Some(obj) = item.as_object_mut() {
            obj.remove("id");
        }
    }

    let mut call_types: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut output_types: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for item in input.iter() {
        let Some(call_id) = item_call_id(item).map(str::to_owned) else {
            continue;
        };
        let Some(ty) = item_type(item) else {
            continue;
        };
        if is_tool_call_type(ty) {
            call_types.insert(call_id.clone(), ty.to_owned());
        }
        if is_tool_output_type(ty) {
            output_types.insert(call_id, ty.to_owned());
        }
    }

    let mut repaired = Vec::with_capacity(input.len());
    for item in input.drain(..) {
        let call_id = item_call_id(&item).map(str::to_owned);
        let ty = item_type(&item).map(str::to_owned);
        if let (Some(call_id), Some(ty)) = (call_id.as_deref(), ty.as_deref()) {
            if is_tool_output_type(ty)
                && call_types
                    .get(call_id)
                    .is_none_or(|call_ty| !matching_output_type(call_ty, ty))
            {
                repaired.push(orphan_output_to_message(&item, call_id));
                continue;
            }
            if is_tool_call_type(ty)
                && output_types
                    .get(call_id)
                    .is_none_or(|out_ty| !matching_output_type(ty, out_ty))
            {
                repaired.push(item);
                repaired.push(serde_json::json!({
                    "type": output_type_for_call(ty),
                    "call_id": call_id,
                    "output": CODEX_INTERRUPTED_TOOL_OUTPUT,
                }));
                continue;
            }
        }
        repaired.push(item);
    }
    *input = repaired;
}

fn system_message_text(item: &rs::InputItem) -> Option<String> {
    let rs::InputItem::EasyMessage(msg) = item else {
        return None;
    };
    if !matches!(msg.role, rs::Role::System | rs::Role::Developer) {
        return None;
    }
    Some(match &msg.content {
        rs::EasyInputContent::Text(text) => text.clone(),
        rs::EasyInputContent::ContentList(parts) => parts
            .iter()
            .filter_map(|part| match part {
                rs::InputContent::InputText(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::sanitize_response_event_json;
    use serde_json::{Value, json};

    fn xai_completed() -> Value {
        json!({
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_test",
                "object": "response",
                "created_at": 1234567890,
                "model": "grok-4.6",
                "status": "completed",
                "output": [{
                    "type": "message",
                    "id": "msg_test",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": "hello",
                        "annotations": []
                    }]
                }]
            }
        })
    }

    fn openai_completed_missing_fields() -> Value {
        json!({
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_gpt",
                "object": "response",
                "created_at": 1234567890,
                "model": "gpt-5.6",
                "status": "completed",
                "output": [
                    {
                        "type": "reasoning",
                        "summary": [{ "type": "summary_text", "text": "think" }],
                        "status": "completed"
                    },
                    {
                        "type": "message",
                        "role": "assistant",
                        "status": "completed",
                        "content": [{
                            "type": "output_text",
                            "text": "Hi. What can I help you with?"
                        }]
                    }
                ]
            }
        })
    }

    #[test]
    fn xai_shaped_payload_is_unchanged() {
        let mut value = xai_completed();
        let before = value.clone();
        assert!(!sanitize_response_event_json(&mut value));
        assert_eq!(value, before);
    }

    #[test]
    fn is_codex_url_matches_chatgpt_backend() {
        assert!(super::is_codex_responses_url(
            "https://chatgpt.com/backend-api/codex"
        ));
        assert!(super::is_codex_responses_url(
            "https://chatgpt.com/backend-api/codex/"
        ));
        assert!(!super::is_codex_responses_url("https://api.openai.com/v1"));
        assert!(!super::is_codex_responses_url("https://api.x.ai/v1"));
    }

    #[test]
    fn codex_hoists_system_messages_out_of_input() {
        use xai_grok_sampling_types::{ConversationItem, ConversationRequest, rs};

        let req = ConversationRequest::from_items(vec![
            ConversationItem::system("You are Codex."),
            ConversationItem::user("hi"),
        ])
        .with_model("gpt-5.6-terra")
        .with_temperature(0.2)
        .with_max_output_tokens(128_000);
        let mut body: rs::CreateResponse = (&req).into();
        super::adapt_codex_create_response(&mut body);
        let rs::InputParam::Items(items) = body.input else {
            panic!("expected items");
        };
        assert_eq!(items.len(), 1, "system item must leave input");
        assert_eq!(body.instructions.as_deref(), Some("You are Codex."));
        assert!(body.temperature.is_none());
        assert!(body.top_p.is_none());
        assert!(body.max_output_tokens.is_none());
        assert_eq!(body.store, Some(false));
        assert_eq!(body.parallel_tool_calls, Some(true));
        assert!(body.reasoning.is_none());
    }

    #[test]
    fn codex_json_allowlist_drops_rejected_parameters() {
        let mut body = json!({
            "model": "gpt-5.6-terra",
            "max_output_tokens": 128000,
            "temperature": 0.7,
            "top_p": 1.0,
            "stream_tool_calls": true,
            "truncation": "auto",
            "stream_options": { "include_usage": true },
            "input": [{ "role": "user", "content": "hi" }],
            "stream": true,
            "reasoning": { "summary": "concise" }
        });
        super::sanitize_codex_request_json(&mut body);
        assert!(body.get("max_output_tokens").is_none());
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert!(body.get("stream_tool_calls").is_none());
        assert!(body.get("truncation").is_none());
        assert!(body.get("stream_options").is_none());
        assert!(body.get("reasoning").is_none());
        assert_eq!(body["store"], false);
        assert_eq!(body["parallel_tool_calls"], true);
        assert_eq!(body["text"]["verbosity"], "low");
        assert_eq!(body["instructions"], "You are a helpful assistant.");
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["model"], "gpt-5.6-terra");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn codex_repairs_orphaned_tool_call_and_output() {
        let mut body = json!({
            "model": "gpt-5.6-terra",
            "input": [
                { "type": "function_call", "call_id": "c1", "name": "read_file", "arguments": "{}", "id": "fc_drop" },
                { "type": "function_call_output", "call_id": "c2", "output": "stale result" },
                { "type": "item_reference", "id": "msg_old" },
                { "type": "message", "role": "user", "content": "hi", "id": "msg_1" }
            ],
            "stream": true
        });
        super::sanitize_codex_request_json(&mut body);
        let input = body["input"].as_array().expect("input");
        assert!(
            input.iter().all(|item| item.get("id").is_none()
                && item.get("type") != Some(&json!("item_reference"))),
            "{input:?}"
        );
        assert!(
            input.iter().any(|item| {
                item.get("type") == Some(&json!("function_call_output"))
                    && item.get("call_id") == Some(&json!("c1"))
            }),
            "orphaned function_call must get a synthetic output: {input:?}"
        );
        assert!(
            input.iter().any(|item| {
                item.get("role") == Some(&json!("assistant"))
                    && item
                        .get("content")
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| s.contains("call_id=c2"))
            }),
            "orphaned function_call_output must fold into an assistant note: {input:?}"
        );
    }

    #[test]
    fn fills_missing_annotations_and_ids() {
        let mut value = openai_completed_missing_fields();
        assert!(sanitize_response_event_json(&mut value));

        let reasoning = &value["response"]["output"][0];
        assert_eq!(reasoning["id"], "compat_0");

        let message = &value["response"]["output"][1];
        assert_eq!(message["id"], "compat_1");
        assert_eq!(message["content"][0]["annotations"], json!([]));
        assert_eq!(
            message["content"][0]["text"],
            "Hi. What can I help you with?"
        );
    }

    #[test]
    fn does_not_invent_id_on_delta_events() {
        let mut value = json!({
            "type": "response.output_text.delta",
            "sequence_number": 0,
            "item_id": "item_test",
            "output_index": 0,
            "content_index": 0,
            "delta": "hello"
        });
        let before = value.clone();
        assert!(!sanitize_response_event_json(&mut value));
        assert_eq!(value, before);
        assert!(value.get("id").is_none());
    }

    #[test]
    fn fills_annotations_on_content_part() {
        let mut value = json!({
            "type": "response.content_part.done",
            "sequence_number": 1,
            "item_id": "msg_1",
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": "done"
            }
        });
        assert!(sanitize_response_event_json(&mut value));
        assert_eq!(value["part"]["annotations"], json!([]));
        assert!(value.get("id").is_none());
    }
}
