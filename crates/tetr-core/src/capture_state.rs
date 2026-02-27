use regex::Regex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Idle,
    Exec,
    Filter,
    SuspendedInteractive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerReason {
    Prompt,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggeredCapture {
    pub text: String,
    pub reason: TriggerReason,
}

#[derive(Debug)]
pub struct CaptureState {
    mode: CaptureMode,
    prompt_regex: Regex,
    pending_command: Option<String>,
    command_echo_skipped: bool,
    lines: Vec<String>,
    last_output_at: Option<Instant>,
    idle_timeout: Duration,
    interactive_mode: bool,
}

impl CaptureState {
    pub fn new(idle_ms: u64) -> Self {
        Self {
            mode: CaptureMode::Idle,
            prompt_regex: Regex::new(r".*[#$>❯]\s*$").expect("prompt regex must be valid"),
            pending_command: None,
            command_echo_skipped: false,
            lines: Vec::new(),
            last_output_at: None,
            idle_timeout: Duration::from_millis(idle_ms),
            interactive_mode: false,
        }
    }

    pub fn mode(&self) -> CaptureMode {
        self.mode
    }

    pub fn note_user_command(&mut self, command: &str) {
        self.mode = CaptureMode::Exec;
        self.pending_command = match normalize_command_for_match(command) {
            normalized if normalized.is_empty() => None,
            normalized => Some(normalized),
        };
        self.command_echo_skipped = false;
        self.lines.clear();
        self.last_output_at = None;
    }

    pub fn ingest_chunk(
        &mut self,
        raw_chunk: &str,
        clean_chunk: &str,
        now: Instant,
    ) -> Option<TriggeredCapture> {
        if raw_chunk.contains("\u{1b}[?1049h") {
            self.interactive_mode = true;
            self.mode = CaptureMode::SuspendedInteractive;
            self.lines.clear();
            return None;
        }

        if raw_chunk.contains("\u{1b}[?1049l") {
            self.interactive_mode = false;
            self.mode = CaptureMode::Idle;
            self.pending_command = None;
            self.command_echo_skipped = false;
            self.lines.clear();
            return None;
        }

        if self.interactive_mode {
            return None;
        }

        if clean_chunk.is_empty() {
            return None;
        }

        // Shell echoes characters while the user is typing before pressing Enter.
        // Those chunks usually have no line terminator and should not start fallback capture.
        if matches!(self.mode, CaptureMode::Idle)
            && !chunk_has_line_terminator(clean_chunk)
            && !self.prompt_regex.is_match(&normalize_shell_text(clean_chunk))
        {
            return None;
        }

        self.last_output_at = Some(now);

        for line in clean_chunk.split('\n') {
            let normalized_line = normalize_shell_text(line);
            if normalized_line.is_empty() {
                continue;
            }

            let is_prompt = self.prompt_regex.is_match(&normalized_line);

            if matches!(self.mode, CaptureMode::Exec | CaptureMode::Filter) && is_prompt {
                self.capture_inline_output_before_prompt(&normalized_line);
                return self.emit(TriggerReason::Prompt);
            }

            if !matches!(self.mode, CaptureMode::Exec | CaptureMode::Filter) || is_prompt {
                continue;
            }

            if !self.command_echo_skipped && self.should_skip_command_echo(&normalized_line) {
                self.command_echo_skipped = true;
                self.mode = CaptureMode::Filter;
                continue;
            }

            self.command_echo_skipped = true;
            self.mode = CaptureMode::Filter;
            self.lines.push(normalized_line);
        }

        None
    }

    pub fn flush_if_idle(&mut self, now: Instant) -> Option<TriggeredCapture> {
        if !matches!(self.mode, CaptureMode::Filter) {
            return None;
        }

        let Some(last_output_at) = self.last_output_at else {
            return None;
        };

        if now.duration_since(last_output_at) < self.idle_timeout {
            return None;
        }

        self.emit(TriggerReason::Idle)
    }

    fn is_command_echo(&self, normalized_line: &str) -> bool {
        let Some(command) = self.pending_command.as_ref() else {
            return false;
        };

        normalize_command_for_match(normalized_line) == *command
    }

    fn should_skip_command_echo(&self, normalized_line: &str) -> bool {
        if self.is_command_echo(normalized_line) {
            return true;
        }

        // stdin-side command capture can be empty when the shell line was produced via
        // history recall or complex line editing. In that case, skip obvious command echoes.
        self.pending_command.is_none() && looks_like_shell_command_line(normalized_line)
    }

    fn capture_inline_output_before_prompt(&mut self, normalized_line: &str) {
        let Some(prefix) = prompt_line_prefix(normalized_line) else {
            return;
        };
        let candidate = prefix.trim();
        if candidate.is_empty() || looks_like_prompt_inline_prefix(candidate) {
            return;
        }

        if !self.command_echo_skipped && self.should_skip_command_echo(candidate) {
            self.command_echo_skipped = true;
            self.mode = CaptureMode::Filter;
            return;
        }

        self.command_echo_skipped = true;
        self.mode = CaptureMode::Filter;
        self.lines.push(candidate.to_string());
    }

    fn emit(&mut self, reason: TriggerReason) -> Option<TriggeredCapture> {
        let mut lines = self.lines.clone();
        trim_trailing_prompt_noise(&mut lines);
        let text = lines.join("\n").trim().to_string();

        self.mode = CaptureMode::Idle;
        self.pending_command = None;
        self.command_echo_skipped = false;
        self.lines.clear();
        self.last_output_at = None;

        if text.is_empty() {
            return None;
        }

        Some(TriggeredCapture { text, reason })
    }
}

fn normalize_shell_text(line: &str) -> String {
    line.trim_end_matches('\r').trim().to_string()
}

fn chunk_has_line_terminator(chunk: &str) -> bool {
    chunk.contains('\n') || chunk.contains('\r')
}

fn normalize_command_for_match(raw: &str) -> String {
    strip_ansi_like_control(raw)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_ansi_like_control(input: &str) -> String {
    let mut output = String::new();
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some('O') => {
                    chars.next();
                    let _ = chars.next();
                }
                Some(_) => {
                    let _ = chars.next();
                }
                None => {}
            }
            continue;
        }

        if ch.is_control() {
            continue;
        }

        output.push(ch);
    }

    output
}

fn prompt_line_prefix(line: &str) -> Option<&str> {
    let trimmed = line.trim_end();
    let (index, last_char) = trimmed.char_indices().last()?;
    if !matches!(last_char, '$' | '#' | '>' | '❯') {
        return None;
    }
    Some(trimmed[..index].trim_end())
}

fn looks_like_prompt_inline_prefix(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }

    if trimmed.starts_with("PS ") {
        return true;
    }

    if trimmed.contains('@') && trimmed.contains(':') && !trimmed.contains(' ') {
        return true;
    }

    trimmed.ends_with('~') || trimmed.ends_with('/') || trimmed.ends_with('\\')
}

fn looks_like_shell_command_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    let Some(first_token) = trimmed.split_whitespace().next() else {
        return false;
    };

    if !(looks_like_command_token(first_token) || looks_like_env_assignment(first_token)) {
        return false;
    }

    if trimmed.contains("&&")
        || trimmed.contains("||")
        || trimmed.contains('|')
        || trimmed.contains('>')
        || trimmed.contains('<')
        || trimmed.contains("--")
        || trimmed.contains(" -")
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || looks_like_env_assignment(first_token)
        || is_known_shell_command(first_token)
    {
        return true;
    }

    let word_count = trimmed.split_whitespace().count();
    word_count <= 3
        && first_token.chars().all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-' | '.')
        })
}

fn looks_like_command_token(token: &str) -> bool {
    if token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with("~/")
        || token.starts_with('/')
    {
        return true;
    }

    token
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
}

fn looks_like_env_assignment(token: &str) -> bool {
    let Some((name, value)) = token.split_once('=') else {
        return false;
    };

    !name.is_empty()
        && !value.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

fn is_known_shell_command(token: &str) -> bool {
    let cmd = token.strip_prefix("sudo").map(str::trim).unwrap_or(token);
    matches!(
        cmd,
        "cd" | "ls"
            | "pwd"
            | "cat"
            | "grep"
            | "find"
            | "awk"
            | "sed"
            | "curl"
            | "wget"
            | "git"
            | "npm"
            | "pnpm"
            | "yarn"
            | "node"
            | "python"
            | "python3"
            | "pip"
            | "pip3"
            | "cargo"
            | "go"
            | "brew"
            | "docker"
            | "kubectl"
            | "make"
            | "cmake"
            | "bash"
            | "zsh"
            | "sh"
            | "source"
            | "export"
            | "unset"
            | "echo"
            | "date"
            | "mv"
            | "cp"
            | "rm"
            | "mkdir"
            | "rmdir"
    )
}

fn trim_trailing_prompt_noise(lines: &mut Vec<String>) {
    while let Some(last) = lines.last() {
        let trimmed = last.trim();
        if trimmed.is_empty() {
            lines.pop();
            continue;
        }

        if looks_like_prompt_header(trimmed)
            || looks_like_prompt_symbol_line(trimmed)
            || looks_like_prompt_decoration(trimmed)
        {
            lines.pop();
            continue;
        }

        break;
    }
}

fn looks_like_prompt_header(line: &str) -> bool {
    line.contains(" at ")
        && line.contains(':')
        && line.chars().filter(|c| c.is_ascii_digit()).count() >= 4
}

fn looks_like_prompt_symbol_line(line: &str) -> bool {
    line.len() <= 2
        && line
            .chars()
            .all(|c| matches!(c, '%' | '~' | '$' | '#' | '>' | '❯'))
}

fn looks_like_prompt_decoration(line: &str) -> bool {
    if line.len() < 8 {
        return false;
    }
    let alnum = line.chars().filter(|c| c.is_ascii_alphanumeric()).count();
    let has_dots = line.contains('·') || line.contains("...");
    has_dots && alnum <= 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn captures_until_prompt_and_skips_echo() {
        let start = Instant::now();
        let mut state = CaptureState::new(300);

        state.note_user_command("ls -la");
        let triggered =
            state.ingest_chunk("ls -la\r\nfoo\nbar\n$ ", "ls -la\r\nfoo\nbar\n$ ", start);

        assert_eq!(
            triggered,
            Some(TriggeredCapture {
                text: "foo\nbar".to_string(),
                reason: TriggerReason::Prompt,
            })
        );
        assert_eq!(state.mode(), CaptureMode::Idle);
    }

    #[test]
    fn captures_on_idle_timeout() {
        let start = Instant::now();
        let mut state = CaptureState::new(300);

        state.note_user_command("echo hi");
        assert_eq!(
            state.ingest_chunk("echo hi\r\nhi\r\n", "echo hi\r\nhi\r\n", start),
            None
        );

        assert_eq!(
            state.flush_if_idle(start + Duration::from_millis(250)),
            None
        );

        assert_eq!(
            state.flush_if_idle(start + Duration::from_millis(350)),
            Some(TriggeredCapture {
                text: "hi".to_string(),
                reason: TriggerReason::Idle,
            })
        );
    }

    #[test]
    fn ignores_empty_output() {
        let start = Instant::now();
        let mut state = CaptureState::new(300);

        state.note_user_command("pwd");
        assert_eq!(state.ingest_chunk("\r\n", "\r\n", start), None);
        assert_eq!(
            state.flush_if_idle(start + Duration::from_millis(500)),
            None
        );
    }

    #[test]
    fn suspends_translation_in_alternate_screen() {
        let start = Instant::now();
        let mut state = CaptureState::new(300);

        state.note_user_command("vim");
        assert_eq!(
            state.ingest_chunk("\u{1b}[?1049h", "", start),
            None,
            "entering alternate screen should suspend translation"
        );
        assert_eq!(state.mode(), CaptureMode::SuspendedInteractive);

        assert_eq!(
            state.ingest_chunk(
                "file content",
                "file content",
                start + Duration::from_millis(1)
            ),
            None,
            "interactive output should be ignored"
        );

        assert_eq!(
            state.ingest_chunk("\u{1b}[?1049l", "", start + Duration::from_millis(2)),
            None,
            "leaving alternate screen resumes translation"
        );

        state.note_user_command("echo ok");
        let triggered = state.ingest_chunk(
            "echo ok\nok\nPS C:\\\\> ",
            "echo ok\nok\nPS C:\\\\> ",
            start + Duration::from_millis(3),
        );

        assert_eq!(
            triggered,
            Some(TriggeredCapture {
                text: "ok".to_string(),
                reason: TriggerReason::Prompt,
            })
        );
    }

    #[test]
    fn does_not_emit_when_only_prompt_arrives() {
        let start = Instant::now();
        let mut state = CaptureState::new(300);

        state.note_user_command("\n");
        let triggered = state.ingest_chunk("$ ", "$ ", start);
        assert_eq!(triggered, None);
    }

    #[test]
    fn does_not_capture_in_progress_input_before_enter() {
        let start = Instant::now();
        let mut state = CaptureState::new(300);

        // User is still typing, shell echoes characters without a line terminator.
        assert_eq!(state.ingest_chunk("tetr", "tetr", start), None);
        assert_eq!(
            state.flush_if_idle(start + Duration::from_millis(350)),
            None
        );
        assert_eq!(state.mode(), CaptureMode::Idle);
    }

    #[test]
    fn does_not_capture_output_without_enter_trigger() {
        let start = Instant::now();
        let mut state = CaptureState::new(300);

        assert_eq!(state.ingest_chunk("hello\n$ ", "hello\n$ ", start), None);
        assert_eq!(
            state.flush_if_idle(start + Duration::from_millis(350)),
            None
        );
        assert_eq!(state.mode(), CaptureMode::Idle);
    }

    #[test]
    fn strips_prompt_noise_lines_from_tail() {
        let start = Instant::now();
        let mut state = CaptureState::new(300);

        state.note_user_command("curl -s https://api.github.com/zen");
        let triggered = state.ingest_chunk(
            "curl -s https://api.github.com/zen\nSpeak like a human.\n%\n~ ........ at 17:50:50\n❯ ",
            "curl -s https://api.github.com/zen\nSpeak like a human.\n%\n~ ........ at 17:50:50\n❯ ",
            start,
        );

        assert_eq!(
            triggered,
            Some(TriggeredCapture {
                text: "Speak like a human.".to_string(),
                reason: TriggerReason::Prompt,
            })
        );
    }

    #[test]
    fn skips_command_echo_when_command_capture_is_empty() {
        let start = Instant::now();
        let mut state = CaptureState::new(300);

        // Simulates history recall/editing paths where stdin-side command capture is empty.
        state.note_user_command("");
        let triggered = state.ingest_chunk(
            "curl -s https://api.github.com/zen\nKeep it logically awesome.\n$ ",
            "curl -s https://api.github.com/zen\nKeep it logically awesome.\n$ ",
            start,
        );

        assert_eq!(
            triggered,
            Some(TriggeredCapture {
                text: "Keep it logically awesome.".to_string(),
                reason: TriggerReason::Prompt,
            })
        );
    }

    #[test]
    fn keeps_natural_sentence_when_command_capture_is_empty() {
        let start = Instant::now();
        let mut state = CaptureState::new(300);

        state.note_user_command("");
        let triggered = state.ingest_chunk(
            "Keep it logically awesome.\n$ ",
            "Keep it logically awesome.\n$ ",
            start,
        );

        assert_eq!(
            triggered,
            Some(TriggeredCapture {
                text: "Keep it logically awesome.".to_string(),
                reason: TriggerReason::Prompt,
            })
        );
    }

    #[test]
    fn captures_output_when_prompt_sticks_to_same_line() {
        let start = Instant::now();
        let mut state = CaptureState::new(300);

        state.note_user_command("");
        let triggered = state.ingest_chunk(
            "curl -s https://api.github.com/zen\n{\"message\":\"API rate limit exceeded\"}$ ",
            "curl -s https://api.github.com/zen\n{\"message\":\"API rate limit exceeded\"}$ ",
            start,
        );

        assert_eq!(
            triggered,
            Some(TriggeredCapture {
                text: "{\"message\":\"API rate limit exceeded\"}".to_string(),
                reason: TriggerReason::Prompt,
            })
        );
    }

    #[test]
    fn does_not_capture_powershell_prompt_inline_prefix_as_output() {
        let start = Instant::now();
        let mut state = CaptureState::new(300);

        state.note_user_command("");
        let triggered = state.ingest_chunk(
            "echo hi\nPS C:\\Users\\colin> ",
            "echo hi\nPS C:\\Users\\colin> ",
            start,
        );

        assert_eq!(triggered, None);
    }
}
