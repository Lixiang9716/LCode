//! Team message protocols (learn-claude-code s16): request/response
//! correlation by `request_id` with type validation and duplicate
//! suppression, plus message-type routing ([`dispatch_message`]) and the
//! protocol tools (`request_shutdown`, `request_plan`, `review_plan`,
//! `submit_plan`).
//!
//! Flow (s16):
//!   Lead:      request_shutdown → shutdown_request{request_id} → teammate
//!   Teammate:  dispatch_message → shutdown_response{request_id} → lead
//!   Lead:      match_response(request_id) → status approved / rejected
//!
//! Approval verdicts travel in the message `content` as JSON
//! (`{"approved": bool, "feedback": str}`) so `TeamMessage` stays free of
//! extra fields (see [`plan_verdict_content`] / [`parse_plan_verdict`]).

use crate::agent::team::{MessageBus, TeamMessage};
use crate::tools::{Tool, ToolResult};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Lifecycle of one protocol request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolStatus {
    Pending,
    Approved,
    Rejected,
}

impl ProtocolStatus {
    /// Lowercase form used in status text.
    pub fn as_str(self) -> &'static str {
        match self {
            ProtocolStatus::Pending => "pending",
            ProtocolStatus::Approved => "approved",
            ProtocolStatus::Rejected => "rejected",
        }
    }
}

/// In-flight state of one protocol request, keyed by `request_id` (s16).
#[derive(Debug, Clone)]
pub struct ProtocolState {
    pub request_id: String,
    /// `msg_type` of the original request (`shutdown_request` or
    /// `plan_approval_request`); responses must match it.
    pub msg_type: String,
    pub sender: String,
    pub target: String,
    pub status: ProtocolStatus,
    /// Plan text (plan approval) or shutdown reason (usually empty).
    pub payload: String,
}

/// Outcome of correlating a response message to its request (s16).
#[derive(Debug, Clone, PartialEq)]
pub enum ResponseMatch {
    /// Correlated: the request transitioned to approved/rejected.
    Matched { approved: bool },
    /// No pending request carries this `request_id`.
    Unknown,
    /// The response type does not match the request type.
    TypeMismatch { expected: String },
    /// The request was already answered; duplicates are ignored.
    Duplicate { status: ProtocolStatus },
}

/// How a teammate loop should react to one dispatched message.
#[derive(Debug, Clone, PartialEq)]
pub enum DispatchAction {
    /// No protocol meaning; the caller injects the raw message.
    None,
    /// The shutdown handshake completed; the loop must exit.
    Shutdown,
    /// A plan approval/rejection note to inject into the conversation.
    PlanNote(String),
}

/// Encode a plan approval verdict as message content (s16 review_plan).
pub fn plan_verdict_content(approve: bool, feedback: &str) -> String {
    serde_json::json!({ "approved": approve, "feedback": feedback }).to_string()
}

/// Decode a plan approval verdict from message content. Falls back to the
/// s16 plain-text convention ("Approved" prefix = approval).
pub fn parse_plan_verdict(msg: &TeamMessage) -> (bool, String) {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg.content) {
        if let (Some(approved), Some(feedback)) = (
            value.get("approved").and_then(|v| v.as_bool()),
            value.get("feedback").and_then(|v| v.as_str()),
        ) {
            return (approved, feedback.to_string());
        }
    }
    let approved = msg.content.to_lowercase().starts_with("approved");
    (approved, msg.content.clone())
}

/// Tracks in-flight protocol requests, keyed by `request_id` (mutex
/// protected, s16).
#[derive(Debug, Default)]
pub struct ProtocolManager {
    pending: Mutex<HashMap<String, ProtocolState>>,
}

impl ProtocolManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh, collision-free request id (`req_0`, `req_1`, ...).
    pub fn new_request_id(&self) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        format!("req_{}", COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Track a new in-flight request.
    pub fn register(&self, state: ProtocolState) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(state.request_id.clone(), state);
        }
    }

    /// Look up a request by id.
    pub fn get(&self, request_id: &str) -> Option<ProtocolState> {
        self.pending.lock().ok()?.get(request_id).cloned()
    }

    /// Number of tracked requests.
    pub fn len(&self) -> usize {
        self.pending.lock().map(|pending| pending.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Correlate a response to its request via `request_id` (s16): type
    /// validation + duplicate suppression. On success the request's
    /// status moves to approved/rejected.
    pub fn match_response(&self, msg: &TeamMessage) -> ResponseMatch {
        let Some(request_id) = &msg.request_id else {
            return ResponseMatch::Unknown;
        };
        let mut pending = match self.pending.lock() {
            Ok(pending) => pending,
            Err(_) => return ResponseMatch::Unknown,
        };
        let Some(state) = pending.get(request_id) else {
            return ResponseMatch::Unknown;
        };
        let expected = match state.msg_type.as_str() {
            "shutdown_request" => "shutdown_response",
            "plan_approval_request" => "plan_approval_response",
            other => return ResponseMatch::TypeMismatch { expected: other.to_string() },
        };
        if msg.msg_type != expected {
            return ResponseMatch::TypeMismatch { expected: expected.to_string() };
        }
        if state.status != ProtocolStatus::Pending {
            return ResponseMatch::Duplicate { status: state.status };
        }
        let approved = msg.msg_type == "shutdown_response" || parse_plan_verdict(msg).0;
        let status = if approved { ProtocolStatus::Approved } else { ProtocolStatus::Rejected };
        let state = pending.get_mut(request_id).expect("request exists (checked above)");
        state.status = status;
        ResponseMatch::Matched { approved }
    }

    /// Lead-side review of a pending plan (s16): sets the status and
    /// returns the state so the caller can send the response message.
    /// Errors when the request is unknown or already answered.
    pub fn review(&self, request_id: &str, approve: bool) -> anyhow::Result<ProtocolState> {
        let mut pending =
            self.pending.lock().map_err(|_| anyhow::anyhow!("protocol lock poisoned"))?;
        let Some(state) = pending.get(request_id) else {
            anyhow::bail!("Request {request_id} not found");
        };
        if state.status != ProtocolStatus::Pending {
            anyhow::bail!("Request {request_id} already {}", state.status.as_str());
        }
        let status = if approve { ProtocolStatus::Approved } else { ProtocolStatus::Rejected };
        let state = pending.get_mut(request_id).expect("request exists (checked above)");
        state.status = status;
        Ok(state.clone())
    }
}

/// Route an incoming message by `msg_type` (s16). A `shutdown_request` is
/// answered with a `shutdown_response` echoing the `request_id` and the
/// loop must exit; a `plan_approval_response` produces a note for the
/// conversation. The remaining whitelisted types (`text`, `request`,
/// `response`, `shutdown_response`, `plan_approval_request`) carry no
/// protocol action for the recipient — the caller injects them raw.
pub fn dispatch_message(name: &str, bus: &MessageBus, msg: &TeamMessage) -> DispatchAction {
    match msg.msg_type.as_str() {
        "shutdown_request" => {
            let response = TeamMessage {
                from: name.to_string(),
                to: msg.from.clone(),
                msg_type: "shutdown_response".to_string(),
                request_id: msg.request_id.clone(),
                content: "approved".to_string(),
            };
            if let Err(error) = bus.send(&response) {
                tracing::warn!(%error, "failed to send shutdown_response");
            }
            DispatchAction::Shutdown
        }
        "plan_approval_response" => {
            let (approved, feedback) = parse_plan_verdict(msg);
            if approved {
                DispatchAction::PlanNote("[Plan approved] Proceed with the task.".to_string())
            } else {
                DispatchAction::PlanNote(format!("[Plan rejected] Feedback: {feedback}"))
            }
        }
        _ => DispatchAction::None,
    }
}

// --- Protocol tools (s16) ---------------------------------------------

/// Tool: `request_shutdown` — the lead asks a teammate to shut down
/// gracefully (registers a pending protocol request, s16).
pub struct RequestShutdownTool {
    pub bus: Arc<MessageBus>,
    pub protocol: Arc<ProtocolManager>,
}

impl Tool for RequestShutdownTool {
    fn name(&self) -> &str {
        "request_shutdown"
    }
    fn description(&self) -> &str {
        "Request a teammate to shut down gracefully. The teammate answers \
         with a shutdown_response correlated by request_id."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "teammate": { "type": "string" } },
            "required": ["teammate"]
        })
    }
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let teammate = arg(args, "teammate")?;
        let request_id = self.protocol.new_request_id();
        self.protocol.register(ProtocolState {
            request_id: request_id.clone(),
            msg_type: "shutdown_request".to_string(),
            sender: "lead".to_string(),
            target: teammate.to_string(),
            status: ProtocolStatus::Pending,
            payload: String::new(),
        });
        let msg = TeamMessage {
            from: "lead".to_string(),
            to: teammate.to_string(),
            msg_type: "shutdown_request".to_string(),
            request_id: Some(request_id.clone()),
            content: "Please shut down gracefully.".to_string(),
        };
        match self.bus.send(&msg) {
            Ok(()) => Ok(ToolResult::ok(format!(
                "Shutdown request sent to {teammate} (req: {request_id})"
            ))),
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Tool: `request_plan` — the lead asks a teammate to submit a plan.
pub struct RequestPlanTool {
    pub bus: Arc<MessageBus>,
}

impl Tool for RequestPlanTool {
    fn name(&self) -> &str {
        "request_plan"
    }
    fn description(&self) -> &str {
        "Ask a teammate to submit a plan for a task for review."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "teammate": { "type": "string" },
                "task": { "type": "string" }
            },
            "required": ["teammate", "task"]
        })
    }
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let teammate = arg(args, "teammate")?;
        let task = arg(args, "task")?;
        let msg = TeamMessage {
            from: "lead".to_string(),
            to: teammate.to_string(),
            msg_type: "text".to_string(),
            request_id: None,
            content: format!("Please submit a plan for: {task}"),
        };
        match self.bus.send(&msg) {
            Ok(()) => Ok(ToolResult::ok(format!("Asked {teammate} to submit a plan"))),
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Tool: `review_plan` — approve or reject a submitted plan by
/// `request_id`; the verdict is sent back to the plan's author.
pub struct ReviewPlanTool {
    pub bus: Arc<MessageBus>,
    pub protocol: Arc<ProtocolManager>,
}

impl Tool for ReviewPlanTool {
    fn name(&self) -> &str {
        "review_plan"
    }
    fn description(&self) -> &str {
        "Approve or reject a teammate's submitted plan (by request_id)."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "request_id": { "type": "string" },
                "approve": { "type": "boolean" },
                "feedback": { "type": "string" }
            },
            "required": ["request_id", "approve"]
        })
    }
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let request_id = arg(args, "request_id")?;
        let approve = args["approve"]
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("Missing 'approve' argument"))?;
        let feedback = args["feedback"].as_str().unwrap_or("");
        let state = match self.protocol.review(request_id, approve) {
            Ok(state) => state,
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };
        let msg = TeamMessage {
            from: "lead".to_string(),
            to: state.sender.clone(),
            msg_type: "plan_approval_response".to_string(),
            request_id: Some(request_id.to_string()),
            content: plan_verdict_content(approve, feedback),
        };
        match self.bus.send(&msg) {
            Ok(()) => Ok(ToolResult::ok(format!(
                "Plan {} ({request_id})",
                if approve { "approved" } else { "rejected" }
            ))),
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Tool: `submit_plan` — a teammate submits a plan to the lead for
/// approval (registers a pending `plan_approval_request`, s16). The loop
/// stamps `from` with the teammate's own name.
pub struct SubmitPlanTool {
    pub bus: Arc<MessageBus>,
    pub protocol: Arc<ProtocolManager>,
}

impl Tool for SubmitPlanTool {
    fn name(&self) -> &str {
        "submit_plan"
    }
    fn description(&self) -> &str {
        "Submit a plan to the lead for approval. The plan is correlated \
         by request_id; keep working until the approval response arrives."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "plan": { "type": "string" },
                "from": { "type": "string" }
            },
            "required": ["plan"]
        })
    }
    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let plan = arg(args, "plan")?;
        let from = args["from"].as_str().unwrap_or("lead").to_string();
        let request_id = self.protocol.new_request_id();
        self.protocol.register(ProtocolState {
            request_id: request_id.clone(),
            msg_type: "plan_approval_request".to_string(),
            sender: from.clone(),
            target: "lead".to_string(),
            status: ProtocolStatus::Pending,
            payload: plan.to_string(),
        });
        let msg = TeamMessage {
            from,
            to: "lead".to_string(),
            msg_type: "plan_approval_request".to_string(),
            request_id: Some(request_id.clone()),
            content: plan.to_string(),
        };
        match self.bus.send(&msg) {
            Ok(()) => Ok(ToolResult::ok(format!(
                "Plan submitted ({request_id}). Waiting for approval..."
            ))),
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Extract a required string argument from tool args.
fn arg<'a>(args: &'a serde_json::Value, key: &str) -> anyhow::Result<&'a str> {
    args[key].as_str().ok_or_else(|| anyhow::anyhow!("Missing '{key}' argument"))
}

/// Register the protocol tools with the registry.
pub fn register(
    registry: &mut crate::tools::ToolRegistry,
    bus: Arc<MessageBus>,
    protocol: Arc<ProtocolManager>,
) {
    registry
        .register(Box::new(RequestShutdownTool { bus: bus.clone(), protocol: protocol.clone() }));
    registry.register(Box::new(RequestPlanTool { bus: bus.clone() }));
    registry.register(Box::new(ReviewPlanTool { bus: bus.clone(), protocol: protocol.clone() }));
    registry.register(Box::new(SubmitPlanTool { bus, protocol }));
}
