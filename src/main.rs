mod checker;
mod cli;
mod config;
mod harper;
mod input;
mod log;
mod painter;
mod parser;
mod picker;
mod prose;
mod pty;
mod resolve;
mod screen;
mod tap;

use std::process::ExitCode;

use checker::{Edit, Flag, worded};
use cli::Invocation;
use config::Config;

/// An argv the program cannot act on, told apart from a program that ran and
/// failed: a caller's `||` should not read the two the same way.
const USAGE_ERROR: u8 = 2;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();

    match cli::parse(&argv) {
        Ok(Invocation::Wrap(command)) => wrap(&command),
        Ok(Invocation::Help) => {
            println!(
                "blackjack — a terminal spell-check overlay\n\n{}\n\n{}",
                cli::USAGE,
                cli::HELP
            );
            ExitCode::SUCCESS
        }
        Ok(Invocation::Version) => {
            println!("blackjack {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Invocation::Check(text)) => check(&text),
        Err(error) => {
            eprintln!("blackjack: {error}");
            eprintln!("{}", cli::USAGE);
            ExitCode::from(USAGE_ERROR)
        }
    }
}

fn wrap(command: &cli::Command) -> ExitCode {
    match pty::run(command) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("blackjack: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// One line put to the checker the way the painter would put it, and the answer
/// printed. A config the user got wrong is the answer here rather than
/// something to carry on without: the question they asked was about the config.
fn check(text: &str) -> ExitCode {
    let config = match Config::strict() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("blackjack: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(warning) = config.grammar_warning() {
        eprintln!("blackjack: {warning}");
    }
    let words = config.words_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    );
    let asked = prose::filtered(text);
    for flag in config.checker(words.as_deref()).check(&asked) {
        if prose::intact(text, &flag.span) {
            println!("{}", flagged(text, &flag));
        }
    }
    ExitCode::SUCCESS
}

/// A flag as a line to read: where it is, the word it stands over, and what is
/// offered for it.
fn flagged(text: &str, flag: &Flag) -> String {
    let word = worded(text, &flag.span);
    let offers: Vec<String> = flag.suggestions.iter().map(offered).collect();
    let offers = match offers.is_empty() {
        true => "(no suggestion)".to_string(),
        false => offers.join(", "),
    };
    format!("{}..{}  {word}  → {offers}", flag.span.start, flag.span.end)
}

fn offered(edit: &Edit) -> String {
    match edit {
        Edit::Replace(word) => word.clone(),
        Edit::InsertAfter(word) => format!("+{word}"),
        Edit::Remove => "(remove)".to_string(),
    }
}
