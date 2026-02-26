use super::{TranslateError, TranslationMeta, Translator};
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
const TERMINAL_TRANSLATION_SYSTEM_PROMPT: &str = r#"你是专业的终端输出翻译助手，负责将英文命令行输出实时翻译为中文。
要求：
1. 只输出译文，不要解释、总结、注释、前后缀或额外说明。
2. 仅翻译英文自然语言内容；原本已经是中文的内容保持原样，不翻译、不改写。
3. 命令、参数、路径、文件名、URL、IP、端口、环境变量、代码、错误码、日志标识符必须保持原样，不翻译、不改写。
4. 保持原始结构：行序、换行、缩进、列表层级、符号尽量与输入一致。
5. 混合内容按片段处理：仅翻译自然语言部分，技术片段保持原文。
6. 术语翻译应简洁、准确、统一；不确定时保留原文。
7. 禁止臆测或补充输入中不存在的信息。"#;

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
        input: &str,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<TranslationMeta, TranslateError> {
        let started = Instant::now();
        let body = build_stream_request_body(&self.model, input);

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
            input_chars: input.chars().count(),
            output_chars,
            latency_ms: started.elapsed().as_millis(),
            truncated: false,
        })
    }
}

fn build_stream_request_body(model: &str, input: &str) -> serde_json::Value {
    json!({
        "model": model,
        "stream": true,
        "messages": [
            {
                "role": "system",
                "content": TERMINAL_TRANSLATION_SYSTEM_PROMPT
            },
            {
                "role": "user",
                "content": input
            }
        ]
    })
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
        let body = build_stream_request_body("deepseek-chat", "ls -la");
        assert_eq!(body["stream"], true);
        assert_eq!(body["model"], "deepseek-chat");
        assert_eq!(
            body["messages"][0]["content"],
            TERMINAL_TRANSLATION_SYSTEM_PROMPT
        );
        assert_eq!(body["messages"][1]["content"], "ls -la");
    }
}
