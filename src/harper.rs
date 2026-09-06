//! The engine behind the checker the painter holds. Named nowhere else.

use std::path::Path;
use std::sync::Arc;

use harper_core::linting::{LintGroup, Suggestion};
use harper_core::spell::{FstDictionary, MergedDictionary, MutableDictionary};
use harper_core::{Dialect, DictWordMetadata, Document};

use crate::checker::{Checker, Class, Edit, Flag};

/// The curated grammar classes and the harper rules each one turns on. Harper
/// ships 828 curated rules; everything outside this table stays off, so
/// the overlay only ever speaks about the slips it was asked to watch for.
const CLASSES: &[(&str, &[&str])] = &[
    // The wrong indefinite article, which the finger picks before the word is.
    ("articles", &["AnA"]),
    // `its` for `it's` and back, the apostrophe rule that inverts between them.
    ("its-its", &["ItsContraction", "ItsPossessive"]),
    // `lets` for `let's`, the same inversion on another word.
    ("lets-lets", &["LetsConfusion"]),
    // A word typed twice across a line break or a pause — the slip a fast
    // typist makes most and reads over every time.
    ("repeated", &["RepeatedWords"]),
    // Two spaces where one was meant. Its subject is the gap between words,
    // which is the one thing the prose filter must never leave behind where it
    // takes a path out — and does not: the hole is a word. Measured on prompt
    // lines full of paths and flags: nothing at all, and a doubled space the
    // user typed still caught.
    ("spacing", &["Spaces", "NoFrenchSpaces"]),
    // `too` where the infinitive marker was meant.
    ("to-too", &["ToTwoToo"]),
    // `then` for a comparison, which is `than`.
    ("then-than", &["ThenThan"]),
];

// Deliberately out, and why. `LongSentences`, `BoringWords`, `FillerWords`,
// `Hedging`, `AvoidContractions` and `AvoidCurses` police how a person writes
// rather than catch what they mistyped, and this is pointed at a prompt, not an
// essay. `CapitalizePersonalPronouns` was tried and dropped: a bare `i` is a
// loop variable in six of fourteen prompts about code, and being a grammar flag
// it cannot be learned away — the wrapper would be arguing with a variable name
// all day to catch a lowercase pronoun. `TheyreConfusions`, `PossessiveYour`,
// `Theres` and `ThatThan` were tried and dropped for the opposite reason: none
// of them fired on the slip it is named for, and a class whose rule never
// speaks is a line of config that does nothing.
//
// Harper 2.8.0 has no single `Matcher` rule to turn on: its curated group holds
// 828 separately named rules, most of them one phrase each, so a `phrases`
// class would be a list nobody could keep curated — the confusions worth having
// are named above.

/// Harper's name for the spell checker. It is always on: spelling is why the
/// overlay exists, and it is the one check that is not a curated class.
const SPELL_CHECK: &str = "SpellCheck";

/// Every grammar class there is, which is what the defaults turn on: a class is
/// added here to be watched, and the config's job is to silence one, not to
/// discover it.
pub fn classes() -> impl Iterator<Item = &'static str> {
    CLASSES.iter().map(|(class, _)| *class)
}

pub struct HarperChecker {
    dictionary: Arc<MergedDictionary>,
    rules: LintGroup,
}

impl HarperChecker {
    /// `grammar` names the curated classes to enable; a name that is not a class
    /// enables nothing, and the config says so on its way past. `dict_path` is
    /// the user's own word list, if they have one.
    pub fn new(grammar: &[String], dict_path: Option<&Path>) -> Self {
        let mut dictionary = MergedDictionary::new();
        dictionary.add_dictionary(FstDictionary::curated());
        if let Some(path) = dict_path {
            dictionary.add_dictionary(Arc::new(custom_words(path)));
        }
        let dictionary = Arc::new(dictionary);

        let mut rules = LintGroup::new_curated(dictionary.clone(), Dialect::American);
        rules.set_all_rules_to(Some(false));
        rules.config.set_rule_enabled(SPELL_CHECK, true);
        for (class, class_rules) in CLASSES {
            if grammar.iter().any(|wanted| wanted == class) {
                for rule in *class_rules {
                    rules.config.set_rule_enabled(rule, true);
                }
            }
        }

        Self { dictionary, rules }
    }
}

impl Checker for HarperChecker {
    fn check(&mut self, line: &str) -> Vec<Flag> {
        let document = Document::new_plain_english(line, &*self.dictionary);

        let mut flags: Vec<Flag> = self
            .rules
            .organized_lints(&document)
            .into_iter()
            .filter(|(rule, _)| is_curated(rule))
            .flat_map(|(rule, lints)| {
                // The engine already knows which of its rules spoke, so the
                // class is read off the rule rather than guessed at from the
                // words the span covers.
                let class = match rule == SPELL_CHECK {
                    true => Class::Spelling,
                    false => Class::Grammar,
                };
                lints.into_iter().map(move |lint| Flag {
                    span: lint.span.start..lint.span.end,
                    class,
                    suggestions: lint.suggestions.iter().map(edit_of).collect(),
                })
            })
            .collect();

        flags.sort_by_key(|flag| (flag.span.start, flag.span.end));
        flags
    }
}

/// Whether a rule is one this wrapper speaks about at all: spelling, or a rule
/// in one of the curated classes. A rule this table does not name is dropped
/// rather than shown, so a harper release that turns something on by itself
/// cannot start underlining words the user never asked about.
fn is_curated(rule: &str) -> bool {
    rule == SPELL_CHECK
        || CLASSES
            .iter()
            .any(|(_, class_rules)| class_rules.contains(&rule))
}

/// All three shapes a suggestion comes in. Read as a bare word an insertion or
/// a deletion means nothing, so each says what it does to the flagged stretch
/// and the picker does it.
fn edit_of(suggestion: &Suggestion) -> Edit {
    match suggestion {
        Suggestion::ReplaceWith(characters) => Edit::Replace(characters.iter().collect()),
        Suggestion::InsertAfter(characters) => Edit::InsertAfter(characters.iter().collect()),
        Suggestion::Remove => Edit::Remove,
    }
}

/// The user's own words, one per line; blank lines and `#` comments are skipped
/// so the file can be annotated. A list that is not there yet is not a list the
/// user got wrong — every session before the first learned word would otherwise
/// open with a complaint. One that cannot be read for any other reason costs a
/// line on stderr, since refusing to start over a word list would be worse than
/// spell-checking without it.
fn custom_words(path: &Path) -> MutableDictionary {
    let mut dictionary = MutableDictionary::new();

    let list = match std::fs::read_to_string(path) {
        Ok(list) => list,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("blackjack: {}: {error}", path.display());
            }
            return dictionary;
        }
    };

    for word in list
        .lines()
        .map(str::trim)
        .filter(|word| !word.is_empty() && !word.starts_with('#'))
    {
        dictionary.append_word_str(word, DictWordMetadata::default());
    }

    dictionary
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use harper_core::spell::Dictionary;

    use super::*;

    /// Every class there is, so a class added to the table is a class these
    /// tests watch without anyone remembering to add it here too.
    fn every_class() -> Vec<String> {
        super::classes().map(str::to_string).collect()
    }

    fn checker() -> HarperChecker {
        HarperChecker::new(&every_class(), None)
    }

    /// A word list written where the test can reach it, named after the test so
    /// two running at once cannot collide.
    fn word_list(name: &str, contents: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("blackjack-{name}-{}.txt", std::process::id()));
        fs::write(&path, contents).unwrap();
        path
    }

    /// Every shape a suggestion comes in says what it does to the flagged
    /// stretch. The curated rules only ever ask for a replacement today, so
    /// nothing else here can reach the other two — they are the engine's
    /// vocabulary, not this table's guess at it.
    #[test]
    fn every_shape_of_suggestion_says_what_it_does() {
        assert_eq!(
            edit_of(&Suggestion::ReplaceWith("the".chars().collect())),
            Edit::Replace("the".into())
        );
        assert_eq!(
            edit_of(&Suggestion::InsertAfter("'s".chars().collect())),
            Edit::InsertAfter("'s".into())
        );
        assert_eq!(edit_of(&Suggestion::Remove), Edit::Remove);
    }

    #[test]
    fn a_misspelling_is_flagged_with_the_word_that_was_meant() {
        let flags = checker().check("teh cat sat");

        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].span, 0..3);
        assert!(
            flags[0].suggestions.contains(&Edit::Replace("the".into())),
            "{:?}",
            flags[0].suggestions
        );
    }

    #[test]
    fn a_possessive_its_standing_in_for_it_is_is_flagged() {
        let flags = checker().check("its a test");

        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].span, 0..3);
        assert!(flags[0].suggestions.contains(&Edit::Replace("it's".into())));
    }

    #[test]
    fn the_wrong_indefinite_article_is_flagged() {
        let flags = checker().check("This is an mule.");

        assert_eq!(flags.len(), 1);
        assert!(flags[0].suggestions.contains(&Edit::Replace("a".into())));
    }

    #[test]
    fn lets_standing_in_for_let_us_is_flagged() {
        let flags = checker().check("Lets try that again.");

        assert_eq!(flags.len(), 1);
        assert!(
            flags[0]
                .suggestions
                .contains(&Edit::Replace("Let's".into())),
            "{:?}",
            flags[0].suggestions
        );
    }

    /// Which kind of slip a flag is comes from the rule that spoke, not from a
    /// guess at the words it covers. The learn key is the reader: a word can be
    /// taught to a dictionary and a grammar slip cannot.
    #[test]
    fn a_flag_carries_the_kind_of_slip_it_is() {
        assert_eq!(checker().check("teh cat sat")[0].class, Class::Spelling);
        assert_eq!(checker().check("This is an mule.")[0].class, Class::Grammar);
    }

    /// The slips the wider table was added for, each flagged by the class that
    /// names it. A class whose rule never fires on the slip it is for is a line
    /// of config that does nothing.
    #[test]
    fn every_class_flags_the_slip_it_was_added_for() {
        for (line, meant) in [
            ("the the cat sat", "the"),
            ("Its a test", "It's"),
            ("a  double space", " "),
            ("i went too the store", "to"),
            ("its bigger then i thought", "than"),
            ("This is an mule.", "a"),
            ("Lets try that again.", "Let's"),
        ] {
            let flags = checker().check(line);
            assert!(
                flags
                    .iter()
                    .any(|flag| flag.suggestions.contains(&Edit::Replace(meant.into()))),
                "{line:?} was not offered {meant:?}: {flags:?}"
            );
        }
    }

    /// A doubled word is one flag over two words, which is the first span the
    /// curated table produces that covers more than one — the fix has to take
    /// out both and put one back.
    #[test]
    fn a_doubled_word_is_one_flag_over_both_of_them() {
        let flags = checker().check("the the cat sat");

        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].span, 0..7);
        assert_eq!(flags[0].class, Class::Grammar);
    }

    #[test]
    fn nothing_outside_the_curated_classes_is_flagged() {
        let mut checker = checker();

        assert!(checker.check("The cat sat on the mat.").is_empty());
        assert!(checker.check("").is_empty());
        // Harper's own rule set calls `very unique` a style slip. Style is not
        // one of the classes blackjack was asked to watch.
        assert!(checker.check("This is very unique.").is_empty());
    }

    #[test]
    fn spans_are_char_indices_not_bytes() {
        let flags = checker().check("中文 teh");

        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].span, 3..6);
    }

    #[test]
    fn a_class_left_out_of_the_config_stays_quiet() {
        let mut only_spelling = HarperChecker::new(&[], None);

        assert!(only_spelling.check("its a test").is_empty());
        assert!(only_spelling.check("This is an mule.").is_empty());
        assert_eq!(only_spelling.check("teh cat").len(), 1);
    }

    #[test]
    fn a_word_from_the_users_own_list_is_not_a_misspelling() {
        let path = word_list("dict", "undercurl\nDECSTBM\n");

        assert_eq!(checker().check("undercurl the DECSTBM").len(), 2);
        assert_eq!(
            HarperChecker::new(&every_class(), Some(&path))
                .check("undercurl the DECSTBM")
                .len(),
            0
        );

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_word_list_is_one_word_a_line_with_room_for_notes() {
        let path = word_list("format", "# jargon\n\n  undercurl  \nDECSTBM\n");

        let words = custom_words(&path);

        assert_eq!(words.word_count(), 2);
        assert!(words.contains_exact_word_str("undercurl"));
        assert!(words.contains_exact_word_str("DECSTBM"));

        fs::remove_file(&path).unwrap();
    }

    /// Nothing is read across a token the filter took out: not the article
    /// beside it, not a word repeated either side of it, not a run of spaces
    /// where it stood. Every line here was proven against a hole filled with
    /// whitespace, where each of them flags — which is the whole reason a hole
    /// is filled with something else.
    #[test]
    fn nothing_is_read_across_a_token_the_filter_took_out() {
        let mut checker = checker();

        for line in [
            "add a --json option to the CLI",
            "i want an --offline flag for the checker",
            "open src/main.rs and add an --exit-code flag",
            "the --flag the --flag versions",
            "compare the src/a.rs the src/b.rs",
            "read a `note` and an `entry`",
        ] {
            let asked = crate::prose::filtered(line);
            let flags = checker.check(&asked);

            assert!(flags.is_empty(), "{line:?} asked as {asked:?}: {flags:?}");
        }
    }

    #[test]
    fn a_word_list_that_cannot_be_read_is_not_fatal() {
        let missing = std::env::temp_dir().join("blackjack-no-such-word-list.txt");

        let flags = HarperChecker::new(&every_class(), Some(&missing)).check("teh cat");

        assert_eq!(flags.len(), 1);
    }
}
