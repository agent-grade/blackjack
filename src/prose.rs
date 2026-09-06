//! Which of what the user typed is prose at all. The wrapper is pointed at a
//! terminal, where a line is as likely to be a path, a flag or an identifier as
//! it is a sentence, and a checker asked about those has nothing useful to say
//! about any of them. So they are taken out of the line before it is asked —
//! filled in rather than removed, character for character, so that everything
//! said about what is left still counts from the same place, and the hole is
//! shaped like a word so that nothing is read across it either.

/// The characters that make a token something other than a word: a path, a
/// namespace, a host, a handle, a home directory, a version.
const NOT_A_WORD: [char; 7] = ['/', '\\', ':', '#', '@', '~', '_'];

/// What a token taken out is filled with, one character of it for each of the
/// token's own. The choice is measured, not chosen: a run of digits is a word
/// harper has no article, repetition or spacing opinion about, so a line asked
/// about with one in it comes back with nothing to say about the hole. Every
/// letter tried answers the article rule in one direction or the other — `an
/// xxxx` asks for `a` — and every punctuation run tried is skipped by the
/// tokenizer, which is what left the rule reading past the hole to the next
/// real word.
const FILLER: char = '0';

/// The line as the checker is asked about it, everything that is not prose
/// filled in. Same length, and every character that is still there is where it
/// was, so a span into this is a span into what the user typed.
pub fn filtered(line: &str) -> String {
    let mut prose = String::with_capacity(line.len());
    for (token, code) in classified(line) {
        match code {
            true => prose.extend(token.chars().map(hole)),
            false => prose.push_str(token),
        }
    }
    prose
}

/// Whether a flag is about the line the user typed, or about the hole the
/// filter left in it: a flag reaching across a character that was taken out is
/// this filter's own doing and is declined before anyone sees it. That is the
/// whole of the job — a rule reading *past* the hole to the next real word is
/// what the filler retires, by leaving a word there to read instead.
///
/// What was taken out is asked of the filter rather than read back off the
/// filled line, which would only ever be as good as the filler being unable to
/// spell what it replaced: `0` replacing `0` is a hole that looks untouched.
pub fn intact(line: &str, span: &std::ops::Range<usize>) -> bool {
    !taken_out(line)
        .skip(span.start)
        .take(span.end.saturating_sub(span.start))
        .any(|gone| gone)
}

/// Character for character, whether the filter took each one out. Whitespace is
/// left where it was even inside a token that goes, so it is not one of them.
fn taken_out(line: &str) -> impl Iterator<Item = bool> + '_ {
    classified(line).flat_map(|(token, code)| {
        token
            .chars()
            .map(move |character| code && !character.is_whitespace())
    })
}

/// The tokens the filter took out — the answer to "why was that not checked",
/// which is the question a filter this quiet will be asked.
pub fn removed(line: &str) -> Vec<&str> {
    classified(line)
        .filter(|(token, code)| *code && !token.trim().is_empty())
        .map(|(token, _)| token.trim_end())
        .collect()
}

/// Each token of the line and whether it is code, decided once for both readers
/// above — a filter that took a token out and a filter that says it did must
/// not be able to disagree.
fn classified(line: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut quoted = false;
    line.split_inclusive(char::is_whitespace).map(move |token| {
        let word = token.trim_end();
        let ticks = word.matches('`').count();
        // Code the user marked as code. The run stays code until its closing
        // tick, and an unclosed one takes the rest of the line with it — which is
        // what it looks like on the screen too.
        let inside = quoted;
        quoted ^= ticks % 2 == 1;
        (token, inside || quoted || is_code(word))
    })
}

/// Whitespace is what tokens are counted by, so it is the one thing a hole must
/// not be filled with: the words either side of it would close up into one.
fn hole(character: char) -> char {
    match character.is_whitespace() {
        true => character,
        false => FILLER,
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

    /// Filled in, not removed: everything left is where it was, so a span into
    /// what the checker reads is a span into what the user typed.
    #[test]
    fn what_is_left_is_where_it_was() {
        let line = "open src/pty.rs and teh file";

        let prose = filtered(line);

        assert_eq!(prose, "open 0000000000 and teh file");
        assert_eq!(prose.chars().count(), line.chars().count());
        assert_eq!(prose.find("teh"), line.find("teh"));
    }

    /// The hole is a word of its own: it is not a boundary that was not there
    /// before, and it is not nothing either, so the words either side of it are
    /// never read as neighbours.
    #[test]
    fn a_hole_stands_between_the_words_around_it() {
        assert_eq!(filtered("a src/x.rs b"), "a 00000000 b");
    }

    /// Code the user marked as code, however ordinary the words inside it.
    #[test]
    fn anything_inside_backticks_is_code() {
        let line = "run `the cat sat` now";

        let prose = filtered(line);

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

        let prose = filtered(line);

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
        let prose = filtered("say `the rest");

        assert!(!prose.contains("rest"), "{prose:?}");
        assert!(prose.starts_with("say "), "{prose:?}");
    }

    /// What was taken out, which is the answer to "why was that not checked".
    #[test]
    fn the_filter_can_say_what_it_took_out() {
        let line = "open src/pty.rs and fix teh `bug` now";

        assert_eq!(removed(line), ["src/pty.rs", "`bug`"]);
    }

    /// A flag reaching across a removed token is the filter's own doing and not
    /// the user's, whatever the checker made of the hole in between.
    #[test]
    fn a_flag_across_something_removed_is_not_the_users() {
        let line = "compare the src/a.rs the src/b.rs";

        assert!(!intact(line, &(8..24)), "the doubled `the`");
        assert!(intact(line, &(8..11)), "one word of it");
        assert!(intact(line, &(0..7)), "a word standing on its own");
    }

    /// And a token that fills to something identical to itself is still a hole:
    /// what was taken out is the filter's own answer, not a difference read back
    /// off the line it filled.
    #[test]
    fn a_token_that_fills_to_itself_is_still_a_hole() {
        let line = "the 0 the";

        assert_eq!(filtered(line), line);
        assert!(!intact(line, &(0..9)), "the doubled `the`");
        assert!(intact(line, &(0..3)), "a word standing on its own");
    }

    /// A misspelling standing next to a path is still a misspelling.
    #[test]
    fn a_word_beside_code_is_still_a_word() {
        assert_eq!(filtered("teh src/pty.rs teh"), "teh 0000000000 teh");
    }

    /// Nothing of the filter's own is ever a character the user typed, whatever
    /// the token was made of: the hole is the same length in characters and
    /// carries none of them.
    #[test]
    fn the_hole_keeps_nothing_of_what_it_replaced() {
        let line = "see std::mem::take and ~/notes";

        let prose = filtered(line);

        assert_eq!(prose.chars().count(), line.chars().count());
        assert_eq!(prose, "see 00000000000000 and 0000000");
        assert!(!prose.contains("mem"), "{prose:?}");
    }
}
