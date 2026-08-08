mod cli;
mod pty;

use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();

    let command = match cli::parse(&argv) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("blackjack: {error}");
            eprintln!("usage: blackjack -- <command> [args...]");
            return ExitCode::FAILURE;
        }
    };

    match pty::run(&command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("blackjack: {error:#}");
            ExitCode::FAILURE
        }
    }
}
