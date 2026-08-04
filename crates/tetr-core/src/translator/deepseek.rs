use super::{
    TranslateError, TranslationContentType, TranslationMeta, TranslationRequest, Translator,
};
use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::json;
use std::env;
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-chat";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";
const TERMINAL_TRANSLATION_SYSTEM_PROMPT: &str = r#"You are a professional terminal-output translator. Translate English command-line output into concise, natural Chinese in real time.
Requirements:
1. Output translation only. No explanations, summaries, annotations, prefixes, or suffixes.
2. Translate only English natural-language content. Keep existing Chinese unchanged.
3. Keep commands, flags, paths, filenames, URLs, IPs, ports, environment variables, code, error codes, and log identifiers exactly unchanged.
4. For mixed lines, translate only natural-language fragments and keep technical fragments untouched.
5. Prefer natural, idiomatic Chinese. Avoid word-for-word literal translation and awkward syntax; you may reorder wording while preserving meaning.
6. For short slogans, idioms, or colloquial lines, prioritize fluent sense translation.
7. Preserve line structure strictly: keep the same line order and newline layout as input; translate line by line and do not merge or split lines.
8. Keep terminology concise, accurate, and consistent. If uncertain, keep the original term.
9. Never hallucinate or add information not present in the input."#;

const TERMINAL_EXPLAIN_SYSTEM_PROMPT: &str = r#"你是一个终端教学助手，面向完全不懂编程的小白用户。用户会提供他们输入的命令和终端输出。请：
1）先用一句话解释这个命令是做什么的（如“ls 命令用于列出当前文件夹的内容”）。
2）再用通俗易懂的中文解释输出内容的含义。
保留命令名、路径、文件名、参数等技术内容不翻译。简洁明了，不要啰嗦。"#;

#[derive(Debug, Clone)]
pub struct DeepSeekTranslator {
    provider_name: &'static str,
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl DeepSeekTranslator {
    pub fn from_env() -> Result<Self, TranslateError> {
        Self::from_deepseek_env()
    }

    pub fn from_deepseek_env() -> Result<Self, TranslateError> {
        let api_key =
            read_env_chain(&["TETR_DEEPSEEK_API_KEY", "TETR_API_KEY"]).ok_or_else(|| {
                TranslateError::Config(
                    "missing API key, expected TETR_DEEPSEEK_API_KEY or TETR_API_KEY".to_string(),
                )
            })?;

        let base_url = read_env_chain(&["TETR_DEEPSEEK_BASE_URL", "TETR_API_BASE_URL"])
            .unwrap_or_else(|| DEFAULT_DEEPSEEK_BASE_URL.to_string());

        let model = read_env_chain(&["TETR_DEEPSEEK_MODEL", "TETR_MODEL"])
            .unwrap_or_else(|| DEFAULT_DEEPSEEK_MODEL.to_string());

        Self::new(
            "deepseek",
            base_url,
            api_key,
            model,
            Duration::from_secs(30),
        )
    }

    pub fn from_openai_compatible_env() -> Result<Self, TranslateError> {
        let api_key = read_env_chain(&["TETR_OPENAI_API_KEY", "TETR_API_KEY", "OPENAI_API_KEY"])
            .ok_or_else(|| {
                TranslateError::Config(
                    "missing API key, expected TETR_OPENAI_API_KEY / TETR_API_KEY / OPENAI_API_KEY"
                        .to_string(),
                )
            })?;

        let base_url = read_env_chain(&["TETR_OPENAI_BASE_URL", "TETR_API_BASE_URL"])
            .unwrap_or_else(|| DEFAULT_OPENAI_BASE_URL.to_string());

        let model = read_env_chain(&["TETR_OPENAI_MODEL", "TETR_MODEL"])
            .unwrap_or_else(|| DEFAULT_OPENAI_MODEL.to_string());

        Self::new(
            "openai-compatible",
            base_url,
            api_key,
            model,
            Duration::from_secs(30),
        )
    }

    pub fn new(
        provider_name: &'static str,
        base_url: String,
        api_key: String,
        model: String,
        timeout: Duration,
    ) -> Result<Self, TranslateError> {
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|err| TranslateError::Request(err.to_string()))?;

        Ok(Self {
            provider_name,
            client,
            base_url,
            api_key,
            model,
        })
    }

    fn endpoint(&self) -> String {
        let normalized = self.base_url.trim_end_matches('/');
        if normalized.ends_with("/chat/completions") {
            normalized.to_string()
        } else {
            format!("{normalized}/chat/completions")
        }
    }
}

impl Translator for DeepSeekTranslator {
    fn provider_name(&self) -> &'static str {
        self.provider_name
    }

    fn stream_translate(
        &self,
        request: &TranslationRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<TranslationMeta, TranslateError> {
        let started = Instant::now();
        let body = build_stream_request_body(&self.model, request);

        let response = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .map_err(|err| TranslateError::Request(err.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .unwrap_or_else(|_| "<failed to read body>".to_string());
            return Err(http_status_error(status, body));
        }

        let mut output_chars = 0usize;
        let reader = BufReader::new(response);
        for line_result in reader.lines() {
            let line = line_result.map_err(|err| TranslateError::Request(err.to_string()))?;
            match parse_sse_event(&line)? {
                SseEvent::Ignore => {}
                SseEvent::Done => break,
                SseEvent::Delta(piece) => {
                    output_chars += piece.chars().count();
                    on_delta(&piece);
                }
            }
        }

        Ok(TranslationMeta {
            provider: self.provider_name().to_string(),
            model: self.model.clone(),
            input_chars: request.input.chars().count(),
            output_chars,
            latency_ms: started.elapsed().as_millis(),
            truncated: false,
        })
    }
}

fn build_stream_request_body(model: &str, request: &TranslationRequest) -> serde_json::Value {
    let (system_prompt, user_content) = match request.content_type {
        TranslationContentType::Translate => {
            (TERMINAL_TRANSLATION_SYSTEM_PROMPT, request.input.clone())
        }
        TranslationContentType::Explain => (
            TERMINAL_EXPLAIN_SYSTEM_PROMPT,
            build_explain_user_content(request),
        ),
    };

    json!({
        "model": model,
        "stream": true,
        "messages": [
            {
                "role": "system",
                "content": system_prompt
            },
            {
                "role": "user",
                "content": user_content
            }
        ]
    })
}

fn build_explain_user_content(request: &TranslationRequest) -> String {
    if let Some(command) = request.command.as_ref() {
        let command = command.trim();
        if !command.is_empty() {
            return format!("命令: {command}\n输出:\n{}", request.input);
        }
    }
    request.input.clone()
}

fn read_env_chain(keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Ok(value) = env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn http_status_error(status: StatusCode, body: String) -> TranslateError {
    let clipped = if body.chars().count() > 600 {
        let mut out = String::new();
        for ch in body.chars().take(600) {
            out.push(ch);
        }
        out.push_str("...(clipped)");
        out
    } else {
        body
    };

    TranslateError::HttpStatus {
        status: status.as_u16(),
        body: clipped,
    }
}

enum SseEvent {
    Ignore,
    Done,
    Delta(String),
}

fn parse_sse_event(line: &str) -> Result<SseEvent, TranslateError> {
    if !line.starts_with("data:") {
        return Ok(SseEvent::Ignore);
    }

    let payload = line.trim_start_matches("data:").trim();
    if payload.is_empty() {
        return Ok(SseEvent::Ignore);
    }

    if payload == "[DONE]" {
        return Ok(SseEvent::Done);
    }

    let chunk: StreamChunk = serde_json::from_str(payload)
        .map_err(|err| TranslateError::Parse(format!("{err}; line={payload}")))?;

    let Some(choices) = chunk.choices else {
        return Ok(SseEvent::Ignore);
    };

    let Some(first) = choices.first() else {
        return Ok(SseEvent::Ignore);
    };

    let Some(content) = first.delta.content.clone() else {
        return Ok(SseEvent::Ignore);
    };

    if content.is_empty() {
        return Ok(SseEvent::Ignore);
    }

    Ok(SseEvent::Delta(content))
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Option<Vec<Choice>>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    delta: Delta,
}

#[derive(Debug, Deserialize)]
struct Delta {
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translator::{TranslationContentType, TranslationRequest};

    #[test]
    fn parses_stream_delta() {
        let line = r#"data: {"choices":[{"delta":{"content":"你好"}}]}"#;
        let event = parse_sse_event(line).expect("delta parse should succeed");

        match event {
            SseEvent::Delta(piece) => assert_eq!(piece, "你好"),
            _ => panic!("expected delta event"),
        }
    }

    #[test]
    fn parses_done_event() {
        let line = "data: [DONE]";
        let event = parse_sse_event(line).expect("done parse should succeed");
        assert!(matches!(event, SseEvent::Done));
    }

    #[test]
    fn maps_401_and_429_errors() {
        let unauthorized = http_status_error(StatusCode::UNAUTHORIZED, "bad key".to_string());
        let too_many = http_status_error(StatusCode::TOO_MANY_REQUESTS, "rate limited".to_string());

        match unauthorized {
            TranslateError::HttpStatus { status, body } => {
                assert_eq!(status, 401);
                assert!(body.contains("bad key"));
            }
            _ => panic!("expected http status error"),
        }

        match too_many {
            TranslateError::HttpStatus { status, body } => {
                assert_eq!(status, 429);
                assert!(body.contains("rate limited"));
            }
            _ => panic!("expected http status error"),
        }
    }

    #[test]
    fn builds_stream_request_with_terminal_prompt() {
        let body =
            build_stream_request_body("deepseek-chat", &TranslationRequest::translate("ls -la"));
        assert_eq!(body["stream"], true);
        assert_eq!(body["model"], "deepseek-chat");
        assert_eq!(
            body["messages"][0]["content"],
            TERMINAL_TRANSLATION_SYSTEM_PROMPT
        );
        assert_eq!(body["messages"][1]["content"], "ls -la");
    }

    #[test]
    fn terminal_prompt_prefers_natural_chinese_over_literal_translation() {
        assert!(
            TERMINAL_TRANSLATION_SYSTEM_PROMPT.contains("Avoid word-for-word literal translation")
        );
        assert!(TERMINAL_TRANSLATION_SYSTEM_PROMPT.contains("idiomatic Chinese"));
    }

    #[test]
    fn builds_explain_request_with_command_context() {
        let request = TranslationRequest {
            input: "total 8\n-rw-r--r-- file.txt".to_string(),
            content_type: TranslationContentType::Explain,
            command: Some("ls -la".to_string()),
        };

        let body = build_stream_request_body("deepseek-chat", &request);
        let user_message = body["messages"][1]["content"]
            .as_str()
            .expect("user message should be present");

        assert!(user_message.contains("命令: ls -la"));
        assert!(user_message.contains("输出:"));
    }
}
