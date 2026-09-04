//! Nothing in the wrapper calls a checker yet; painting is what will.
#![allow(dead_code)]

use std::ops::Range;

/// What a flag is about. `Grammar` carries the curated class that raised it, so
/// a painter can style an article slip differently from a misspelling and a
/// later picker can say which rule spoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Spelling,
    Grammar(&'static str),
}

/// One stretch of the line worth underlining. `span` counts **characters**, not
/// bytes: the painter works in columns, and a byte offset is already wrong by
/// the time a single accented character precedes the word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flag {
    pub span: Range<usize>,
    pub kind: Kind,
    pub message: String,
    pub suggestions: Vec<String>,
}

/// Everything the wrapper knows about a checking engine. Harper is the first
/// implementation behind it and never the interface itself.
pub trait Checker {
    fn check(&mut self, line: &str) -> Vec<Flag>;
}

/// A checker that flags exactly the words it was handed, for tests that need a
/// known answer rather than a real engine's.
#[cfg(test)]
pub struct FakeChecker {
    flagged: Vec<String>,
    kind: Kind,
}

#[cfg(test)]
impl FakeChecker {
    pub fn spelling(flagged: &[&str]) -> Self {
        Self::new(flagged, Kind::Spelling)
    }

    pub fn grammar(class: &'static str, flagged: &[&str]) -> Self {
        Self::new(flagged, Kind::Grammar(class))
    }

    fn new(flagged: &[&str], kind: Kind) -> Self {
        Self {
            flagged: flagged.iter().map(|word| (*word).to_string()).collect(),
            kind,
        }
    }
}

#[cfg(test)]
impl Checker for FakeChecker {
    fn check(&mut self, line: &str) -> Vec<Flag> {
        words(line)
            .into_iter()
            .filter(|(_, word)| self.flagged.iter().any(|flagged| flagged == word))
            .map(|(span, word)| Flag {
                span,
                kind: self.kind,
                message: format!("`{word}` is flagged"),
                suggestions: Vec::new(),
            })
            .collect()
    }
}

/// The line's words, each with the character range it occupies. An apostrophe
/// holds a word together so that `it's` is one word rather than two.
#[cfg(test)]
fn words(line: &str) -> Vec<(Range<usize>, String)> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut start = 0;
    let mut end = 0;

    for (at, character) in line.chars().enumerate() {
        end = at + 1;
        if character.is_alphanumeric() || character == '\'' {
            if word.is_empty() {
                start = at;
            }
            word.push(character);
        } else if !word.is_empty() {
            words.push((start..at, std::mem::take(&mut word)));
        }
    }
    if !word.is_empty() {
        words.push((start..end, word));
    }

    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_are_char_indices_not_bytes() {
        let mut checker = FakeChecker::spelling(&["teh"]);

        let flags = checker.check("中文 teh");

        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].span, 3..6);
    }

    #[test]
    fn an_empty_line_yields_nothing() {
        let mut checker = FakeChecker::spelling(&["teh"]);

        assert!(words("").is_empty());
        assert!(words("   ").is_empty());
        assert!(checker.check("").is_empty());
        assert!(checker.check("   ").is_empty());
    }

    #[test]
    fn only_the_words_it_was_given_are_flagged() {
        let mut checker = FakeChecker::spelling(&["teh", "hte"]);

        let flags = checker.check("teh cat sat on hte mat");

        assert_eq!(flags.len(), 2);
        assert_eq!(flags[0].span, 0..3);
        assert_eq!(flags[1].span, 15..18);
    }

    #[test]
    fn a_word_ending_the_line_is_flagged() {
        let mut checker = FakeChecker::spelling(&["teh"]);

        let flags = checker.check("and teh");

        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].span, 4..7);
    }

    #[test]
    fn punctuation_ends_a_word_but_an_apostrophe_does_not() {
        let mut checker = FakeChecker::spelling(&["teh", "it's"]);

        let flags = checker.check("teh, it's teh.");

        assert_eq!(flags.len(), 3);
        assert_eq!(flags[0].span, 0..3);
        assert_eq!(flags[1].span, 5..9);
        assert_eq!(flags[2].span, 10..13);
    }

    #[test]
    fn a_grammar_flag_carries_the_class_that_raised_it() {
        let mut checker = FakeChecker::grammar("its-its", &["its"]);

        let flags = checker.check("its a test");

        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].kind, Kind::Grammar("its-its"));
    }
}
