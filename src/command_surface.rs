use anyhow::Result;
use clap::{Command as ClapCommand, CommandFactory};
use serde::Serialize;

use crate::{Cli, CommandSurfaceArgs};

#[derive(Debug, Serialize)]
struct CommandSurface {
    schema_version: u8,
    binary: &'static str,
    commands: Vec<SurfaceCommand>,
}

#[derive(Debug, Serialize)]
struct SurfaceCommand {
    name: String,
    about: Option<String>,
    usage: String,
    arguments: Vec<SurfaceArgument>,
    subcommands: Vec<SurfaceCommand>,
}

#[derive(Debug, Serialize)]
struct SurfaceArgument {
    id: String,
    long: Option<String>,
    short: Option<String>,
    help: Option<String>,
    required: bool,
    possible_values: Vec<SurfaceValue>,
}

#[derive(Debug, Serialize)]
struct SurfaceValue {
    name: String,
    help: Option<String>,
}

pub(crate) fn run_command_surface(args: CommandSurfaceArgs) -> Result<()> {
    let surface = build_command_surface();
    let json = serde_json::to_string_pretty(&surface)?;
    if args.json {
        println!("{json}");
    } else {
        println!("{json}");
    }
    Ok(())
}

fn build_command_surface() -> CommandSurface {
    let cli = Cli::command();
    CommandSurface {
        schema_version: 1,
        binary: "auto",
        commands: cli
            .get_subcommands()
            .map(surface_command)
            .collect::<Vec<_>>(),
    }
}

fn surface_command(command: &ClapCommand) -> SurfaceCommand {
    let mut usage_command = command.clone();
    SurfaceCommand {
        name: command.get_name().to_string(),
        about: command.get_about().map(|about| about.to_string()),
        usage: usage_command.render_usage().to_string(),
        arguments: command
            .get_arguments()
            .map(surface_argument)
            .collect::<Vec<_>>(),
        subcommands: command
            .get_subcommands()
            .map(surface_command)
            .collect::<Vec<_>>(),
    }
}

fn surface_argument(arg: &clap::Arg) -> SurfaceArgument {
    let possible_values = arg
        .get_value_parser()
        .possible_values()
        .map(|values| {
            values
                .map(|value| SurfaceValue {
                    name: value.get_name().to_string(),
                    help: value.get_help().map(|help| help.to_string()),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    SurfaceArgument {
        id: arg.get_id().as_str().to_string(),
        long: arg.get_long().map(str::to_string),
        short: arg.get_short().map(|short| short.to_string()),
        help: arg.get_help().map(|help| help.to_string()),
        required: arg.is_required_set(),
        possible_values,
    }
}

#[cfg(test)]
mod tests {
    use super::build_command_surface;

    #[test]
    fn command_surface_includes_parallel_receipt_backfill_action() {
        let surface = build_command_surface();
        let parallel = surface
            .commands
            .iter()
            .find(|command| command.name == "parallel")
            .expect("parallel command");
        let action = parallel
            .arguments
            .iter()
            .find(|argument| argument.id == "action")
            .expect("parallel action argument");

        assert!(action
            .possible_values
            .iter()
            .any(|value| value.name == "receipt-backfill"));
    }
}
