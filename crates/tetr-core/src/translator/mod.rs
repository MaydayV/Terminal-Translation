pub mod deepseek;
pub mod mock;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TranslationMeta {
    pub provider: String,
    pub model: String,
    pub input_chars: usize,
    pub output_chars: usize,
    pub latency_ms: u128,
    pub truncated: bool,
}

#[derive(Debug, Error)]
pub enum TranslateError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("request failed: {0}")]
    Request(String),

    #[error("provider returned HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },

    #[error("failed to parse stream: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranslationContentType {
    Translate,
    Explain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationRequest {
    pub input: String,
    pub content_type: TranslationContentType,
    pub command: Option<String>,
}

impl TranslationRequest {
    pub fn translate(input: impl Into<String>) -> Self {
        Self {
            input: input.into(),
            content_type: TranslationContentType::Translate,
            command: None,
        }
    }

    pub fn explain(command: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            input: output.into(),
            content_type: TranslationContentType::Explain,
            command: Some(command.into()),
        }
    }
}

pub trait Translator: Send + Sync {
    fn provider_name(&self) -> &'static str;

    fn stream_translate(
        &self,
        request: &TranslationRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<TranslationMeta, TranslateError>;
}
