//! Explicit local conversation search. Content scopes are shared with saved
//! queries; searched bodies and snippets must remain ephemeral desktop data.
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
}
