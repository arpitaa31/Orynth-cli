use std::process::ExitCode;

use orynth_cli::{
    TerminalCliCommand, execute_terminal_operation, parse_terminal_cli, render_terminal_plan,
    undo_terminal_operation,
};
use orynth_terminal_tools::TerminalEnvironment;

fn main() -> ExitCode {
    match parse_terminal_cli(std::env::args().skip(1)) {
        Ok(TerminalCliCommand::Help) => {
            print_help();
            ExitCode::SUCCESS
        }
        Ok(TerminalCliCommand::Plan(operation)) => match TerminalEnvironment::detect()
            .map_err(|error| error.to_string())
            .and_then(|environment| {
                render_terminal_plan(&operation, &environment).map_err(|error| error.to_string())
            }) {
            Ok(report) => {
                print!("{report}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("orynth-shell: {error}");
                ExitCode::from(2)
            }
        },
        Ok(TerminalCliCommand::Translate(operation)) => match TerminalEnvironment::detect()
            .map_err(|error| error.to_string())
            .and_then(|environment| {
                render_terminal_plan(&operation, &environment).map_err(|error| error.to_string())
            }) {
            Ok(report) => {
                println!("Translation: bounded local grammar");
                print!("{report}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("orynth-shell: {error}");
                ExitCode::from(2)
            }
        },
        Ok(TerminalCliCommand::Undo) => match TerminalEnvironment::detect()
            .map_err(|error| error.to_string())
            .and_then(|environment| {
                undo_terminal_operation(&environment).map_err(|error| error.to_string())
            }) {
            Ok(report) => {
                print!("{report}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("orynth-shell: {error}");
                ExitCode::from(2)
            }
        },
        Ok(TerminalCliCommand::Execute {
            operation,
            confirmed,
        }) => match TerminalEnvironment::detect()
            .map_err(|error| error.to_string())
            .and_then(|environment| {
                execute_terminal_operation(&operation, &environment, confirmed)
                    .map_err(|error| error.to_string())
            }) {
            Ok(report) => {
                print!("{report}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("orynth-shell: {error}");
                ExitCode::from(2)
            }
        },
        Err(error) => {
            eprintln!("orynth-shell: {error}");
            eprintln!();
            print_help();
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!("orynth-shell terminal planning and execution boundary");
    println!();
    println!("Usage:");
    println!("  orynth-shell plan find --root <path> --pattern <glob> --limit <n>");
    println!("  orynth-shell plan move --from <path> --to <path>");
    println!("  orynth-shell plan copy --from <path> --to <path>");
    println!("  orynth-shell plan remove --path <path>");
    println!("  orynth-shell plan list-processes");
    println!("  orynth-shell plan git <action> [args...]");
    println!("  orynth-shell plan shell <command tokens...>");
    println!("  orynth-shell translate <bounded terminal phrase>");
    println!("  orynth-shell undo");
    println!("  orynth-shell execute find --root <path> --pattern <glob> --limit <n>");
    println!("  orynth-shell execute move --from <path> --to <path> --confirm");
    println!("  orynth-shell execute copy --from <path> --to <path> --confirm");
    println!("  orynth-shell execute remove --path <path> --confirm");
    println!();
    println!("Plan commands are read-only; execute supports only rooted filesystem operations.");
    println!("Mutating execute commands require --confirm and are verified by the tool runtime.");
}
