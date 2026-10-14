//! Context chips: terminal blocks attached to the composer ("Send to agent"). A chip is shown
//! as a short label above the input; when the message is sent its text goes in front of what the
//! user typed, quoted in a fenced block.

/// Longest label of a chip, in characters.
const LABEL_CHARS: usize = 48;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    /// What the chip shows: `$ ls /nope · exit 2`.
    pub label: String,
    /// The text that is sent, already quoted.
    pub quoted: String,
    /// The output was cut at `chat.attach_max_bytes` (or by the daemon).
    pub truncated: bool,
}

impl Chip {
    /// A terminal block: its `command`, its `output` and how it ended.
    pub fn from_block(
        command: Option<&str>,
        exit_code: Option<i32>,
        output: &str,
        truncated: bool,
    ) -> Self {
        let command = command.map(str::trim).filter(|c| !c.is_empty());
        let first_line = command.and_then(|c| c.lines().next()).unwrap_or("(output)");
        let mut label = format!("$ {first_line}");
        if let Some(code) = exit_code {
            label.push_str(&format!(" · exit {code}"));
        }
        if truncated {
            label.push_str(" · cut");
        }
        let label = shorten(&label, LABEL_CHARS);

        let mut block = String::new();
        if let Some(command) = command {
            block.push_str("$ ");
            block.push_str(command);
            block.push('\n');
        }
        block.push_str(output.trim_end_matches('\n'));
        let mut heading = String::from("Context from my terminal");
        if let Some(code) = exit_code {
            heading.push_str(&format!(" (exit {code})"));
        }
        heading.push(':');
        let mut quoted = format!("{heading}\n{}", fenced(&block));
        if truncated {
            quoted.push_str("\n(the output was cut)");
        }
        Self {
            label,
            quoted,
            truncated,
        }
    }
}

/// `text` in a fenced block whose fence is longer than any run of backticks inside.
pub fn fenced(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}\n{text}\n{fence}")
}

fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// The message to send: every chip's quote, then the draft, separated by blank lines.
pub fn compose(chips: &[Chip], draft: &str) -> String {
    let mut parts: Vec<&str> = chips.iter().map(|c| c.quoted.as_str()).collect();
    let draft = draft.trim_end();
    if !draft.trim().is_empty() {
        parts.push(draft);
    }
    parts.join("\n\n")
}

/// `text` cut to at most `max` bytes at a line end when possible; the flag says it was cut.
pub fn cut_to(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_owned(), false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let kept = &text[..end];
    let kept = kept.rfind('\n').map_or(kept, |i| &kept[..i]);
    (kept.to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_is_quoted_with_its_command_and_exit_code() {
        let chip = Chip::from_block(
            Some("ls /nope"),
            Some(2),
            "ls: /nope: No such file\n",
            false,
        );
        assert_eq!(chip.label, "$ ls /nope · exit 2");
        assert_eq!(
            chip.quoted,
            "Context from my terminal (exit 2):\n```\n$ ls /nope\nls: /nope: No such file\n```"
        );
        assert!(!chip.truncated);
    }

    #[test]
    fn the_fence_outgrows_backticks_in_the_text() {
        let quoted = fenced("a ``` b\n````");
        assert!(quoted.starts_with("`````\n"), "{quoted}");
        assert!(quoted.ends_with("\n`````"), "{quoted}");
        assert_eq!(fenced("plain"), "```\nplain\n```");
    }

    #[test]
    fn a_long_label_is_shortened_and_a_cut_is_said() {
        let command = "x".repeat(200);
        let chip = Chip::from_block(Some(&command), None, "out", true);
        assert_eq!(chip.label.chars().count(), LABEL_CHARS);
        assert!(chip.label.ends_with('…'));
        assert!(chip.quoted.ends_with("(the output was cut)"));
    }

    #[test]
    fn output_without_a_command_still_makes_a_chip() {
        let chip = Chip::from_block(None, None, "just output", false);
        assert_eq!(chip.label, "$ (output)");
        assert!(chip.quoted.contains("just output"));
    }

    #[test]
    fn compose_puts_the_chips_before_the_draft() {
        let a = Chip::from_block(Some("a"), Some(0), "1", false);
        let b = Chip::from_block(Some("b"), Some(0), "2", false);
        let message = compose(&[a.clone(), b.clone()], "why?\n");
        assert_eq!(message, format!("{}\n\n{}\n\nwhy?", a.quoted, b.quoted));
        assert_eq!(compose(std::slice::from_ref(&a), "  \n"), a.quoted);
        assert_eq!(compose(&[], "hi"), "hi");
        assert_eq!(compose(&[], " "), "");
    }

    #[test]
    fn cutting_prefers_a_line_end_and_a_character_boundary() {
        assert_eq!(cut_to("short", 10), ("short".to_owned(), false));
        assert_eq!(cut_to("one\ntwo\nthree", 9), ("one\ntwo".to_owned(), true));
        let (cut, flagged) = cut_to("ééééé", 5);
        assert!(flagged);
        assert_eq!(cut, "éé");
    }
}
