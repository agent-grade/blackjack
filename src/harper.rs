//! The engine behind the checker the painter holds. Named nowhere else.

use std::path::Path;
use std::sync::Arc;

use harper_core::linting::{LintGroup, Suggestion};
use harper_core::spell::{FstDictionary, MergedDictionary, MutableDictionary};
use harper_core::{Dialect, DictWordMetadata, Document};

use crate::checker::{Checker, Flag, Kind};

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
            .filter_map(|(rule, lints)| kind_of(&rule).map(|kind| (kind, lints)))
            .flat_map(|(kind, lints)| {
                lints.into_iter().map(move |lint| Flag {
                    span: lint.span.start..lint.span.end,
                    kind,
                    message: lint.message,
                    suggestions: lint.suggestions.iter().filter_map(replacement).collect(),
                })
            })
            .collect();

        flags.sort_by_key(|flag| (flag.span.start, flag.span.end));
        flags
    }
}

/// Which class a harper rule reports under. A rule this table does not name is
/// dropped rather than shown, so a harper release that turns something on by
/// itself cannot start underlining words the user never asked about.
fn kind_of(rule: &str) -> Option<Kind> {
    if rule == SPELL_CHECK {
        return Some(Kind::Spelling);
    }
    CLASSES
        .iter()
        .find(|(_, class_rules)| class_rules.contains(&rule))
        .map(|(class, _)| Kind::Grammar(class))
}

/// Only a straight replacement survives: it is the one suggestion shape that
/// means anything as a bare word. Insertions and deletions need the span beside
/// them to read, and nothing displays suggestions until the picker lands.
fn replacement(suggestion: &Suggestion) -> Option<String> {
    match suggestion {
        Suggestion::ReplaceWith(characters) => Some(characters.iter().collect()),
        Suggestion::InsertAfter(_) | Suggestion::Remove => None,
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

    #[test]
    fn a_misspelling_is_flagged_with_the_word_that_was_meant() {
        let flags = checker().check("teh cat sat");

        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].kind, Kind::Spelling);
        assert_eq!(flags[0].span, 0..3);
        assert!(
            flags[0].suggestions.iter().any(|word| word == "the"),
            "{:?}",
            flags[0].suggestions
        );
    }

    #[test]
    fn a_possessive_its_standing_in_for_it_is_is_flagged() {
        let flags = checker().check("its a test");

        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].kind, Kind::Grammar("its-its"));
        assert_eq!(flags[0].span, 0..3);
    }

    #[test]
    fn the_wrong_indefinite_article_is_flagged() {
        let flags = checker().check("This is an mule.");

        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].kind, Kind::Grammar("articles"));
        assert!(flags[0].suggestions.iter().any(|word| word == "a"));
    }

    #[test]
    fn lets_standing_in_for_let_us_is_flagged() {
        let flags = checker().check("Lets try that again.");

        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].kind, Kind::Grammar("lets-lets"));
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
