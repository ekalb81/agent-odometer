//! Source evidence only: never token attribution or an accounting input.
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct ContextEvidence {
    pub contributors: Vec<&'static str>,
    pub compaction: bool,
    pub pre_compaction_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub unsupported: bool,
}

pub fn project(value: &Value) -> ContextEvidence {
    let presentation = crate::transcript_view::present(value);
    let mut contributors = Vec::new();
    for block in &presentation.blocks {
        let category = match block.kind {
            "tool_call" => Some(if block.name.as_deref() == Some("Skill") {
                "skill invocation"
            } else {
                "tool input"
            }),
            "tool_result" | "tool_error" => Some("tool output"),
            "text" => match presentation.role.as_deref() {
                Some("user") => Some("user message"),
                Some("system" | "developer") => Some("instructions"),
                Some("assistant") => Some("assistant message"),
                _ => None,
            },
            _ => None,
        };
        if let Some(category) = category {
            if !contributors.contains(&category) {
                contributors.push(category);
            }
        }
    }
    let kind = value["type"].as_str().unwrap_or("");
    let compaction =
        kind == "compacted" || (kind == "system" && value["subtype"] == "compact_boundary");
    // These are recorded per-call usage fields, never a reconstructed context size.
    let info = if kind == "event_msg" && value["payload"]["type"] == "token_count" {
        &value["payload"]["info"]
    } else {
        &Value::Null
    };
    let unsupported = contributors.is_empty() && !compaction && info.is_null();
    ContextEvidence {
        contributors,
        compaction,
        pre_compaction_tokens: if kind == "system" && compaction {
            value["compactMetadata"]["preTokens"].as_u64()
        } else {
            None
        },
        input_tokens: info["last_token_usage"]["input_tokens"].as_u64(),
        output_tokens: info["last_token_usage"]["output_tokens"].as_u64(),
        context_window: info["model_context_window"].as_u64(),
        unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn evidence_does_not_reconstruct_tokens_or_missing_boundaries() {
        let user = project(
            &json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Read demo.rs"}]}}),
        );
        assert_eq!(user.contributors, ["user message"]);
        assert_eq!(user.input_tokens, None);
        assert!(!user.compaction);
        assert!(project(&json!({"type":"compacted"})).compaction);
        assert!(project(&json!({"type":"system","subtype":"compact_boundary"})).compaction);
        let boundary = project(
            &json!({"type":"system","subtype":"compact_boundary","compactMetadata":{"preTokens":12000}}),
        );
        assert_eq!(boundary.pre_compaction_tokens, Some(12000));
        let skill = project(
            &json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Skill","input":{"skill":"synthetic"}}]}}),
        );
        assert_eq!(skill.contributors, ["skill invocation"]);
        assert!(project(&json!({"type":"unknown","tokens":999})).unsupported);
        let source = json!({"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":40,"output_tokens":3},"total_token_usage":{"input_tokens":900},"model_context_window":2000}}});
        let before = source.clone();
        let evidence = project(&source);
        assert_eq!(evidence.input_tokens, Some(40));
        assert_eq!(evidence.output_tokens, Some(3));
        assert_eq!(evidence.context_window, Some(2000));
        assert_eq!(source, before);
    }
}
