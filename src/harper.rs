//! The engine behind the checker the painter holds. Named nowhere else.

use std::path::Path;
use std::sync::Arc;

use harper_core::linting::{LintGroup, Suggestion};
use harper_core::spell::{FstDictionary, MergedDictionary, MutableDictionary};
use harper_core::{Dialect, DictWordMetadata, Document};

use crate::checker::{Checker, Edit, Flag};

/// The curated grammar classes and the harper rules each one turns on. Harper
/// ships some seven hundred rules; everything outside this table stays off, so
/// the overlay only ever speaks about the slips it was asked to watch for.
const CLASSES: &[(&str, &[&str])] = &[
    ("articles", &["AnA"]),
    ("its-its", &["ItsContraction", "ItsPossessive"]),
    ("lets-lets", &["LetsConfusion"]),
];

/// Harper's name for the spell checker. It is always on: spelling is why the
/// overlay exists, and it is the one check that is not a curated class.
const SPELL_CHECK: &str = "SpellCheck";

pub struct HarperChecker {
    dictionary: Arc<MergedDictionary>,
    rules: LintGroup,
}

impl HarperChecker {
    /// `grammar` names the curated classes to enable; a name that is not a class
    /// is ignored. `dict_path` is the user's own word list, if they have one.
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
            .flat_map(|(_, lints)| {
                lints.into_iter().map(|lint| Flag {
                    span: lint.span.start..lint.span.end,
                    message: lint.message,
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
/// so the file can be annotated. A word list that cannot be read costs one line
/// on stderr — refusing to start over a missing word list would be worse than
/// spell-checking without it.
fn custom_words(path: &Path) -> MutableDictionary {
    let mut dictionary = MutableDictionary::new();

    let list = match std::fs::read_to_string(path) {
        Ok(list) => list,
        Err(error) => {
            eprintln!("blackjack: {}: {error}", path.display());
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

    const EVERY_CLASS: [&str; 3] = ["articles", "its-its", "lets-lets"];

    fn classes(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn checker() -> HarperChecker {
        HarperChecker::new(&classes(&EVERY_CLASS), None)
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
            HarperChecker::new(&classes(&EVERY_CLASS), Some(&path))
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

    #[test]
    fn a_word_list_that_cannot_be_read_is_not_fatal() {
        let missing = std::env::temp_dir().join("blackjack-no-such-word-list.txt");

        let flags = HarperChecker::new(&classes(&EVERY_CLASS), Some(&missing)).check("teh cat");

        assert_eq!(flags.len(), 1);
    }
}
