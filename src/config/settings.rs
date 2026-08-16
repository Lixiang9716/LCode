//! Configuration data structures and their default values.

use crate::config::tuning::{
    BackgroundConfig, CompactionConfig, EventsConfig, MemoryConfig, RetryConfig, SubagentConfig,
    TeamConfig, TodoConfig,
};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

/// Main configuration structure.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// LLM provider settings
    #[serde(default)]
    pub llm: LlmConfig,

    /// Agent behavior settings
    #[serde(default)]
    pub agent: AgentConfig,

    /// Tool-specific settings
    #[serde(default)]
    pub tools: ToolsConfig,

    /// Context compaction tuning
    #[serde(default)]
    pub compaction: CompactionConfig,

    /// Teammate loop tuning
    #[serde(default)]
    pub team: TeamConfig,

    /// Subagent delegation tuning
    #[serde(default)]
    pub subagent: SubagentConfig,

    /// Cross-session memory tuning
    #[serde(default)]
    pub memory: MemoryConfig,

    /// Background command tuning
    #[serde(default)]
    pub background: BackgroundConfig,

    /// Retry/backoff tuning
    #[serde(default)]
    pub retry: RetryConfig,

    /// Event bus / command channel capacities
    #[serde(default)]
    pub events: EventsConfig,

    /// Todo list limits
    #[serde(default)]
    pub todo: TodoConfig,
}

/// LLM provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Provider name: "openai", "anthropic", "openai_compatible"
    #[serde(default = "default_provider")]
    pub provider: String,

    /// API key for the provider. `SecretString` redacts the value in
    /// `Debug` output and requires an explicit `expose_secret` to read
    /// it; serialization to config files stays plaintext (secrecy
    /// deliberately blocks `Serialize` for secrets, so a custom
    /// serializer is wired here).
    #[serde(default, serialize_with = "serialize_secret")]
    pub api_key: SecretString,

    /// Model name to use
    #[serde(default = "default_model")]
    pub model: String,

    /// API base URL (for OpenAI-compatible providers)
    #[serde(default)]
    pub api_base: Option<String>,

    /// Maximum tokens in a single response
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,

    /// Temperature for generation
    #[serde(default = "default_temperature")]
    pub temperature: f32,

    /// Model to fail over to when the primary model keeps failing (used
    /// by the retry layer after `max_attempts` consecutive failures)
    #[serde(default)]
    pub fallback_model: Option<String>,

    /// Disable the provider's thinking mode (DeepSeek v4 defaults to
    /// enabled): skips the hidden reasoning tokens, lowering prompt
    /// tokens (~79 fewer) and making responses faster and more direct.
    #[serde(default)]
    pub thinking_disabled: bool,

    /// Reasoning effort for DeepSeek v4's thinking mode (`low` / `high` /
    /// `max`; `high` is the default tier). `low` saves ~65% of the
    /// hidden reasoning tokens and ~50% of the latency with no measured
    /// accuracy loss. Ignored when `thinking_disabled` is set. On the
    /// OpenAI-format endpoint this maps to the top-level
    /// `reasoning_effort` parameter; on DeepSeek's Anthropic-compatible
    /// endpoint it maps to a `reasoning: {type: "enabled", effort}` block
    /// (not sent to the native Anthropic endpoint, which has no such
    /// field).
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,

    /// Force thinking mode off for internal utility calls (context
    /// compaction summaries, memory extraction/consolidation) even when
    /// `thinking_disabled` is false. Those calls summarize or classify
    /// existing text — hidden reasoning there costs ~10x the tokens with
    /// no benefit (measured 48 vs 531 tokens on the same task).
    #[serde(default = "default_internal_thinking_disabled")]
    pub internal_thinking_disabled: bool,

    /// Hard cost cap for one session, USD. `None` (default) disables
    /// the budget gate; when set, the session aborts as soon as the
    /// estimated spend reaches the cap.
    #[serde(default)]
    pub budget_total_usd: Option<f64>,

    /// Spend ratio (0..1) that triggers a one-shot budget warning
    /// reminder in the conversation.
    #[serde(default = "default_budget_warning_ratio")]
    pub budget_warning_ratio: f64,
}

/// Reasoning effort tiers for DeepSeek v4's thinking mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    High,
    Max,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            api_key: SecretString::from(String::new()),
            model: default_model(),
            api_base: None,
            max_tokens: default_max_tokens(),
            temperature: default_temperature(),
            fallback_model: None,
            thinking_disabled: false,
            reasoning_effort: None,
            internal_thinking_disabled: default_internal_thinking_disabled(),
            budget_total_usd: None,
            budget_warning_ratio: default_budget_warning_ratio(),
        }
    }
}

/// Agent behavior configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// System prompt or path to system prompt template
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,

    /// Maximum conversation turns in a session
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    /// Require user approval before executing tools
    #[serde(default = "default_require_approval")]
    pub require_approval: bool,

    /// Maximum context window size (in tokens)
    #[serde(default = "default_context_size")]
    pub context_size: usize,

    /// Directory containing skills (`SKILL.md` files). Defaults to
    /// `<workspace>/skills` when unset (G9 / s07).
    #[serde(default)]
    pub skills_dir: Option<std::path::PathBuf>,

    /// Turns without a todo update before the nag reminder fires (s03)
    #[serde(default = "default_todo_nag_after_turns")]
    pub todo_nag_after_turns: u32,

    /// Inject a fix-and-rerun reminder after a failing test command
    /// (cargo test/nextest, npm test, pytest, go test, ...). One-way
    /// merge (false wins).
    #[serde(default = "default_test_until_green")]
    pub test_until_green: bool,

    /// Run a self-review pass before finishing: the internal
    /// (thinking-disabled) provider reviews the session and issues
    /// restart the loop up to `self_review_max_rounds` times.
    /// Off by default.
    #[serde(default)]
    pub self_review: bool,

    /// Restart rounds allowed for self-review fixes.
    #[serde(default = "default_self_review_max_rounds")]
    pub self_review_max_rounds: u32,

    /// Inject a workspace-context block (git branch, status, diff stat)
    /// at turn start, so the model sees repository state without
    /// spending turns on `git status` itself. Off by default (opt-in).
    #[serde(default)]
    pub workspace_aware: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: default_system_prompt(),
            max_turns: default_max_turns(),
            require_approval: default_require_approval(),
            context_size: default_context_size(),
            skills_dir: None,
            todo_nag_after_turns: default_todo_nag_after_turns(),
            test_until_green: default_test_until_green(),
            self_review: false,
            self_review_max_rounds: default_self_review_max_rounds(),
            workspace_aware: false,
        }
    }
}

/// Tool permission settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsConfig {
    /// Allowed directories for file operations (empty = project root only)
    #[serde(default)]
    pub allowed_dirs: Vec<String>,

    /// Shell commands that are always allowed
    #[serde(default)]
    pub allowed_commands: Vec<String>,

    /// Shell commands that are always denied
    #[serde(default)]
    pub denied_commands: Vec<String>,

    /// Enable web fetch/search tools
    #[serde(default = "default_true")]
    pub enable_web: bool,

    /// Hard cap for a single URL fetch (read_file/write_file URL mode);
    /// the download aborts and cleans up when exceeded.
    #[serde(default = "default_max_fetch_bytes")]
    pub max_fetch_bytes: usize,

    /// Total timeout for one URL fetch, seconds.
    #[serde(default = "default_fetch_timeout_secs")]
    pub fetch_timeout_secs: u64,

    /// Host allowlist for URL fetches (empty = allow all hosts). Exact
    /// host or `*.suffix` wildcard.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,

    /// Host denylist for URL fetches, checked before `allowed_hosts`.
    /// Defaults block loopback, link-local and cloud metadata endpoints
    /// (SSRF). Exact host or `*.suffix` wildcard.
    #[serde(default = "default_denied_hosts")]
    pub denied_hosts: Vec<String>,

    /// URL fetches ignore `auto_approve` and always go through the
    /// approval channel when true. One-way merge (project layer can only
    /// relax, not tighten — same semantics as
    /// `llm.internal_thinking_disabled`).
    #[serde(default = "default_network_requires_approval")]
    pub network_requires_approval: bool,

    /// File paths read_file refuses to read (secret material). Wildcard
    /// patterns (`*`/`?`), matched against the file name and the full
    /// relative path.
    #[serde(default = "default_sensitive_paths")]
    pub sensitive_paths: Vec<String>,

    /// Redact detected secrets in read_file output (gitleaks-compatible
    /// rules) before it reaches the LLM context. Best effort — the
    /// sensitive-path block and the approval gate are the hard lines.
    /// One-way merge (false wins).
    #[serde(default = "default_scrub_secrets")]
    pub scrub_secrets: bool,

    /// Shell isolation mode: "none" (default, unchanged), "auto"
    /// (landlock → bwrap → docker, first available), or an explicit
    /// backend. Unavailable backends run unsandboxed with a warning.
    #[serde(default = "default_sandbox")]
    pub sandbox: String,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            allowed_dirs: Vec::new(),
            allowed_commands: Vec::new(),
            denied_commands: vec![
                "rm -rf /".into(),
                "sudo".into(),
                "chmod 777".into(),
                "mkfs".into(),
            ],
            enable_web: true,
            max_fetch_bytes: default_max_fetch_bytes(),
            fetch_timeout_secs: default_fetch_timeout_secs(),
            allowed_hosts: Vec::new(),
            denied_hosts: default_denied_hosts(),
            network_requires_approval: default_network_requires_approval(),
            sensitive_paths: default_sensitive_paths(),
            scrub_secrets: default_scrub_secrets(),
            sandbox: default_sandbox(),
        }
    }
}

/// Serialize a secret to plaintext (config files are the intended,
/// user-owned storage; in-memory `Debug` stays redacted).
fn serialize_secret<S: serde::Serializer>(
    value: &SecretString,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(secrecy::ExposeSecret::expose_secret(value))
}

// Default value functions. `pub(super)` so `config` module can use them in
// merge/override logic; the ones re-exported for the test suite are
// `#[doc(hidden)] pub`. serde `default =` paths resolve within this module.
pub(super) fn default_provider() -> String {
    "anthropic".into()
}
#[doc(hidden)]
pub fn default_model() -> String {
    "claude-sonnet-4-20250514".into()
}
#[doc(hidden)]
pub fn default_max_tokens() -> u32 {
    8192
}
#[doc(hidden)]
pub fn default_temperature() -> f32 {
    0.3
}
pub(super) fn default_system_prompt() -> String {
    "You are LCode, an expert software engineer and coding agent. \
     You help users write, review, debug, and understand code. \
     Be concise, accurate, and helpful."
        .into()
}
#[doc(hidden)]
pub fn default_max_turns() -> u32 {
    100
}
#[doc(hidden)]
pub fn default_require_approval() -> bool {
    true
}
#[doc(hidden)]
pub fn default_context_size() -> usize {
    128000
}
#[doc(hidden)]
pub fn default_todo_nag_after_turns() -> u32 {
    3
}
#[doc(hidden)]
pub fn default_test_until_green() -> bool {
    true
}
#[doc(hidden)]
pub fn default_self_review_max_rounds() -> u32 {
    1
}
pub(super) fn default_true() -> bool {
    true
}
#[doc(hidden)]
pub fn default_internal_thinking_disabled() -> bool {
    true
}
#[doc(hidden)]
pub fn default_max_fetch_bytes() -> usize {
    52_428_800
}
#[doc(hidden)]
pub fn default_fetch_timeout_secs() -> u64 {
    60
}
#[doc(hidden)]
pub fn default_denied_hosts() -> Vec<String> {
    vec![
        "127.0.0.1".into(),
        "localhost".into(),
        "::1".into(),
        "169.254.169.254".into(),
        "metadata.google.internal".into(),
        "*.internal".into(),
        "*.local".into(),
    ]
}
#[doc(hidden)]
pub fn default_network_requires_approval() -> bool {
    true
}
#[doc(hidden)]
pub fn default_budget_warning_ratio() -> f64 {
    0.8
}
#[doc(hidden)]
pub fn default_sensitive_paths() -> Vec<String> {
    vec![
        ".env".into(),
        ".env.*".into(),
        ".lcode.toml".into(),
        "*.pem".into(),
        "id_rsa*".into(),
        ".ssh/*".into(),
    ]
}
#[doc(hidden)]
pub fn default_scrub_secrets() -> bool {
    true
}
#[doc(hidden)]
pub fn default_sandbox() -> String {
    "none".into()
}

impl ReasoningEffort {
    /// Wire-format value for request bodies ("low", "high", "max").
    pub fn as_str(&self) -> &'static str {
        match self {
            ReasoningEffort::Low => "low",
            ReasoningEffort::High => "high",
            ReasoningEffort::Max => "max",
        }
    }
}

impl std::str::FromStr for ReasoningEffort {
    type Err = String;

    /// Case-insensitive parse shared by the env override and the
    /// `lcode config set` paths. TOML files deserialize strictly via
    /// serde and accept the lowercase spellings only.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_lowercase().as_str() {
            "low" => Ok(ReasoningEffort::Low),
            "high" => Ok(ReasoningEffort::High),
            "max" => Ok(ReasoningEffort::Max),
            other => Err(format!("invalid reasoning_effort: {other} (low/high/max)")),
        }
    }
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
