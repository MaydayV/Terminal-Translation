#[derive(Debug, Clone)]
pub struct TruncationConfig {
    pub max_chars: usize,
    pub tail_lines: usize,
    pub max_error_lines: usize,
}

impl Default for TruncationConfig {
    fn default() -> Self {
        Self {
            max_chars: 3_500,
            tail_lines: 20,
            max_error_lines: 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncationResult {
    pub text: String,
    pub truncated: bool,
    pub original_chars: usize,
}

pub fn truncate_for_translation(input: &str, config: &TruncationConfig) -> TruncationResult {
    let original_chars = input.chars().count();

    if original_chars <= config.max_chars {
        return TruncationResult {
            text: input.trim().to_string(),
            truncated: false,
            original_chars,
        };
    }

    let mut selected_lines: Vec<String> = Vec::new();
    let mut error_count = 0usize;

    let all_lines: Vec<&str> = input.lines().collect();
    for line in &all_lines {
        if error_count >= config.max_error_lines {
            break;
        }

        let lower = line.to_ascii_lowercase();
        if lower.contains("error")
            || lower.contains("failed")
            || lower.contains("exception")
            || lower.contains("panic")
            || lower.contains("fatal")
            || lower.contains("denied")
            || lower.contains("not found")
        {
            let normalized = line.trim();
            if !normalized.is_empty() {
                selected_lines.push(normalized.to_string());
                error_count += 1;
            }
        }
    }

    let tail: Vec<String> = all_lines
        .iter()
        .rev()
        .take(config.tail_lines)
        .rev()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();

    if !selected_lines.is_empty() && !tail.is_empty() {
        selected_lines.push("...".to_string());
    }

    selected_lines.extend(tail);

    let mut output = selected_lines.join("\n");
    if output.chars().count() > config.max_chars {
        let trimmed: String = output
            .chars()
            .rev()
            .take(config.max_chars)
            .collect::<Vec<char>>()
            .into_iter()
            .rev()
            .collect();
        output = format!("...\n{trimmed}");
    }

    TruncationResult {
        text: output,
        truncated: true,
        original_chars,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_short_output_intact() {
        let cfg = TruncationConfig {
            max_chars: 200,
            tail_lines: 5,
            max_error_lines: 5,
        };

        let result = truncate_for_translation("hello\nworld", &cfg);
        assert_eq!(result.text, "hello\nworld");
        assert!(!result.truncated);
    }

    #[test]
    fn keeps_errors_and_tail_for_long_output() {
        let cfg = TruncationConfig {
            max_chars: 120,
            tail_lines: 3,
            max_error_lines: 2,
        };

        let input = [
            "step 1",
            "step 2",
            "ERROR: network timeout",
            "step 4",
            "step 5",
            "FAILED to resolve module",
            "step 7",
            "tail A",
            "tail B",
            "tail C",
        ]
        .join("\n");

        let result = truncate_for_translation(&input.repeat(4), &cfg);
        assert!(result.truncated);
        assert!(result.text.contains("ERROR: network timeout"));
        assert!(result.text.contains("FAILED to resolve module"));
        assert!(result.text.contains("tail C"));
    }

    #[test]
    fn enforces_max_chars_after_selection() {
        let cfg = TruncationConfig {
            max_chars: 40,
            tail_lines: 5,
            max_error_lines: 5,
        };
        let long_line = "x".repeat(200);
        let result = truncate_for_translation(&long_line, &cfg);

        assert!(result.truncated);
        assert!(result.text.chars().count() <= 44);
        assert!(result.text.starts_with("..."));
    }
}
