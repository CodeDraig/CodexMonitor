use crate::client::{BackendScope, ConversationKey};
use serde_json::Value;
use std::collections::HashMap;

/// A disconnected composition is recoverable only through an explicit action.
/// It is never installed automatically in a later connection generation.
#[derive(Debug)]
pub(super) struct RecoveredComposition {
    pub scope: BackendScope,
    pub draft: String,
    pub attachments: Vec<Value>,
}

#[derive(Default)]
pub(super) struct CompositionRecovery {
    entries: HashMap<ConversationKey, Vec<RecoveredComposition>>,
}

impl CompositionRecovery {
    pub fn preserve(
        &mut self,
        key: ConversationKey,
        scope: BackendScope,
        draft: String,
        attachments: Vec<Value>,
    ) {
        if !draft.is_empty() || !attachments.is_empty() {
            self.entries
                .entry(key)
                .or_default()
                .push(RecoveredComposition {
                    scope,
                    draft,
                    attachments,
                });
        }
    }

    pub fn available(&self, key: &ConversationKey) -> &[RecoveredComposition] {
        self.entries.get(key).map(Vec::as_slice).unwrap_or_default()
    }

    pub fn take(&mut self, key: &ConversationKey, index: usize) -> Option<RecoveredComposition> {
        let entries = self.entries.get_mut(key)?;
        (index < entries.len()).then(|| entries.remove(index))
    }
}

pub(super) fn renderer_identity(scope: &BackendScope, key: &ConversationKey) -> String {
    serde_json::to_string(&(scope, key)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ConversationRef;
    use serde_json::json;

    #[test]
    fn same_ids_across_connection_generations_keep_separate_recoverable_compositions() {
        let key = ConversationKey {
            endpoint_id: "local".into(),
            conversation: ConversationRef::Codex {
                workspace_id: "w".into(),
                thread_id: "t".into(),
            },
        };
        let first = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 1,
            store_id: Some("a".into()),
        };
        let later = BackendScope {
            connection_epoch: 3,
            store_id: Some("b".into()),
            ..first.clone()
        };
        let mut recovery = CompositionRecovery::default();
        recovery.preserve(
            key.clone(),
            first.clone(),
            "old draft 🐉".into(),
            vec![json!({"id":"old-file"})],
        );
        recovery.preserve(
            key.clone(),
            later.clone(),
            "new draft".into(),
            vec![json!({"id":"new-file"})],
        );
        assert_eq!(recovery.available(&key).len(), 2);
        assert_ne!(
            renderer_identity(&first, &key),
            renderer_identity(&later, &key)
        );
        let recovered = recovery.take(&key, 0).unwrap();
        assert_eq!(recovered.scope, first);
        assert_eq!(recovered.draft, "old draft 🐉");
        assert_eq!(recovered.attachments, vec![json!({"id":"old-file"})]);
        assert_eq!(recovery.available(&key)[0].draft, "new draft");
    }
}
