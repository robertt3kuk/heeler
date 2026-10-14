//! `heeler integration install | uninstall | status`: local file operations on the agents'
//! configuration, no daemon involved.

use super::print_line;
use crate::args::IntegrationCommand;
use crate::error::CliError;
use crate::integration;

pub(super) fn run(json: bool, command: IntegrationCommand) -> Result<(), CliError> {
    let (action, args) = match command {
        IntegrationCommand::Install(args) => ("install", args),
        IntegrationCommand::Uninstall(args) => ("uninstall", args),
        IntegrationCommand::Status(args) => ("status", args),
    };
    let dir = integration::config_dir(args.target, args.dir.as_deref())?;
    let report = match action {
        "install" => integration::install(args.target, &dir)?,
        "uninstall" => integration::uninstall(args.target, &dir)?,
        _ => integration::status(args.target, &dir)?,
    };
    if json {
        let text = serde_json::to_string(&report)
            .map_err(|e| CliError::Failed(format!("encoding the report: {e}")))?;
        print_line(&text)
    } else {
        print_line(&integration::describe(action, &report))
    }
}
