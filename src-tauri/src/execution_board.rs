//! Ephemeral execution metadata from the authorized, bounded transcript reader.
//! Source bodies, tool arguments and file edits never leave this projection.
use crate::transcript::{TranscriptAvailability, TranscriptCursor, TranscriptPage};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ExecutionBlock {
    pub kind: String,
    pub call_id: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExecutionRecord {
    pub record_id: String,
    pub timestamp: Option<String>,
    pub role: Option<String>,
    pub issue: Option<String>,
    pub blocks: Vec<ExecutionBlock>,
}

#[derive(Debug, Serialize)]
pub struct ExecutionPage {
    pub availability: TranscriptAvailability,
    pub issues: Vec<String>,
    pub records: Vec<ExecutionRecord>,
    pub next_cursor: Option<TranscriptCursor>,
    pub source_complete: bool,
}

pub fn project(page: TranscriptPage) -> ExecutionPage {
    ExecutionPage {
        availability: page.availability,
        issues: page.issues,
        next_cursor: page.next_cursor,
        source_complete: page.source_complete,
        records: page
            .records
            .into_iter()
            .map(|record| {
                let (timestamp, role, blocks) = record.presentation.map_or_else(
                    || (None, None, vec![]),
                    |presentation| {
                        (
                            presentation.timestamp,
                            presentation.role,
                            presentation
                                .blocks
                                .into_iter()
                                .map(|block| ExecutionBlock {
                                    kind: block.kind.into(),
                                    call_id: block.call_id,
                                    name: block.name,
                                })
                                .collect(),
                        )
                    },
                );
                ExecutionRecord {
                    record_id: record.id,
                    timestamp,
                    role,
                    issue: record.issue,
                    blocks,
                }
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_keeps_anchors_and_gaps_but_excludes_bodies() {
        let page = TranscriptPage {
            provider: Some("codex".into()),
            availability: TranscriptAvailability::Partial,
            issues: vec!["record_too_large".into()],
            next_cursor: None,
            source_complete: false,
            records: vec![crate::transcript::TranscriptRecord {
                id: "exact-anchor".into(),
                byte_offset: 4,
                byte_length: 100,
                raw_json: Some("private raw payload".into()),
                kind: None,
                message_id: None,
                issue: Some("partial".into()),
                context_evidence: None,
                presentation: Some(crate::transcript_view::TranscriptPresentation {
                    role: Some("assistant".into()),
                    timestamp: Some("2026-01-01T00:00:00Z".into()),
                    blocks: vec![crate::transcript_view::TranscriptBlock {
                        kind: "tool_call",
                        text: "private tool input".into(),
                        call_id: Some("call1".into()),
                        name: Some("Read".into()),
                        edit: Some(crate::transcript_view::TranscriptEdit {
                            path: Some("private path".into()),
                            before: "private before".into(),
                            after: "private after".into(),
                        }),
                    }],
                }),
            }],
        };
        let value = serde_json::to_value(project(page)).unwrap();
        assert_eq!(value["records"][0]["record_id"], "exact-anchor");
        assert_eq!(value["records"][0]["blocks"][0]["call_id"], "call1");
        assert_eq!(value["availability"], "partial");
        assert!(!value.to_string().contains("private"));
        assert!(!value.to_string().contains("raw_json"));
    }
}
