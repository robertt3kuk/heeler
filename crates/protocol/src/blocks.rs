//! Terminal blocks (protocol 0.5, capability `terminal.blocks`): one prompt + command + output
//! unit derived from OSC 133 marks, addressed by absolute line numbers (`docs/protocol.md`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use staffy_core::TerminalId;

/// Most blocks one `terminal.blocks` call returns, whatever `limit` asks for.
pub const BLOCKS_MAX_LIMIT: u32 = 500;

/// Where a block is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockState {
    /// The prompt is shown; the user has not run a command yet.
    Prompt,
    /// The command runs (`OSC 133;C` seen, no `D` yet).
    Running,
    /// The command ended (`OSC 133;D`, or the next prompt started without one).
    Finished,
    /// Lines of the block were evicted from the scrollback or erased (`ESC[3J`, `ESC c`). The
    /// other fields keep what was known; text of the lost lines is unavailable.
    Truncated,
}

/// One prompt, its command and the command's output.
///
/// Lines are absolute: line 0 is the first row the terminal ever scrolled into its history,
/// numbers only grow and never change when older lines are evicted or erased. The first row of
/// the viewport has the line number `viewport_top` of the `terminal.blocks` answer; the row
/// `n` above it (`terminal.history` `from_line = n - 1`) has `viewport_top - n`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    /// Monotonic per terminal, starting at 1.
    pub id: u64,
    /// Line of the prompt start (`OSC 133;A`).
    pub prompt_line: u64,
    /// Line where the command input starts (`OSC 133;B`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_line: Option<u64>,
    /// First line of the output (`OSC 133;C`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_start: Option<u64>,
    /// End of the output, exclusive: the line after the last output line (`OSC 133;D`). The
    /// output occupies `output_start..end_line`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u64>,
    /// The command as the user typed it: the text between `B` and `C` read from the screen
    /// (soft-wrapped rows joined, hard line breaks kept as `\n`), at most 4 KiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Unix milliseconds: when the prompt started (`A`); from the moment the command started
    /// running (`C`) on, when the command started.
    pub started_at_ms: u64,
    /// Unix milliseconds of `D`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    /// Working directory the shell reported (`OSC 7`) for this prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    pub state: BlockState,
}

/// `terminal.blocks`: blocks newest first, at most `limit` (clamped to [`BLOCKS_MAX_LIMIT`]),
/// all with `id < before_id` when it is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalBlocks {
    pub terminal_id: TerminalId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_id: Option<u64>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalBlocksResult {
    /// Newest first.
    pub blocks: Vec<Block>,
    /// Absolute line number of the viewport's first row when the daemon answered.
    pub viewport_top: u64,
}

/// Which text of a block `terminal.block_text` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockPart {
    Command,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalBlockText {
    pub terminal_id: TerminalId,
    pub block_id: u64,
    pub part: BlockPart,
    /// Longest `text` in bytes; the daemon clamps it to its own limit.
    pub max_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalBlockTextResult {
    /// Plain text: soft-wrapped rows joined, trailing spaces and trailing empty lines removed,
    /// lines separated by `\n`.
    pub text: String,
    /// The text is incomplete: cut at `max_bytes`, or its first lines were evicted/erased.
    pub truncated: bool,
}
