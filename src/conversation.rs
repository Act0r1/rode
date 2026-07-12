use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static CARD_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ConversationAttachment {
    /// Compatibility with conversation events written before attachments were typed.
    Legacy(String),
    Context {
        label: String,
    },
    Image {
        path: PathBuf,
    },
}

impl ConversationAttachment {
    pub fn label(&self) -> String {
        match self {
            Self::Legacy(label) => label.clone(),
            Self::Context { label } => label.clone(),
            Self::Image { path } => path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Image")
                .to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CardStatus {
    Pending,
    Running,
    Success,
    Failed,
    Cancelled,
    #[default]
    Complete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeTone {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CardKind {
    UserMessage {
        text: String,
        model: String,
        access: String,
        attachments: Vec<ConversationAttachment>,
    },
    AssistantMessage {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Command {
        item_id: String,
        command: String,
        cwd: String,
        output: String,
        exit_code: Option<i64>,
    },
    FileChange {
        item_id: String,
        summary: String,
    },
    ToolResult {
        title: String,
        output: String,
    },
    Approval {
        item_id: String,
        approval_kind: String,
        title: String,
        detail: String,
    },
    Notice {
        tone: NoticeTone,
        text: String,
    },
    TurnBoundary {
        label: String,
        detail: Option<String>,
    },
    CancelledTurn {
        detail: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConversationCard {
    pub id: String,
    pub turn_id: Option<String>,
    pub created_at_ms: i64,
    pub status: CardStatus,
    pub collapsed: bool,
    pub kind: CardKind,
}

impl ConversationCard {
    pub fn new(kind: CardKind, status: CardStatus, turn_id: Option<String>) -> Self {
        Self {
            id: new_card_id(),
            turn_id,
            created_at_ms: now_ms(),
            status,
            collapsed: false,
            kind,
        }
    }

    pub fn stable(
        id: impl Into<String>,
        kind: CardKind,
        status: CardStatus,
        turn_id: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            turn_id,
            created_at_ms: now_ms(),
            status,
            collapsed: false,
            kind,
        }
    }

    pub fn is_collapsible(&self) -> bool {
        matches!(self.kind, CardKind::Reasoning { .. })
            || matches!(&self.kind, CardKind::Command { output, .. } if output.lines().count() > 8)
            || matches!(&self.kind, CardKind::ToolResult { output, .. } if output.lines().count() > 8)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConversationProjection {
    cards: Rc<Vec<ConversationCard>>,
    index_by_id: HashMap<String, usize>,
}

impl ConversationProjection {
    pub fn cards(&self) -> &[ConversationCard] {
        &self.cards
    }

    pub fn shared_cards(&self) -> Rc<Vec<ConversationCard>> {
        self.cards.clone()
    }

    pub fn cards_mut(&mut self) -> &mut [ConversationCard] {
        Rc::make_mut(&mut self.cards).as_mut_slice()
    }

    pub fn get_mut(&mut self, id: &str) -> Option<(usize, &mut ConversationCard)> {
        let index = self.index_by_id.get(id).copied()?;
        Some((index, &mut Rc::make_mut(&mut self.cards)[index]))
    }

    pub fn replace(&mut self, cards: Vec<ConversationCard>) {
        self.cards = Rc::new(cards);
        self.rebuild_index();
    }

    pub fn hide_internal_lifecycle_cards(&mut self) {
        Rc::make_mut(&mut self.cards).retain(|card| {
            !matches!(
                card.kind,
                CardKind::Notice {
                    tone: NoticeTone::Info,
                    ..
                }
            ) && !matches!(
                card.kind,
                CardKind::TurnBoundary { .. } if card.status != CardStatus::Failed
            )
        });
        self.rebuild_index();
    }

    pub fn clear(&mut self) {
        Rc::make_mut(&mut self.cards).clear();
        self.index_by_id.clear();
    }

    pub fn push(&mut self, card: ConversationCard) -> usize {
        let id = card.id.clone();
        let cards = Rc::make_mut(&mut self.cards);
        cards.push(card);
        let index = cards.len() - 1;
        self.index_by_id.insert(id, index);
        index
    }

    pub fn upsert(&mut self, card: ConversationCard) -> usize {
        if let Some(index) = self.index_by_id.get(&card.id).copied() {
            let existing = &mut Rc::make_mut(&mut self.cards)[index];
            let created_at_ms = existing.created_at_ms;
            *existing = card;
            existing.created_at_ms = created_at_ms;
            index
        } else {
            self.push(card)
        }
    }

    pub fn append_assistant_delta(&mut self, item_id: &str, turn_id: &str, delta: &str) -> usize {
        let id = format!("assistant-{item_id}");
        if let Some(index) = self.index_by_id.get(&id).copied() {
            let card = &mut Rc::make_mut(&mut self.cards)[index];
            if let CardKind::AssistantMessage { text } = &mut card.kind {
                text.push_str(delta);
                card.status = CardStatus::Running;
            }
            return index;
        }
        self.push(ConversationCard::stable(
            id,
            CardKind::AssistantMessage {
                text: delta.to_owned(),
            },
            CardStatus::Running,
            Some(turn_id.to_owned()),
        ))
    }

    pub fn append_reasoning_delta(
        &mut self,
        item_id: &str,
        content_index: i64,
        turn_id: &str,
        delta: &str,
    ) -> usize {
        let id = format!("reasoning-{item_id}-{content_index}");
        if let Some(index) = self.index_by_id.get(&id).copied() {
            let card = &mut Rc::make_mut(&mut self.cards)[index];
            if let CardKind::Reasoning { text } = &mut card.kind {
                text.push_str(delta);
                card.status = CardStatus::Running;
            }
            return index;
        }
        self.push(ConversationCard::stable(
            id,
            CardKind::Reasoning {
                text: delta.to_owned(),
            },
            CardStatus::Running,
            Some(turn_id.to_owned()),
        ))
    }

    pub fn toggle_collapsed(&mut self, id: &str) -> Option<usize> {
        let index = self.index_by_id.get(id).copied()?;
        let card = &mut Rc::make_mut(&mut self.cards)[index];
        if !card.is_collapsible() {
            return None;
        }
        card.collapsed = !card.collapsed;
        Some(index)
    }

    pub fn reconcile_after_restart(&mut self) -> bool {
        let mut changed = false;
        for card in Rc::make_mut(&mut self.cards) {
            if matches!(card.status, CardStatus::Pending | CardStatus::Running) {
                card.status = match card.kind {
                    CardKind::Approval { .. } => CardStatus::Failed,
                    _ => CardStatus::Cancelled,
                };
                changed = true;
            }
        }
        if changed {
            self.push(ConversationCard::new(
                CardKind::Notice {
                    tone: NoticeTone::Warning,
                    text: "Rode restarted while provider activity was in progress. Live approvals are no longer actionable and unfinished cards are marked interrupted."
                        .to_owned(),
                },
                CardStatus::Complete,
                None,
            ));
        }
        changed
    }

    fn rebuild_index(&mut self) {
        self.index_by_id = self
            .cards
            .iter()
            .enumerate()
            .map(|(index, card)| (card.id.clone(), index))
            .collect();
    }
}

fn new_card_id() -> String {
    format!(
        "card-{}-{}",
        now_ms(),
        CARD_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        CardKind, CardStatus, ConversationAttachment, ConversationCard, ConversationProjection,
        NoticeTone,
    };

    #[test]
    fn streaming_updates_keep_stable_card_identity() {
        let mut projection = ConversationProjection::default();
        let first = projection.append_assistant_delta("item-1", "turn-1", "hello");
        let second = projection.append_assistant_delta("item-1", "turn-1", " world");
        assert_eq!(first, second);
        assert_eq!(projection.cards().len(), 1);
        assert!(matches!(
            &projection.cards()[0].kind,
            CardKind::AssistantMessage { text } if text == "hello world"
        ));
    }

    #[test]
    fn typed_cards_round_trip_without_losing_presentation_state() {
        let mut card = ConversationCard::stable(
            "reasoning-turn-1",
            CardKind::Reasoning {
                text: "Inspecting the repository".to_owned(),
            },
            CardStatus::Complete,
            Some("turn-1".to_owned()),
        );
        card.collapsed = true;
        let json = serde_json::to_string(&card).expect("serialize card");
        let restored: ConversationCard = serde_json::from_str(&json).expect("restore card");
        assert_eq!(restored, card);
    }

    #[test]
    fn legacy_string_attachments_remain_readable() {
        let json = r#"{"id":"user-1","turn_id":null,"created_at_ms":1,"status":"complete","collapsed":false,"kind":{"type":"user_message","text":"Look","model":"gpt-5.4","access":"workspace_write","attachments":["design.png"]}}"#;
        let card: ConversationCard = serde_json::from_str(json).expect("read legacy card");
        assert!(matches!(
            card.kind,
            CardKind::UserMessage { attachments, .. }
                if attachments == vec![ConversationAttachment::Legacy("design.png".to_owned())]
        ));
    }

    #[test]
    fn every_typed_card_variant_round_trips() {
        let variants = vec![
            CardKind::UserMessage {
                text: "Prompt".to_owned(),
                model: "gpt-5.4".to_owned(),
                access: "read_only".to_owned(),
                attachments: vec![ConversationAttachment::Context {
                    label: "Current Git diff".to_owned(),
                }],
            },
            CardKind::AssistantMessage {
                text: "Answer".to_owned(),
            },
            CardKind::Reasoning {
                text: "Summary".to_owned(),
            },
            CardKind::Command {
                item_id: "cmd-1".to_owned(),
                command: "cargo test".to_owned(),
                cwd: "/workspace".to_owned(),
                output: "ok".to_owned(),
                exit_code: Some(0),
            },
            CardKind::FileChange {
                item_id: "file-1".to_owned(),
                summary: "src/main.rs".to_owned(),
            },
            CardKind::ToolResult {
                title: "Tool".to_owned(),
                output: "Result".to_owned(),
            },
            CardKind::Approval {
                item_id: "approval-1".to_owned(),
                approval_kind: "command".to_owned(),
                title: "Run tests".to_owned(),
                detail: "Needs approval".to_owned(),
            },
            CardKind::Notice {
                tone: NoticeTone::Error,
                text: "Failure".to_owned(),
            },
            CardKind::TurnBoundary {
                label: "Complete".to_owned(),
                detail: None,
            },
            CardKind::CancelledTurn {
                detail: "Interrupted".to_owned(),
            },
        ];
        for (index, kind) in variants.into_iter().enumerate() {
            let card = ConversationCard::stable(
                format!("card-{index}"),
                kind,
                CardStatus::Complete,
                Some("turn-1".to_owned()),
            );
            let payload = serde_json::to_string(&card).expect("serialize card variant");
            assert_eq!(
                serde_json::from_str::<ConversationCard>(&payload).expect("restore card variant"),
                card
            );
        }
    }

    #[test]
    fn large_projection_updates_by_stable_id_without_growing() {
        let mut projection = ConversationProjection::default();
        for index in 0..10_000 {
            projection.push(ConversationCard::stable(
                format!("event-{index}"),
                CardKind::Notice {
                    tone: NoticeTone::Info,
                    text: index.to_string(),
                },
                CardStatus::Complete,
                None,
            ));
        }
        let updated_index = projection.upsert(ConversationCard::stable(
            "event-9999",
            CardKind::Notice {
                tone: NoticeTone::Warning,
                text: "updated".to_owned(),
            },
            CardStatus::Success,
            None,
        ));
        assert_eq!(updated_index, 9_999);
        assert_eq!(projection.cards().len(), 10_000);
    }

    #[test]
    fn restart_reconciles_unfinished_and_live_approval_cards() {
        let mut projection = ConversationProjection::default();
        projection.push(ConversationCard::stable(
            "assistant-1",
            CardKind::AssistantMessage {
                text: "partial".to_owned(),
            },
            CardStatus::Running,
            Some("turn-1".to_owned()),
        ));
        projection.push(ConversationCard::stable(
            "approval-1",
            CardKind::Approval {
                item_id: "approval-1".to_owned(),
                approval_kind: "command".to_owned(),
                title: "Run".to_owned(),
                detail: String::new(),
            },
            CardStatus::Pending,
            Some("turn-1".to_owned()),
        ));
        assert!(projection.reconcile_after_restart());
        assert_eq!(projection.cards()[0].status, CardStatus::Cancelled);
        assert_eq!(projection.cards()[1].status, CardStatus::Failed);
    }

    #[test]
    fn hides_internal_lifecycle_cards_but_keeps_failures() {
        let mut projection = ConversationProjection::default();
        projection.replace(vec![
            ConversationCard::stable(
                "session-ready",
                CardKind::Notice {
                    tone: NoticeTone::Info,
                    text: "Codex session ready".to_owned(),
                },
                CardStatus::Complete,
                None,
            ),
            ConversationCard::stable(
                "turn-complete",
                CardKind::TurnBoundary {
                    label: "Turn complete".to_owned(),
                    detail: None,
                },
                CardStatus::Success,
                Some("turn-1".to_owned()),
            ),
            ConversationCard::stable(
                "turn-failed",
                CardKind::TurnBoundary {
                    label: "Turn failed".to_owned(),
                    detail: Some("provider error".to_owned()),
                },
                CardStatus::Failed,
                Some("turn-2".to_owned()),
            ),
            ConversationCard::stable(
                "assistant",
                CardKind::AssistantMessage {
                    text: "Answer".to_owned(),
                },
                CardStatus::Success,
                Some("turn-1".to_owned()),
            ),
        ]);

        projection.hide_internal_lifecycle_cards();

        assert_eq!(
            projection
                .cards()
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            vec!["turn-failed", "assistant"]
        );
    }
}
