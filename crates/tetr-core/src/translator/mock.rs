use super::{TranslateError, TranslationMeta, TranslationRequest, Translator};
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct MockTranslator;

impl Translator for MockTranslator {
    fn provider_name(&self) -> &'static str {
        "mock"
    }

    fn stream_translate(
        &self,
        request: &TranslationRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<TranslationMeta, TranslateError> {
        let started = Instant::now();
        let synthetic = format!("【模拟翻译】{}", request.input.trim());
        let mut output_chars = 0usize;

        for chunk in synthetic.as_bytes().chunks(12) {
            let piece = String::from_utf8_lossy(chunk);
            output_chars += piece.chars().count();
            on_delta(&piece);
        }

        Ok(TranslationMeta {
            provider: self.provider_name().to_string(),
            model: "mock-stream".to_string(),
            input_chars: request.input.chars().count(),
            output_chars,
            latency_ms: started.elapsed().as_millis(),
            truncated: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_streaming_deltas() {
        let translator = MockTranslator;
        let mut merged = String::new();

        let mut cb = |delta: &str| merged.push_str(delta);
        let meta = translator
            .stream_translate(&TranslationRequest::translate("hello"), &mut cb)
            .expect("mock stream should always succeed");

        assert!(merged.contains("模拟翻译"));
        assert_eq!(meta.provider, "mock");
        assert!(meta.output_chars > 0);
    }
}
