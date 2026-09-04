mod cli;
mod input;
mod parser;
mod pty;
mod screen;
mod tap;

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
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("blackjack: {error:#}");
            ExitCode::FAILURE
        }
    }
}
