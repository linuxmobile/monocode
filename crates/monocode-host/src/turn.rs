//! Running a provider turn on behalf of a remote client.
//!
//! A command creates or resumes a session, starts the provider on a worker
//! thread, and folds each batch of normalized [`Event`]s into the stored
//! snapshot. Remote clients poll `sessions.sync`, so the loop's job is to keep
//! the snapshot and its revision moving, not to render anything.
//!
//! The block projection here is deliberately narrower than the desktop's: it
//! covers the block roles a remote viewer reads (user, assistant, reasoning,
//! tool, approval, question, system). An event it does not understand stops the
//! turn with a clear error rather than being dropped, because a silently
//! ignored event would show the client a transcript that does not match what
//! the provider actually did.

use crate::session::{HostSession, Receipt};
use monocode_core::agent::{Approval, Question, Settings};
use monocode_core::provider::{self, Event, Kind, Request};
use monocode_core::tool::ToolCall;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A live provider turn.
pub struct Turn {
    pub run_id: String,
    /// Set by `sessions.cancel`; read by the provider between reads.
    pub cancel: Arc<AtomicBool>,
}

impl Turn {
    pub fn new(run_id: String) -> Self {
        Self {
            run_id,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Everything a turn needs besides the session itself.
pub struct TurnRequest {
    pub kind: Kind,
    pub binary: std::path::PathBuf,
    pub cwd: std::path::PathBuf,
    pub prompt: String,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub settings: Settings,
}

/// Builds the provider request for a turn.
pub fn request(turn: TurnRequest) -> Request {
    Request {
        kind: turn.kind,
        binary: turn.binary,
        cwd: turn.cwd,
        prompt: turn.prompt,
        provider_id: turn.provider_id,
        model: turn.model,
        settings: turn.settings,
    }
}

/// Accumulates provider events into a session snapshot.
///
/// The caller writes the snapshot whenever `dirty()` is true and the flush
/// interval has passed.
pub struct Projection {
    session: HostSession,
    /// Events since the last write, in order. These become the persisted
    /// event rows so a client that already has revision N can replay them.
    pending: Vec<Value>,
    /// Text streamed since the last write, keyed by block id.
    streaming: HashMap<String, String>,
    dirty: bool,
    /// The provider's own conversation id, once it reports one.
    pub provider_session_id: Option<String>,
    pub status: Option<String>,
    pub error: Option<String>,
    pub done: bool,
}

impl Projection {
    pub fn new(session: HostSession) -> Self {
        Self {
            session,
            pending: Vec::new(),
            streaming: HashMap::new(),
            dirty: false,
            provider_session_id: None,
            status: None,
            error: None,
            done: false,
        }
    }

    pub fn session(&self) -> &HostSession {
        &self.session
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Applies one batch of events. Returns an error for anything this build
    /// cannot represent, which the caller turns into a failed turn.
    pub fn apply(&mut self, events: Vec<Event>) -> Result<(), String> {
        if events.is_empty() {
            return Ok(());
        }
        for event in events {
            self.apply_one(event)?;
        }
        self.dirty = true;
        Ok(())
    }

    fn apply_one(&mut self, event: Event) -> Result<(), String> {
        match event {
            Event::Ready | Event::Done => {}
            Event::SessionId(id) => {
                self.provider_session_id = Some(id.clone());
                self.pending
                    .push(json!({ "type": "session.providerBound", "providerSessionId": id }));
            }
            Event::TurnId(id) => {
                self.pending
                    .push(json!({ "type": "turn.started", "providerTurnId": id }));
            }
            Event::Model(model) => {
                if let Some(session) = self.session.session.as_object_mut() {
                    session.insert("model".into(), Value::String(model));
                }
                self.pending
                    .push(json!({ "type": "session.configChanged" }));
            }
            Event::Commands(commands) => {
                // `NativeCommand` is core-internal and not serializable; the
                // client only lists name and description.
                let listed: Vec<Value> = commands
                    .iter()
                    .map(|command| {
                        json!({ "name": command.name, "description": command.description })
                    })
                    .collect();
                self.pending
                    .push(json!({ "type": "session.commands", "commands": listed }));
            }
            Event::Context { used, window } => {
                self.pending.push(json!({
                    "type": "context",
                    "used": used,
                    "window": window,
                }));
            }
            Event::Metrics(metrics) => {
                self.pending.push(json!({ "type": "turn.metrics" }));
                self.attach(|block| {
                    if let Some(object) = block.as_object_mut() {
                        object.insert(
                            "turnMetrics".into(),
                            serde_json::to_value(metrics).unwrap_or(Value::Null),
                        );
                    }
                });
            }
            Event::Status(text) => {
                self.status = Some(text.clone());
                self.pending.push(json!({ "type": "status", "text": text }));
            }
            Event::BeginAssistant => {
                self.push_block(json!({
                    "id": new_id(),
                    "role": "assistant",
                    "text": "",
                    "streaming": true,
                }));
            }
            Event::Text(text) => {
                self.append_streaming("assistant", &text);
                self.pending
                    .push(json!({ "type": "message.delta", "text": text }));
            }
            Event::Reasoning(text) => {
                self.append_streaming("reasoning", &text);
                self.pending
                    .push(json!({ "type": "reasoning.delta", "text": text }));
            }
            Event::Tool(tool) => {
                self.upsert_tool(tool);
            }
            Event::Approval(approval) => {
                self.push_approval(approval);
            }
            Event::ApprovalResolved(id) => {
                self.resolve_approval(&id, true);
                self.pending
                    .push(json!({ "type": "approval.resolved", "id": id }));
            }
            Event::Question(question) => {
                self.push_question(question);
            }
            Event::QuestionResolved(id) => {
                self.resolve_question(&id);
                self.pending
                    .push(json!({ "type": "question.resolved", "id": id }));
            }
            Event::UsageLimited(limit) => {
                let resets = serde_json::to_value(&limit).unwrap_or(Value::Null);
                self.pending
                    .push(json!({ "type": "usage.limited", "limit": resets }));
            }
            Event::Background(tasks) => {
                let tasks: Vec<_> = tasks.into_iter().map(|t| t.description).collect();
                self.pending
                    .push(json!({ "type": "background.updated", "tasks": tasks }));
            }
            Event::Interjection { text, .. } => {
                self.pending
                    .push(json!({ "type": "interjection", "text": text }));
                self.push_block(json!({
                    "id": new_id(),
                    "role": "system",
                    "text": text,
                    "streaming": false,
                }));
            }
            Event::ConfigChanged {
                model,
                model_settings,
            } => {
                if let Some(session) = self.session.session.as_object_mut() {
                    if let Some(model) = model {
                        session.insert("model".into(), Value::String(model));
                    }
                    if !model_settings.is_empty() {
                        session.insert(
                            "modelSettings".into(),
                            serde_json::to_value(model_settings).unwrap_or(Value::Null),
                        );
                    }
                }
                self.pending
                    .push(json!({ "type": "session.configChanged" }));
            }
            Event::FollowUp(message) => {
                self.push_block(json!({
                    "id": new_id(),
                    "role": "user",
                    "text": message.text,
                    "streaming": false,
                }));
            }
            Event::FollowUpFailed { id, error } => {
                // A follow-up the provider refused is reported, not hidden:
                // the client would otherwise show a message that was never sent.
                return Err(format!("follow-up {id} failed: {error}"));
            }
        }
        Ok(())
    }

    /// Applies `change` to the block currently streaming, creating one if the
    /// provider started emitting without an explicit begin.
    fn attach(&mut self, change: impl FnOnce(&mut Value)) {
        if let Some(block) = self.last_streaming_block_mut() {
            change(block);
        }
    }

    fn last_streaming_block_mut(&mut self) -> Option<&mut Value> {
        let blocks = self.session.session.get_mut("blocks")?.as_array_mut()?;
        blocks
            .iter_mut()
            .rev()
            .find(|block| block.get("streaming").and_then(Value::as_bool) == Some(true))
    }

    /// Appends streamed text to the open block of `role`, opening one if the
    /// previous block is finished or of a different role.
    fn append_streaming(&mut self, role: &str, text: &str) {
        let open = self
            .session
            .session
            .get("blocks")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks.last().is_some_and(|block| {
                    block.get("role").and_then(Value::as_str) == Some(role)
                        && block.get("streaming").and_then(Value::as_bool) == Some(true)
                })
            });
        if !open {
            self.push_block(json!({
                "id": new_id(),
                "role": role,
                "text": "",
                "streaming": true,
            }));
        }
        let id = self
            .session
            .session
            .get("blocks")
            .and_then(Value::as_array)
            .and_then(|blocks| blocks.last())
            .and_then(|block| block.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let Some(id) = id else { return };
        self.streaming.entry(id.clone()).or_default().push_str(text);
        let merged = self.streaming.get(&id).cloned().unwrap_or_default();
        self.replace_block_text(&id, &merged);
    }

    fn replace_block_text(&mut self, id: &str, text: &str) {
        if let Some(block) = self.block_mut(id) {
            if let Some(object) = block.as_object_mut() {
                object.insert("text".into(), Value::String(text.to_string()));
            }
        }
    }

    fn block_mut(&mut self, id: &str) -> Option<&mut Value> {
        self.session
            .session
            .get_mut("blocks")?
            .as_array_mut()?
            .iter_mut()
            .find(|block| block.get("id").and_then(Value::as_str) == Some(id))
    }

    fn blocks_mut(&mut self) -> &mut Vec<Value> {
        let session = self
            .session
            .session
            .as_object_mut()
            .expect("session snapshot is an object");
        session
            .entry("blocks")
            .or_insert_with(|| Value::Array(Vec::new()));
        session
            .get_mut("blocks")
            .and_then(Value::as_array_mut)
            .expect("blocks is an array")
    }

    fn push_block(&mut self, block: Value) {
        self.blocks_mut().push(block);
    }

    fn upsert_tool(&mut self, tool: ToolCall) {
        let value = serde_json::to_value(&tool).unwrap_or(Value::Null);
        let id = tool.id.clone();
        self.pending
            .push(json!({ "type": "tool.updated", "tool": value }));
        if let Some(block) = self.block_mut(&id) {
            if let Some(object) = block.as_object_mut() {
                object.insert("tool".into(), value);
            }
            return;
        }
        self.push_block(json!({
            "id": id,
            "role": "tool",
            "text": "",
            "streaming": false,
            "tool": value,
        }));
    }

    fn push_approval(&mut self, approval: Approval) {
        let value = json!({
            "id": approval.id,
            "title": approval.title,
            "detail": approval.detail,
            "canRemember": approval.can_remember,
        });
        self.pending
            .push(json!({ "type": "approval.requested", "approval": value }));
        self.push_block(json!({
            "id": approval.id,
            "role": "approval",
            "text": approval.title,
            "streaming": false,
            "approval": value,
            "decision": Value::Null,
        }));
    }

    fn resolve_approval(&mut self, id: &str, _allow: bool) {
        if let Some(block) = self.block_mut(id) {
            if let Some(object) = block.as_object_mut() {
                object.insert("decision".into(), Value::String("answered".into()));
            }
        }
    }

    fn push_question(&mut self, question: Question) {
        let id = question.id.clone();
        let value = json!({
            "id": question.id,
            "title": question.title,
            "autoResolveAt": question.auto_resolve_at,
            "items": question.items.iter().map(|item| json!({
                "id": item.id,
                "prompt": item.prompt,
                "multiple": item.multiple,
                "allowCustom": item.allow_custom,
                "options": item.options.iter().map(|option| json!({
                    "id": option.id,
                    "label": option.label,
                    "description": option.description,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        });
        self.pending
            .push(json!({ "type": "question.asked", "question": value }));
        self.push_block(json!({
            "id": id,
            "role": "approval",
            "text": "",
            "streaming": false,
            "question": value,
            "decision": Value::Null,
        }));
    }

    fn resolve_question(&mut self, id: &str) {
        if let Some(block) = self.block_mut(id) {
            if let Some(object) = block.as_object_mut() {
                object.insert("decision".into(), Value::String("answered".into()));
            }
        }
    }

    /// Closes the provider's answer: stops streaming and, on failure, records
    /// why so the client is not left with a transcript that looks complete.
    pub fn settle(&mut self, status: &str, message: Option<String>) {
        if let Some(blocks) = self
            .session
            .session
            .get_mut("blocks")
            .and_then(Value::as_array_mut)
        {
            for block in blocks.iter_mut() {
                if let Some(object) = block.as_object_mut() {
                    if object.get("streaming").and_then(Value::as_bool) == Some(true) {
                        object.insert("streaming".into(), Value::Bool(false));
                    }
                }
            }
        }
        if let Some(message) = message {
            self.push_block(json!({
                "id": new_id(),
                "role": "system",
                "text": message,
                "streaming": false,
            }));
        }
        self.session.status = status.to_string();
        self.session.updated_at = now_ms();
        self.dirty = true;
    }

    /// Takes the events accumulated since the last write.
    pub fn take_events(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.pending)
    }

    /// Advances the revision before a write.
    pub fn bump(&mut self) {
        self.session.revision += 1;
        self.session.updated_at = now_ms();
    }

    pub fn into_session(self) -> HostSession {
        self.session
    }
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// A receipt acknowledging an applied command.
pub fn receipt(command_id: &str, signature: &str) -> Receipt {
    Receipt {
        command_id: command_id.to_string(),
        signature: signature.to_string(),
        receipt: json!({ "applied": true }),
    }
}

/// Runs a turn to completion, folding events into `projection`.
///
/// `persist` is called whenever the accumulation should reach the database; it
/// receives the projected session and the events to record alongside it.
pub fn run_turn(
    turn: &Turn,
    request: Request,
    mut projection: Projection,
    mut persist: impl FnMut(&mut Projection) -> Result<(), String>,
) -> Result<Projection, String> {
    let result = provider::run(request, turn.cancel.clone(), |events| {
        if turn.cancel.load(Ordering::Relaxed) {
            return Err("Stopped".into());
        }
        projection.apply(events)?;
        if projection.dirty() {
            projection.bump();
            persist(&mut projection)?;
        }
        Ok(())
    });
    match result {
        Ok(()) => {
            projection.settle("idle", None);
            projection.bump();
            persist(&mut projection)?;
            Ok(projection)
        }
        Err(error) => {
            // Cancellation is a normal end, not a failure; the transcript keeps
            // whatever the provider had produced.
            let stopped = turn.cancel.load(Ordering::Relaxed);
            if stopped {
                projection.settle("interrupted", None);
            } else {
                projection.settle("idle", Some(error.clone()));
            }
            projection.error = (!stopped).then(|| error.clone());
            projection.bump();
            persist(&mut projection)?;
            if stopped {
                Ok(projection)
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::HostSession;

    fn session() -> HostSession {
        HostSession {
            project_id: "p".into(),
            revision: 0,
            run_id: Some("r".into()),
            status: "running".into(),
            created_at: Some(1),
            updated_at: 1,
            archived: None,
            pinned: None,
            auto_worktree_branch: None,
            block_revisions: None,
            session: json!({
                "id": "s1",
                "cwd": "/tmp",
                "title": "T",
                "harness": "claude",
                "blocks": [],
            }),
        }
    }

    fn blocks(projection: &Projection) -> Vec<Value> {
        projection.session().session["blocks"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn streamed_text_accumulates_into_one_block() {
        let mut projection = Projection::new(session());
        projection.apply(vec![Event::BeginAssistant]).unwrap();
        projection.apply(vec![Event::Text("Hel".into())]).unwrap();
        projection.apply(vec![Event::Text("lo".into())]).unwrap();
        let blocks = blocks(&projection);
        assert_eq!(blocks.len(), 1, "each delta opened a new block: {blocks:?}");
        assert_eq!(blocks[0]["text"], "Hello");
        assert_eq!(blocks[0]["role"], "assistant");
    }

    #[test]
    fn text_without_a_begin_still_opens_a_block() {
        let mut projection = Projection::new(session());
        projection
            .apply(vec![Event::Text("orphan".into())])
            .unwrap();
        assert_eq!(blocks(&projection)[0]["text"], "orphan");
    }

    #[test]
    fn reasoning_and_assistant_text_stay_in_separate_blocks() {
        let mut projection = Projection::new(session());
        projection
            .apply(vec![Event::Text("answer".into())])
            .unwrap();
        projection
            .apply(vec![Event::Reasoning("thought".into())])
            .unwrap();
        let blocks = blocks(&projection);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["role"], "assistant");
        assert_eq!(blocks[1]["role"], "reasoning");
        assert_eq!(blocks[1]["text"], "thought");
    }

    #[test]
    fn settling_stops_every_streaming_block() {
        let mut projection = Projection::new(session());
        projection
            .apply(vec![Event::Text("partial".into())])
            .unwrap();
        projection.settle("idle", None);
        assert_eq!(blocks(&projection)[0]["streaming"], false);
        assert_eq!(projection.session().status, "idle");
    }

    #[test]
    fn a_failed_turn_records_why_in_the_transcript() {
        let mut projection = Projection::new(session());
        projection.settle("idle", Some("provider crashed".into()));
        let blocks = blocks(&projection);
        assert_eq!(blocks.last().unwrap()["role"], "system");
        assert_eq!(blocks.last().unwrap()["text"], "provider crashed");
    }

    #[test]
    fn a_session_id_is_reported_as_a_provider_binding() {
        let mut projection = Projection::new(session());
        projection
            .apply(vec![Event::SessionId("prov-1".into())])
            .unwrap();
        assert_eq!(projection.provider_session_id.as_deref(), Some("prov-1"));
        let events = projection.take_events();
        assert_eq!(events[0]["type"], "session.providerBound");
        assert_eq!(events[0]["providerSessionId"], "prov-1");
    }

    #[test]
    fn a_refused_follow_up_fails_the_turn_instead_of_vanishing() {
        let mut projection = Projection::new(session());
        let error = projection
            .apply(vec![Event::FollowUpFailed {
                id: "f1".into(),
                error: "no such session".into(),
            }])
            .unwrap_err();
        assert!(error.contains("no such session"), "got: {error}");
    }

    #[test]
    fn a_tool_call_reuses_its_block_across_updates() {
        let mut projection = Projection::new(session());
        let call = ToolCall::new("t1", "read");
        projection.apply(vec![Event::Tool(call.clone())]).unwrap();
        projection.apply(vec![Event::Tool(call)]).unwrap();
        let blocks = blocks(&projection);
        assert_eq!(blocks.len(), 1, "a tool update created a second block");
        assert_eq!(blocks[0]["role"], "tool");
    }

    #[test]
    fn an_approval_blocks_needs_input_until_it_is_answered() {
        let mut projection = Projection::new(session());
        projection
            .apply(vec![Event::Approval(Approval {
                id: "a1".into(),
                title: "Allow?".into(),
                detail: "detail".into(),
                can_remember: false,
            })])
            .unwrap();
        assert!(crate::session::summary(projection.session())["needsInput"]
            .as_bool()
            .unwrap());
        projection
            .apply(vec![Event::ApprovalResolved("a1".into())])
            .unwrap();
        assert!(!crate::session::summary(projection.session())["needsInput"]
            .as_bool()
            .unwrap());
    }

    #[test]
    fn events_are_drained_once() {
        let mut projection = Projection::new(session());
        projection
            .apply(vec![Event::SessionId("p".into())])
            .unwrap();
        assert_eq!(projection.take_events().len(), 1);
        assert!(projection.take_events().is_empty());
    }
}
