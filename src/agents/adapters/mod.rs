//! Per-agent `AgentAdapter` implementations registered by `AdapterRegistry`.

mod aider;
mod amp;
mod antigravity;
mod auggie;
mod claude;
mod cline;
mod codex;
mod copilot;
mod cursor;
mod gemini;
mod goose;
mod grok;
mod kiro;
mod opencode;
mod openhands;
mod pi;

pub(super) use aider::AiderAdapter;
pub(super) use amp::AmpAdapter;
pub(super) use antigravity::AntigravityAdapter;
pub(super) use auggie::AuggieAdapter;
pub(super) use claude::ClaudeCodeAdapter;
pub(super) use cline::ClineCliAdapter;
pub(super) use codex::CodexAdapter;
pub(super) use copilot::GitHubCopilotCliAdapter;
pub(super) use cursor::CursorCliAdapter;
pub(super) use gemini::GeminiAdapter;
pub(super) use goose::GooseCliAdapter;
pub(super) use grok::GrokAdapter;
pub(super) use kiro::KiroCliAdapter;
pub(super) use opencode::OpenCodeAdapter;
pub(super) use openhands::OpenHandsCliAdapter;
pub(super) use pi::PiAdapter;
