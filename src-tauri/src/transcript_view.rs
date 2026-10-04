//! Bounded presentation of explicitly opened source records. Never used by accounting.
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct TranscriptPresentation {
    pub role: Option<String>,
    pub timestamp: Option<String>,
    pub blocks: Vec<TranscriptBlock>,
}
#[derive(Debug, Serialize)]
pub struct TranscriptBlock {
    pub kind: &'static str,
    pub text: String,
    pub call_id: Option<String>,
    pub name: Option<String>,
    pub edit: Option<TranscriptEdit>,
}
#[derive(Debug, Serialize)]
pub struct TranscriptEdit {
    pub path: Option<String>,
    pub before: String,
    pub after: String,
}
fn string(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}
fn body(value: &Value) -> String {
    value.as_str().map(str::to_owned).unwrap_or_else(|| {
        if value.is_null() {
            String::new()
        } else {
            serde_json::to_string_pretty(value).unwrap_or_default()
        }
    })
}
fn block(kind: &'static str, text: String) -> TranscriptBlock {
    TranscriptBlock {
        kind,
        text,
        call_id: None,
        name: None,
        edit: None,
    }
}
fn tool(
    kind: &'static str,
    value: &Value,
    name: Option<String>,
    id: Option<String>,
    payload: &Value,
) -> TranscriptBlock {
    let mut result = block(kind, body(payload));
    result.name = name;
    result.call_id = id;
    // This is a provider-recorded replacement, not a reconstructed file diff.
    if kind == "tool_call" {
        if let (Some(before), Some(after)) = (
            payload.get("old_string").and_then(Value::as_str),
            payload.get("new_string").and_then(Value::as_str),
        ) {
            result.edit = Some(TranscriptEdit {
                path: string(&payload["file_path"]),
                before: before.into(),
                after: after.into(),
            });
        }
        // Codex arguments may themselves be a JSON string. Parsing belongs here.
        if result.edit.is_none()
            && result
                .name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case("edit"))
        {
            if let Some(input) = payload
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
            {
                if let (Some(before), Some(after)) =
                    (input["old_string"].as_str(), input["new_string"].as_str())
                {
                    result.edit = Some(TranscriptEdit {
                        path: string(&input["file_path"]),
                        before: before.into(),
                        after: after.into(),
                    });
                }
            }
        }
    }
    if value["is_error"] == true {
        result.kind = "tool_error";
    }
    result
}
fn content(value: &Value, blocks: &mut Vec<TranscriptBlock>) {
    if let Some(text) = value.as_str() {
        blocks.push(block("text", text.into()));
        return;
    }
    let Some(parts) = value.as_array() else {
        return;
    };
    for part in parts {
        match part["type"].as_str().unwrap_or("") {
            "text" | "input_text" | "output_text" => {
                blocks.push(block("text", body(&part["text"])))
            }
            "thinking" | "reasoning" => blocks.push(block("reasoning", body(&part["thinking"]))),
            "tool_use" => blocks.push(tool(
                "tool_call",
                part,
                string(&part["name"]),
                string(&part["id"]),
                &part["input"],
            )),
            "tool_result" => blocks.push(tool(
                "tool_result",
                part,
                None,
                string(&part["tool_use_id"]),
                &part["content"],
            )),
            _ => blocks.push(block("source_block", body(part))),
        }
    }
}
pub fn present(value: &Value) -> TranscriptPresentation {
    let mut result = TranscriptPresentation {
        role: None,
        timestamp: string(&value["timestamp"]),
        blocks: vec![],
    };
    let record_type = value["type"].as_str().unwrap_or("");
    if record_type == "response_item" {
        let item = &value["payload"];
        result.role = string(&item["role"]);
        match item["type"].as_str().unwrap_or("") {
            "message" => content(&item["content"], &mut result.blocks),
            "function_call" => result.blocks.push(tool(
                "tool_call",
                item,
                string(&item["name"]),
                string(&item["call_id"]),
                &item["arguments"],
            )),
            "custom_tool_call" => result.blocks.push(tool(
                "tool_call",
                item,
                string(&item["name"]),
                string(&item["call_id"]),
                &item["input"],
            )),
            "function_call_output" | "custom_tool_call_output" => result.blocks.push(tool(
                "tool_result",
                item,
                None,
                string(&item["call_id"]),
                &item["output"],
            )),
            "reasoning" => content(&item["summary"], &mut result.blocks),
            _ => (),
        }
    } else if record_type == "user" || record_type == "assistant" {
        result.role = Some(record_type.into());
        content(&value["message"]["content"], &mut result.blocks);
    } else if record_type == "event_msg" {
        let payload = &value["payload"];
        match payload["type"].as_str().unwrap_or("") {
            "user_message" => {
                result.role = Some("user".into());
                result.blocks.push(block("text", body(&payload["message"])));
            }
            "agent_message" => {
                result.role = Some("assistant".into());
                result.blocks.push(block("text", body(&payload["message"])));
            }
            _ => (),
        }
    }
    // Unknown records remain fully inspectable via raw_json; never infer a role.
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn interleaved_claude_blocks_preserve_order_identity_and_recorded_edit() {
        let view = present(
            &json!({"type":"assistant","timestamp":"2026-01-01T00:00:00Z","message":{"content":[{"type":"text","text":"Before"},{"type":"tool_use","id":"call-1","name":"Edit","input":{"file_path":"synthetic.rs","old_string":"old","new_string":"new"}},{"type":"text","text":"After"}]}}),
        );
        assert_eq!(view.role.as_deref(), Some("assistant"));
        assert_eq!(
            view.blocks.iter().map(|b| b.kind).collect::<Vec<_>>(),
            ["text", "tool_call", "text"]
        );
        assert_eq!(view.blocks[1].call_id.as_deref(), Some("call-1"));
        assert_eq!(view.blocks[1].edit.as_ref().unwrap().before, "old");
        let view = present(
            &json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call-1","content":"failed","is_error":true}]}}),
        );
        assert_eq!(view.blocks[0].kind, "tool_error");
        assert_eq!(view.blocks[0].call_id.as_deref(), Some("call-1"));
    }
    #[test]
    fn codex_calls_outputs_and_unknown_records_remain_source_backed() {
        let call = present(
            &json!({"type":"response_item","payload":{"type":"function_call","name":"Read","call_id":"read-1","arguments":"{\"file\":\"synthetic.rs\"}"}}),
        );
        let output = present(
            &json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"read-1","output":"first\nsecond"}}),
        );
        assert_eq!(call.blocks[0].call_id, output.blocks[0].call_id);
        assert_eq!(output.blocks[0].text, "first\nsecond");
        let unknown = present(&json!({"type":"unknown","payload":{"text":"must not be invented"}}));
        assert!(unknown.blocks.is_empty());
        assert!(unknown.role.is_none());
    }
}
