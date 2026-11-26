//! Whitelisted public output carried from a native transport to the single persistence writer.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicKind {
    Message,
    ToolStart,
    ToolEnd,
    Activity,
    Gap,
    Diagnostic,
}
impl PublicKind {
    pub fn event_kind(self) -> &'static str {
        match self {
            Self::Message => "public.message",
            Self::ToolStart => "public.tool_started",
            Self::ToolEnd => "public.tool_completed",
            Self::Activity => "public.activity",
            Self::Gap => "public.gap",
            Self::Diagnostic => "public.diagnostic",
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PublicUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub context_limit: Option<u64>,
}
/// No arbitrary provider object crosses this boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicPart {
    pub native_stream: String,
    pub native_item_id: String,
    pub part: u64,
    pub received_offset: u64,
    pub block_key: String,
    pub stream: String,
    pub first_offset: u64,
    pub kind: PublicKind,
    pub speaker: Option<String>,
    pub name: Option<String>,
    pub text: String,
    pub activity: Option<String>,
    pub model: Option<String>,
    pub usage: Option<PublicUsage>,
    pub redacted: bool,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}
