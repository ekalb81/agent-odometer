//! Explicit local conversation search. Content scopes are shared with saved
//! queries; searched bodies and snippets must remain ephemeral desktop data.
use crate::{
    store::AppState,
    transcript::{self, TranscriptCursor, TranscriptRequest},
};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

/// Tool bodies require a separate explicit choice for calls and results. Old
/// saved queries that omit scope retain conversation-only behavior.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ContentScope {
    pub conversation: bool,
    pub tool_calls: bool,
    pub tool_results: bool,
}

impl Default for ContentScope {
    fn default() -> Self {
        Self {
            conversation: true,
            tool_calls: false,
            tool_results: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetainedMessageField {
    UserMessage,
    LastAgentMessage,
}

/// Source records and retained message fields have different identities and
/// coverage. A missing source must never turn a retained target into a made-up
/// transcript anchor, or redirect a stale target to a different message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SearchTarget {
    SourceRecord {
        session_id: String,
        record_id: String,
        block_index: usize,
    },
    RetainedTurn {
        session_id: String,
        session_identity: String,
        snapshot_revision: String,
        turn_id: String,
        field: RetainedMessageField,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum SearchPosition {
    Source {
        cursor: TranscriptCursor,
        incomplete: bool,
    },
    Retained {
        session_identity: String,
        snapshot_revision: String,
        next_turn: usize,
        #[serde(default)]
        incomplete: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchCursor {
    pub session_id: String,
    pub query: String,
    pub scope: ContentScope,
    pub position: SearchPosition,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub session_id: String,
    pub query: String,
    #[serde(default)]
    pub scope: ContentScope,
    pub cursor: Option<SearchCursor>,
}

#[derive(Debug, Serialize)]
pub struct SearchSnippet {
    pub text: String,
    /// UTF-16 indices align with JavaScript string slicing, including emoji.
    pub match_start: usize,
    pub match_end: usize,
    pub truncated_before: bool,
    pub truncated_after: bool,
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub target: SearchTarget,
    pub content_kind: String,
    pub snippet: SearchSnippet,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchPhase {
    Source,
    Retained,
}

#[derive(Debug, Serialize)]
pub struct SearchPage {
    pub phase: SearchPhase,
    pub hits: Vec<SearchHit>,
    pub next_cursor: Option<SearchCursor>,
    pub issues: Vec<String>,
    /// Complete source coverage for the selected content classes. False for
    /// missing, omitted, or unrecognized content, even when no match was found.
    pub source_complete: bool,
    /// Only completeness of the retained prompt/final-reply fields, not a claim
    /// that the retained snapshot contains the whole original conversation.
    pub retained_complete: bool,
    pub scanned_records: usize,
    pub scanned_messages: usize,
}

fn matcher(query: &str) -> Result<Regex, String> {
    if query.trim().is_empty() || query.chars().count() > 256 || query.len() > 1024 {
        return Err("invalid_search_query".into());
    }
    RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .build()
        .map_err(|_| "invalid_search_query".into())
}

fn snippet(text: &str, matcher: &Regex) -> Option<SearchSnippet> {
    let found = matcher.find(text)?;
    let boundaries: Vec<usize> = text
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .collect();
    let start_char = boundaries.binary_search(&found.start()).ok()?;
    let end_char = boundaries.binary_search(&found.end()).ok()?;
    // A 256-character query may require a larger snippet than ordinary context.
    let width = 240.max(end_char - start_char);
    let begin = start_char.saturating_sub(60.min(width - (end_char - start_char)));
    let end = (begin + width).min(boundaries.len() - 1);
    let begin_byte = boundaries[begin];
    let end_byte = boundaries[end];
    Some(SearchSnippet {
        text: text[begin_byte..end_byte].into(),
        match_start: text[begin_byte..found.start()].encode_utf16().count(),
        match_end: text[begin_byte..found.end()].encode_utf16().count(),
        truncated_before: begin_byte > 0,
        truncated_after: end_byte < text.len(),
    })
}

fn includes(scope: ContentScope, kind: &str) -> bool {
    match kind {
        "text" => scope.conversation,
        "tool_call" => scope.tool_calls,
        "tool_result" | "tool_error" => scope.tool_results,
        _ => false,
    }
}

fn search_records(
    session_id: &str,
    scope: ContentScope,
    matcher: &Regex,
    records: &[transcript::TranscriptRecord],
) -> (Vec<SearchHit>, bool) {
    let mut hits = Vec::new();
    let mut unrecognized = false;
    for record in records {
        let Some(presentation) = &record.presentation else {
            unrecognized |= record.issue.is_some();
            continue;
        };
        unrecognized |= presentation.blocks.is_empty()
            && matches!(
                record.kind.as_deref(),
                Some("response_item" | "user" | "assistant")
            );
        unrecognized |= presentation
            .blocks
            .iter()
            .any(|block| block.kind == "source_block");
        if let Some((index, block, snippet)) = presentation
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| {
                includes(scope, block.kind)
                    && (block.kind != "text"
                        || matches!(presentation.role.as_deref(), Some("user" | "assistant")))
            })
            .find_map(|(index, block)| {
                snippet(&block.text, matcher).map(|snippet| (index, block, snippet))
            })
        {
            // One hit per source record. The UI calls this matching records,
            // rather than suggesting this counts every occurrence.
            hits.push(SearchHit {
                target: SearchTarget::SourceRecord {
                    session_id: session_id.into(),
                    record_id: record.id.clone(),
                    block_index: index,
                },
                content_kind: block.kind.into(),
                snippet,
            });
        }
    }
    (hits, unrecognized)
}

fn retained_position(
    state: &AppState,
    session_id: &str,
    issues: &mut Vec<String>,
) -> Option<SearchPosition> {
    let Some(history) = state.history_ready() else {
        issues.push("retained_history_unavailable".into());
        return None;
    };
    match history.retained_search_info(session_id) {
        Ok(info) if info.bytes <= 8 * 1024 * 1024 && info.format_supported => {
            Some(SearchPosition::Retained {
                session_identity: info.identity,
                snapshot_revision: info.revision,
                next_turn: 0,
                incomplete: false,
            })
        }
        Ok(info) => {
            issues.push(
                if info.bytes > 8 * 1024 * 1024 {
                    "retained_snapshot_too_large"
                } else {
                    "retained_snapshot_unsupported"
                }
                .into(),
            );
            None
        }
        Err(_) => {
            issues.push("retained_messages_unavailable".into());
            None
        }
    }
}

pub(crate) fn retained_page(
    session_id: &str,
    earlier_incomplete: bool,
    messages: crate::history_store::RetainedSearchMessages,
    matcher: &Regex,
) -> (SearchPage, Option<SearchPosition>) {
    let scanned_messages = messages.messages.len();
    let incomplete =
        earlier_incomplete || messages.messages.iter().any(|message| message.truncated);
    let hits = messages
        .messages
        .into_iter()
        .filter_map(|message| {
            let mut snippet = snippet(&message.text, matcher)?;
            snippet.truncated_after |= message.truncated;
            Some(SearchHit {
                target: SearchTarget::RetainedTurn {
                    session_id: session_id.into(),
                    session_identity: messages.info.identity.clone(),
                    snapshot_revision: messages.info.revision.clone(),
                    turn_id: message.turn_id,
                    field: message.field,
                },
                content_kind: match message.field {
                    RetainedMessageField::UserMessage => "user_message",
                    RetainedMessageField::LastAgentMessage => "last_agent_message",
                }
                .into(),
                snippet,
            })
        })
        .collect();
    let position = messages
        .next_turn
        .map(|next_turn| SearchPosition::Retained {
            session_identity: messages.info.identity,
            snapshot_revision: messages.info.revision,
            next_turn,
            incomplete,
        });
    let mut issues = vec![
        "retained_prompt_and_final_reply_only".into(),
        "retained_matches_may_overlap_source".into(),
    ];
    if incomplete {
        issues.push("retained_fields_truncated".into());
    }
    (
        SearchPage {
            phase: SearchPhase::Retained,
            hits,
            next_cursor: None,
            issues,
            source_complete: false,
            retained_complete: position.is_none() && !incomplete,
            scanned_records: 0,
            scanned_messages,
        },
        position,
    )
}

/// One call examines one bounded source page or 25 retained turns. All bodies
/// and snippets are ephemeral; no index, summary, diagnostic, or pricing writes.
pub fn search_for_session(state: &AppState, request: SearchRequest) -> Result<SearchPage, String> {
    if request.session_id.is_empty() || request.session_id.len() > 256 {
        return Err("invalid_session_id".into());
    }
    if !request.scope.conversation && !request.scope.tool_calls && !request.scope.tool_results {
        return Err("empty_content_scope".into());
    }
    let matcher = matcher(&request.query)?;
    if request.cursor.as_ref().is_some_and(|cursor| {
        cursor.session_id != request.session_id
            || cursor.query != request.query
            || cursor.scope != request.scope
    }) {
        return Err("search_cursor_invalid".into());
    }
    let cursor = request.cursor.map(|cursor| cursor.position);
    let position;
    let mut result = if let Some(SearchPosition::Retained {
        session_identity,
        snapshot_revision,
        next_turn,
        incomplete,
    }) = cursor
    {
        if !request.scope.conversation
            || session_identity.len() > 128
            || snapshot_revision.len() > 256
        {
            return Err("search_cursor_invalid".into());
        }
        let history = state
            .history_ready()
            .ok_or("retained_history_unavailable")?;
        let messages = history
            .retained_search_messages(
                &request.session_id,
                Some(&session_identity),
                Some(&snapshot_revision),
                next_turn,
            )
            .map_err(|_| "retained_target_unavailable".to_string())?;
        let (page, next) = retained_page(&request.session_id, incomplete, messages, &matcher);
        position = next;
        page
    } else {
        let (cursor, earlier_incomplete) = match cursor {
            Some(SearchPosition::Source { cursor, incomplete }) => (Some(cursor), incomplete),
            _ => (None, false),
        };
        let page = transcript::read_for_session(
            state,
            TranscriptRequest {
                session_id: request.session_id.clone(),
                cursor,
                max_records: Some(25),
                max_bytes: Some(128 * 1024),
                record_id: None,
            },
        );
        if page.availability == transcript::TranscriptAvailability::CursorInvalid {
            return Err("search_cursor_invalid".into());
        }
        let (hits, unrecognized) =
            search_records(&request.session_id, request.scope, &matcher, &page.records);
        let mut issues = page.issues;
        if unrecognized {
            issues.push("unrecognized_content_not_searched".into());
        }
        let incomplete = earlier_incomplete || unrecognized;
        let source_complete = page.source_complete && !incomplete;
        position = if let Some(cursor) = page.next_cursor {
            Some(SearchPosition::Source { cursor, incomplete })
        } else if !source_complete && request.scope.conversation {
            retained_position(state, &request.session_id, &mut issues)
        } else {
            None
        };
        SearchPage {
            phase: SearchPhase::Source,
            hits,
            next_cursor: None,
            issues,
            source_complete,
            retained_complete: false,
            scanned_records: page.records.len(),
            scanned_messages: 0,
        }
    };
    result.next_cursor = position.map(|position| SearchCursor {
        session_id: request.session_id,
        query: request.query,
        scope: request.scope,
        position,
    });
    Ok(result)
}

#[derive(Debug, Serialize)]
pub struct RetainedSearchLanding {
    pub target: SearchTarget,
    pub text: String,
    pub truncated: bool,
}

pub fn resolve_retained_target(
    state: &AppState,
    target: SearchTarget,
) -> Result<RetainedSearchLanding, String> {
    let SearchTarget::RetainedTurn {
        session_id,
        session_identity,
        snapshot_revision,
        turn_id,
        field,
    } = &target
    else {
        return Err("not_a_retained_target".into());
    };
    if session_id.len() > 256
        || session_identity.len() > 128
        || snapshot_revision.len() > 256
        || turn_id.len() > 1024
    {
        return Err("invalid_search_target".into());
    }
    let history = state
        .history_ready()
        .ok_or("retained_history_unavailable")?;
    let message = history
        .retained_search_target(
            session_id,
            session_identity,
            snapshot_revision,
            turn_id,
            *field,
        )
        .map_err(|_| "retained_target_unavailable".to_string())?;
    Ok(RetainedSearchLanding {
        target,
        text: message.text,
        truncated: message.truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_scope_fields_cannot_opt_in_to_tool_bodies() {
        let scope: ContentScope = serde_json::from_str("{}").unwrap();
        assert_eq!(scope, ContentScope::default());
        let scope: ContentScope =
            serde_json::from_str(r#"{"conversation":false,"tool_calls":true}"#).unwrap();
        assert!(!scope.conversation);
        assert!(scope.tool_calls);
        assert!(!scope.tool_results);
        assert!(serde_json::from_str::<ContentScope>(r#"{"tools":true}"#).is_err());
    }

    #[test]
    fn literal_unicode_search_produces_javascript_safe_highlight_offsets() {
        let literal = matcher("[cafÃ©]").unwrap();
        let result = snippet("ðŸ¦€ [CAFÃ‰] literal", &literal).unwrap();
        assert_eq!((result.match_start, result.match_end), (3, 9));
        assert!(snippet("CAFE", &literal).is_none());
        assert!(matcher(" ").is_err());
        assert!(matcher(&"x".repeat(257)).is_err());
        let result = snippet(
            &format!("{}needle{}", "x".repeat(300), "z".repeat(300)),
            &matcher("needle").unwrap(),
        )
        .unwrap();
        assert_eq!(result.text.chars().count(), 240);
        assert!(result.truncated_before && result.truncated_after);
    }

    #[test]
    fn mixed_provider_records_do_not_opt_in_to_tool_bodies_via_role() {
        let values = [
            serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call","content":"private needle"}]}}),
            serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"public needle"},{"type":"tool_use","id":"call","name":"Read","input":{"path":"private needle"}}]}}),
            serde_json::json!({"type":"response_item","payload":{"type":"function_call","name":"Read","arguments":"private needle"}}),
        ];
        let records: Vec<_> = values
            .iter()
            .enumerate()
            .map(|(index, value)| transcript::TranscriptRecord {
                id: format!("record-{index}"),
                byte_offset: 0,
                byte_length: 0,
                raw_json: None,
                kind: None,
                message_id: None,
                issue: None,
                presentation: Some(crate::transcript_view::present(value)),
            })
            .collect();
        let matcher = matcher("needle").unwrap();
        let (hits, _) = search_records("synthetic", ContentScope::default(), &matcher, &records);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content_kind, "text");
        let scope = ContentScope {
            conversation: false,
            tool_calls: true,
            tool_results: true,
        };
        let (hits, _) = search_records("synthetic", scope, &matcher, &records);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].content_kind, "tool_result");
        assert_eq!(hits[1].content_kind, "tool_call");
    }

    #[test]
    fn default_search_excludes_reasoning_summaries_even_with_assistant_role() {
        let values = [
            serde_json::json!({"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"text","text":"private needle"}]}}),
            serde_json::json!({"type":"response_item","payload":{"type":"reasoning","role":"assistant","summary":[{"type":"text","text":"private needle"}]}}),
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"text","text":"public needle"}]}}),
        ];
        let records: Vec<_> = values
            .iter()
            .enumerate()
            .map(|(index, value)| transcript::TranscriptRecord {
                id: format!("record-{index}"),
                byte_offset: 0,
                byte_length: 0,
                raw_json: None,
                kind: Some("response_item".into()),
                message_id: None,
                issue: None,
                presentation: Some(crate::transcript_view::present(value)),
            })
            .collect();
        let (hits, _) = search_records(
            "synthetic",
            ContentScope::default(),
            &matcher("needle").unwrap(),
            &records,
        );
        assert_eq!(hits.len(), 1);
        assert!(
            matches!(&hits[0].target,SearchTarget::SourceRecord {record_id,..} if record_id=="record-2")
        );
    }
}
