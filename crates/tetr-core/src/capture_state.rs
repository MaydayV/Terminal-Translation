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
        self.pending_command = Some(normalize_shell_text(command));
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

        self.last_output_at = Some(now);

        for line in clean_chunk.split('\n') {
            let normalized_line = normalize_shell_text(line);
            if normalized_line.is_empty() {
                continue;
            }

            if matches!(self.mode, CaptureMode::Exec | CaptureMode::Filter)
                && self.prompt_regex.is_match(&normalized_line)
            {
                return self.emit(TriggerReason::Prompt);
            }

            if !matches!(self.mode, CaptureMode::Exec | CaptureMode::Filter) {
                continue;
            }

            if !self.command_echo_skipped && self.is_command_echo(&normalized_line) {
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

        !command.is_empty() && normalized_line == command
    }

    fn emit(&mut self, reason: TriggerReason) -> Option<TriggeredCapture> {
        let text = self.lines.join("\n").trim().to_string();

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
}
