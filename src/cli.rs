use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    MissingSeparator,
    EmptyCommand,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSeparator => write!(f, "no `--` separator"),
            Self::EmptyCommand => write!(f, "no command after `--`"),
        }
    }
}

impl std::error::Error for ParseError {}

pub fn parse(argv: &[String]) -> Result<Command, ParseError> {
    let separator = argv
        .iter()
        .position(|a| a == "--")
        .ok_or(ParseError::MissingSeparator)?;
    let rest = &argv[separator + 1..];
    let (program, args) = rest.split_first().ok_or(ParseError::EmptyCommand)?;
    Ok(Command {
        program: program.clone(),
        args: args.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn takes_the_program_after_the_separator() {
        let parsed = parse(&argv(&["blackjack", "--", "claude"])).unwrap();
        assert_eq!(parsed.program, "claude");
        assert!(parsed.args.is_empty());
    }

    #[test]
    fn keeps_the_wrapped_commands_own_arguments() {
        let parsed = parse(&argv(&["blackjack", "--", "claude", "--model", "opus"])).unwrap();
        assert_eq!(parsed.program, "claude");
        assert_eq!(parsed.args, argv(&["--model", "opus"]));
    }

    /// A second `--` belongs to the wrapped command, not to blackjack.
    #[test]
    fn passes_a_later_separator_through_untouched() {
        let parsed = parse(&argv(&["blackjack", "--", "cargo", "test", "--", "-q"])).unwrap();
        assert_eq!(parsed.program, "cargo");
        assert_eq!(parsed.args, argv(&["test", "--", "-q"]));
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
