//! Which of what the user typed is prose at all. The wrapper is pointed at a
//! terminal, where a line is as likely to be a path, a flag or an identifier as
//! it is a sentence, and a checker asked about those has nothing useful to say
//! about any of them. So they are taken out of the line before it is asked —
//! blanked rather than removed, character for character, so that everything
//! said about what is left still counts from the same place.

/// The characters that make a token something other than a word: a path, a
/// namespace, a host, a handle, a home directory, a version.
const NOT_A_WORD: [char; 7] = ['/', '\\', ':', '#', '@', '~', '_'];

/// The line with everything that is not prose blanked out. Same length, and
/// every character that is still there is where it was, so a span into this is
/// a span into what the user typed.
pub fn blanked(line: &str) -> String {
    let mut prose = String::with_capacity(line.len());
    let mut quoted = false;
    for token in line.split_inclusive(char::is_whitespace) {
        let word = token.trim_end();
        let ticks = word.matches('`').count();
        // Code the user marked as code. The run stays blanked until its closing
        // tick, and an unclosed one blanks the rest of the line — which is what
        // it looks like on the screen too.
        let inside = quoted;
        quoted ^= ticks % 2 == 1;
        match inside || quoted || is_code(word) {
            true => prose.extend(token.chars().map(blank)),
            false => prose.push_str(token),
        }
    }
    prose
}

/// Whitespace is what tokens are counted by, so it is the one thing a blank
/// must not become.
fn blank(character: char) -> char {
    match character.is_whitespace() {
        true => character,
        false => ' ',
    }
}

/// Whether a token is code rather than a word. Every test here is one a reader
/// would make at a glance: something in it that words do not have.
fn is_code(token: &str) -> bool {
    // A flag, a shell variable, and code the user marked as code: a backtick
    // pair that opens and closes inside one token, which the run the caller
    // keeps never sees.
    if token.starts_with('-') || token.starts_with('$') || token.contains('`') {
        return true;
    }
    if token
        .chars()
        .any(|c| NOT_A_WORD.contains(&c) || c.is_ascii_digit())
    {
        return true;
    }
    dotted(token) || cased(token)
}

/// A dot with letters both sides — a file extension, a host, a qualified name.
/// A dot ending a sentence has nothing after it and is left alone.
fn dotted(word: &str) -> bool {
    let characters: Vec<char> = word.chars().collect();
    characters.windows(3).any(|around| {
        around[1] == '.' && around[0].is_alphanumeric() && around[2].is_alphanumeric()
    })
}

/// A capital inside the word, after a small letter: `camelCase`, `PascalCase`.
/// A capital that starts the word is a sentence doing its job.
fn cased(word: &str) -> bool {
    word.chars()
        .zip(word.chars().skip(1))
        .any(|(before, after)| before.is_lowercase() && after.is_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a terminal is full of, and none of it is anything a dictionary
    /// should be asked about.
    #[test]
    fn what_a_terminal_is_full_of_is_not_prose() {
        for code in [
            "src/pty.rs",
            "C:\\Users\\x",
            "std::mem::take",
            "#tag",
            "yurii@example.com",
            "~/notes",
            "parking_bar",
            "--release",
            "$HOME",
            "v2",
            "40x100",
            "example.com",
            "https://example.com/x",
            "camelCase",
            "PascalCase",
        ] {
            assert!(is_code(code), "{code} read as prose");
        }
    }

    /// And what it is not full of. A capital starting a word is a sentence, a
    /// dot ending one is a full stop, and a hyphen inside one is a word.
    #[test]
    fn ordinary_words_are_prose() {
        // `harper-core` is here on purpose: a hyphenated crate name and a
        // hyphenated compound are the same shape, and nothing but a dictionary
        // tells them apart.
        for word in [
            "the",
            "The",
            "HELLO",
            "sentence.",
            "well-known",
            "harper-core",
            "it's",
            "teh",
            "recieve",
        ] {
            assert!(!is_code(word), "{word} read as code");
        }
    }

    /// Blanked, not removed: everything left is where it was, so a span into
    /// what the checker reads is a span into what the user typed.
    #[test]
    fn what_is_left_is_where_it_was() {
        let line = "open src/pty.rs and teh file";

        let prose = blanked(line);

        assert_eq!(prose, "open            and teh file");
        assert_eq!(prose.chars().count(), line.chars().count());
        assert_eq!(prose.find("teh"), line.find("teh"));
    }

    /// A blank is not a word boundary that was not there before, and the words
    /// either side of it stay two words.
    #[test]
    fn blanking_a_token_leaves_the_words_around_it_alone() {
        assert_eq!(blanked("a src/x.rs b"), "a          b");
    }

    /// Code the user marked as code, however ordinary the words inside it.
    #[test]
    fn anything_inside_backticks_is_code() {
        let line = "run `the cat sat` now";

        let prose = blanked(line);

        assert_eq!(prose.chars().count(), line.chars().count());
        assert!(!prose.contains("cat"), "{prose:?}");
        assert!(
            prose.starts_with("run ") && prose.ends_with(" now"),
            "{prose:?}"
        );
    }

    /// Including a pair that fits inside one token, which is how a name gets
    /// quoted far more often than a phrase does.
    #[test]
    fn a_backticked_word_on_its_own_is_code_too() {
        let line = "run `teh` now";

        let prose = blanked(line);

        assert_eq!(prose.chars().count(), line.chars().count());
        assert!(!prose.contains("teh"), "{prose:?}");
        assert!(
            prose.starts_with("run ") && prose.ends_with(" now"),
            "{prose:?}"
        );
    }

    /// A backtick nobody closed reads as code to the end, which is how it
    /// looks on the screen as well.
    #[test]
    fn an_unclosed_backtick_runs_to_the_end_of_the_line() {
        let prose = blanked("say `the rest");

        assert!(!prose.contains("rest"), "{prose:?}");
        assert!(prose.starts_with("say "), "{prose:?}");
    }

    /// A misspelling standing next to a path is still a misspelling.
    #[test]
    fn a_word_beside_code_is_still_a_word() {
        assert_eq!(blanked("teh src/pty.rs teh"), "teh            teh");
    }
}
