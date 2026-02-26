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

pub trait Translator: Send + Sync {
    fn provider_name(&self) -> &'static str;

    fn stream_translate(
        &self,
        input: &str,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<TranslationMeta, TranslateError>;
}
