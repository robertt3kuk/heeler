//! `hutan help [topic]`: clap's help for a command, and `hutan help agent`, the compact guide
//! for coding agents (the `agent` hooks command keeps its own `hutan agent --help`).

use clap::CommandFactory as _;

use super::print_line;
use crate::args::Cli;
use crate::error::CliError;
use crate::mcp::guide::AGENT_GUIDE;

pub(super) fn run(topic: &[String]) -> Result<(), CliError> {
    if topic.len() == 1 && topic[0] == "agent" {
        return print_line(AGENT_GUIDE.trim_end());
    }
    let mut command = Cli::command();
    let mut current = &mut command;
    for word in topic {
        current = current
            .find_subcommand_mut(word)
            .ok_or_else(|| CliError::Usage(format!("no help for `{}`", topic.join(" "))))?;
    }
    let text = current.render_long_help().to_string();
    print_line(text.trim_end())
}
