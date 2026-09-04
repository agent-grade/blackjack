use std::fmt;

/// What the argv asked for. Wrapping a program is what the wrapper is for; the
/// rest are the things a user needs before a session, or instead of one.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    Wrap(Command),
    Help,
    Version,
    /// What the checker makes of one line, printed and done with — the way to
    /// try a word list or a config change without wrapping anything.
    Check(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    MissingSeparator,
    EmptyCommand,
    NothingToCheck,
    Unknown(String),
    Unexpected(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSeparator => write!(f, "no `--` separator"),
            Self::EmptyCommand => write!(f, "no command after `--`"),
            Self::NothingToCheck => write!(f, "no text after `--check`"),
            Self::Unknown(flag) => write!(f, "unknown option `{flag}`"),
            Self::Unexpected(argument) => write!(f, "unexpected argument `{argument}`"),
        }
    }
}

impl std::error::Error for ParseError {}

pub const USAGE: &str = "usage: blackjack [--check <text>] -- <command> [args...]";

/// The rest of `--help`, which prints `USAGE` above it. Short enough to read
/// standing up: the README is where a user goes to learn the program, and this
/// is where they go to remember one flag.
pub const HELP: &str = "  -h, --help          this
  -V, --version       the version
      --check <text>  print what the checker makes of one line, and stop

Everything after `--` is the wrapped command and its own arguments, untouched.

  BLACKJACK_PAINT_OFF   set to anything to wrap without painting
  BLACKJACK_DEBUG_LOG   a file to append a decision log to";

/// Options are read only before the separator: everything after `--` belongs to
/// the wrapped command, which has its own `--help` to answer for.
pub fn parse(argv: &[String]) -> Result<Invocation, ParseError> {
    let separator = argv.iter().position(|a| a == "--");
    let mut ours = argv.iter().skip(1).take_while(|a| *a != "--");
    if let Some(first) = ours.next() {
        if !first.starts_with('-') {
            // Not an option, so it is the command — and with no separator in
            // front of it, the separator is the thing to say is missing.
            return Err(match separator {
                Some(_) => ParseError::Unexpected(first.to_string()),
                None => ParseError::MissingSeparator,
            });
        }
        return match first.as_str() {
            "-h" | "--help" => Ok(Invocation::Help),
            "-V" | "--version" => Ok(Invocation::Version),
            "--check" => ours
                .next()
                .cloned()
                .map(Invocation::Check)
                .ok_or(ParseError::NothingToCheck),
            other => Err(ParseError::Unknown(other.to_string())),
        };
    }
    let separator = separator.ok_or(ParseError::MissingSeparator)?;
    let rest = &argv[separator + 1..];
    let (program, args) = rest.split_first().ok_or(ParseError::EmptyCommand)?;
    Ok(Invocation::Wrap(Command {
        program: program.clone(),
        args: args.to_vec(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The wrapped command, which is every invocation that is not a question
    /// about the wrapper itself.
    fn wrapped(items: &[&str]) -> Command {
        match parse(&argv(items)).unwrap() {
            Invocation::Wrap(command) => command,
            other => panic!("not a wrap: {other:?}"),
        }
    }

    #[test]
    fn takes_the_program_after_the_separator() {
        let parsed = wrapped(&["blackjack", "--", "claude"]);
        assert_eq!(parsed.program, "claude");
        assert!(parsed.args.is_empty());
    }

    #[test]
    fn keeps_the_wrapped_commands_own_arguments() {
        let parsed = wrapped(&["blackjack", "--", "claude", "--model", "opus"]);
        assert_eq!(parsed.program, "claude");
        assert_eq!(parsed.args, argv(&["--model", "opus"]));
    }

    /// A second `--` belongs to the wrapped command, not to blackjack.
    #[test]
    fn passes_a_later_separator_through_untouched() {
        let parsed = wrapped(&["blackjack", "--", "cargo", "test", "--", "-q"]);
        assert_eq!(parsed.program, "cargo");
        assert_eq!(parsed.args, argv(&["test", "--", "-q"]));
    }

    #[test]
    fn asks_the_wrapper_about_itself_before_the_separator() {
        assert_eq!(parse(&argv(&["blackjack", "--help"])), Ok(Invocation::Help));
        assert_eq!(parse(&argv(&["blackjack", "-h"])), Ok(Invocation::Help));
        assert_eq!(
            parse(&argv(&["blackjack", "--version"])),
            Ok(Invocation::Version)
        );
        assert_eq!(
            parse(&argv(&["blackjack", "--check", "a teh word"])),
            Ok(Invocation::Check("a teh word".to_string()))
        );
    }

    /// The wrapped command's own options are its own, `--help` included: a user
    /// running `blackjack -- claude --help` is asking claude, not this.
    #[test]
    fn leaves_the_wrapped_commands_options_to_it() {
        assert_eq!(
            wrapped(&["blackjack", "--", "claude", "--help"]).args,
            argv(&["--help"])
        );
        assert_eq!(
            wrapped(&["blackjack", "--", "claude", "-V"]).program,
            "claude"
        );
    }

    #[test]
    fn rejects_an_option_it_does_not_have() {
        assert_eq!(
            parse(&argv(&["blackjack", "--paint"])),
            Err(ParseError::Unknown("--paint".to_string()))
        );
    }

    /// A word before the separator is the wrapped command in the wrong place,
    /// which is a different mistake from a flag nobody knows.
    #[test]
    fn rejects_a_stray_argument_before_the_separator() {
        assert_eq!(
            parse(&argv(&["blackjack", "claude", "--", "sh"])),
            Err(ParseError::Unexpected("claude".to_string()))
        );
    }

    #[test]
    fn rejects_a_check_with_nothing_to_check() {
        assert_eq!(
            parse(&argv(&["blackjack", "--check"])),
            Err(ParseError::NothingToCheck)
        );
    }

    #[test]
    fn rejects_a_missing_separator() {
        assert_eq!(
            parse(&argv(&["blackjack", "claude"])),
            Err(ParseError::MissingSeparator)
        );
    }

    #[test]
    fn rejects_a_separator_with_nothing_after_it() {
        assert_eq!(
            parse(&argv(&["blackjack", "--"])),
            Err(ParseError::EmptyCommand)
        );
    }
}
