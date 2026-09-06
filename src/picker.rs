//! The two answers to a flagged word, without the user leaving the keyboard:
//! taking one of its suggestions, or teaching the wrapper that the word was
//! right all along. No menu is drawn for either — pressing the fix key again
//! is how the next suggestion is asked for, and pressing anything else is how
//! the offer is declined. A learned word is kept for the session and written
//! to the user's list for every session after it.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::checker::{Class, Edit, Flag};
use crate::input::{ALT, CTRL, SHIFT, SUPER};

/// The key that fixes the word under the cursor, as the user writes it in the
/// config: the modifiers held down, joined by `+`, and then the character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    modifiers: u16,
    character: char,
}

/// Written back the way the config writes it, so the log can say which key is
/// actually live rather than which one the file asked for.
impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (bit, name) in [
            (SHIFT, "shift"),
            (ALT, "alt"),
            (CTRL, "ctrl"),
            (SUPER, "cmd"),
        ] {
            if self.modifiers & bit != 0 {
                write!(f, "{name}+")?;
            }
        }
        write!(f, "{}", self.character)
    }
}

impl Chord {
    /// `ctrl+.`, `cmd+.`, `alt+shift+x`. The key is the last character written, which
    /// is what lets `+` itself be the key; whatever comes before it names the
    /// modifiers, and there has to be one — a key with none would swallow that
    /// character every time the user typed it. An unknown modifier, a missing
    /// one, or an empty key leaves this `None`, and the caller keeps its
    /// default: a config typo must not cost the user their wrapper.
    pub fn parse(written: &str) -> Option<Self> {
        let written: String = written.chars().filter(|c| !c.is_whitespace()).collect();
        let character = written.chars().next_back()?;
        let named = written[..written.len() - character.len_utf8()].trim_end_matches('+');
        let mut modifiers = 0;
        for name in named.split('+').filter(|name| !name.is_empty()) {
            modifiers |= match name.to_ascii_lowercase().as_str() {
                "shift" => SHIFT,
                "alt" | "opt" | "option" | "meta" => ALT,
                "ctrl" | "control" => CTRL,
                "cmd" | "command" | "super" | "win" => SUPER,
                _ => return None,
            };
        }
        (modifiers != 0).then_some(Self {
            modifiers,
            character,
        })
    }

    /// Whether a keystroke is this chord — the whole of it, because a chord
    /// arrives as its own keystroke and anything glued to it is a paste or a
    /// burst that cannot be claimed.
    ///
    /// Two encodings answer to it. The kitty protocol's says which modifiers
    /// were held and is matched exactly. The escape-prefixed form is all a
    /// terminal without that protocol can offer — it says only that *some*
    /// meta key was held, so it is taken at that: whatever modifiers the user
    /// configured, on such a terminal the fix key is their character with alt.
    /// A chord like `ctrl+.` has no character of its own in ASCII, so without
    /// this there would be nothing to press at all.
    ///
    /// `cmd` is the one modifier no terminal sends of its own accord — it
    /// belongs to the application — so a chord holding it answers to the kitty
    /// form only, from a terminal the user has bound to send it.
    pub fn matches(&self, keys: &[u8]) -> bool {
        let kitty = format!("\x1b[{};{}u", self.character as u32, self.modifiers + 1);
        if keys == kitty.as_bytes() {
            return true;
        }
        let mut legacy = [0u8; 5];
        legacy[0] = 0x1b;
        let escaped = self.character.encode_utf8(&mut legacy[1..]).len() + 1;
        keys == &legacy[..escaped]
    }
}

/// What the checker last said, and about which line. The picker is on the
/// keystroke side and the checking happens on the output side, so the answer
/// travels; the line it was about travels with it, because a line that has
/// moved on since makes every span in it a guess.
#[derive(Debug, Default)]
pub struct Checked {
    pub line: String,
    pub flags: Vec<Flag>,
}

pub type SharedChecked = Arc<Mutex<Checked>>;

/// The words the user has told the wrapper to stop objecting to. They are held
/// here rather than given to the engine because an engine's dictionary is built
/// once, and the whole point is that a word learned now stops being underlined
/// now.
pub type SharedLearned = Arc<Mutex<BTreeSet<String>>>;

/// A fix in progress. The stretch of the line between `at` and `at + standing`
/// is what the last press left behind; pressing again takes it out and puts
/// the next suggestion in its place.
#[derive(Debug)]
struct Cycling {
    at: usize,
    original: String,
    offers: Vec<String>,
    /// Which offer is standing, counted so that the last one is followed by
    /// the original word again.
    offered: usize,
    standing: String,
}

pub struct Picker {
    chord: Chord,
    learn: Chord,
    checked: SharedChecked,
    learned: SharedLearned,
    /// Where a learned word is written down, so that it is still learned
    /// tomorrow. Nowhere, if the user has no home to put it in.
    words: Option<PathBuf>,
    cycling: Option<Cycling>,
}

impl Picker {
    pub fn new(
        chord: Chord,
        learn: Chord,
        checked: SharedChecked,
        learned: SharedLearned,
        words: Option<PathBuf>,
    ) -> Self {
        Self {
            chord,
            learn,
            checked,
            learned,
            words,
            cycling: None,
        }
    }

    /// The word under the cursor, taken as one the user meant. It is remembered
    /// for this session and written down for the next; the underline goes at
    /// the next paint, since what is flagged is filtered through what has been
    /// learned rather than asked about again.
    pub fn learn(&mut self, line: &str, cursor: usize) -> Option<String> {
        let (flag, word) = self.claimed(line, cursor)?;
        // A grammar slip is not a word, so there is nothing here to teach a
        // dictionary and the keystroke belongs to the wrapped program.
        if flag.class != Class::Spelling || word.is_empty() {
            return None;
        }
        self.learned.lock().ok()?.insert(word.clone());
        self.write_down(&word);
        Some(word)
    }

    /// The flag under the cursor and the word it stands over — the question
    /// both keys start from, so they cannot disagree about what is flagged.
    /// A word already learned is not offered to either: its underline is gone,
    /// and a key that acts on something the screen no longer shows is a
    /// surprise.
    fn claimed(&self, line: &str, cursor: usize) -> Option<(Flag, String)> {
        let checked = self.checked.lock().ok()?;
        if checked.line != line {
            return None;
        }
        let characters: Vec<char> = line.chars().collect();
        let learned = self.learned.lock().ok()?;
        checked
            .flags
            .iter()
            .filter(|flag| covers(flag, cursor))
            .find_map(|flag| {
                let word: String = characters.get(flag.span.clone())?.iter().collect();
                (!learned.contains(&word)).then(|| (flag.clone(), word))
            })
    }

    /// One word per line, appended. A list that cannot be written is a list the
    /// user loses tomorrow, which is worth a line on stderr and nothing more —
    /// the word is learned for this session either way.
    fn write_down(&self, word: &str) {
        let Some(path) = &self.words else {
            return;
        };
        if let Some(directory) = path.parent() {
            let _ = std::fs::create_dir_all(directory);
        }
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| writeln!(file, "{word}"));
        if let Err(error) = written {
            eprintln!("blackjack: {}: {error}", path.display());
        }
    }

    pub fn is_learn(&self, keys: &[u8]) -> bool {
        self.learn.matches(keys)
    }

    /// The keystrokes to send in place of the chord, given the line the model
    /// holds and where its cursor is. `None` means the chord was not this one,
    /// or there is nothing under the cursor to fix — either way the keystroke
    /// is the user's and goes through untouched.
    pub fn fix(&mut self, line: &str, cursor: usize) -> Option<Vec<u8>> {
        let cycling = match self.cycling.take() {
            // A cycle already running, with the cursor still where the last
            // offer left it: this press is about the same word. The flags are
            // not consulted again — they describe the line before the first
            // offer went in, and it is this cycle that changed it.
            Some(cycling) if cursor == cycling.at + cycling.standing.chars().count() => cycling,
            _ => self.start(line, cursor)?,
        };
        Some(self.next_offer(cycling, cursor))
    }

    /// Any other keystroke ends the cycle: the word has been left as the user
    /// wanted it, or they have moved on, and either way the next press of the
    /// chord is a fresh question.
    pub fn interrupted(&mut self) {
        self.cycling = None;
    }

    pub fn is_chord(&self, keys: &[u8]) -> bool {
        self.chord.matches(keys)
    }

    fn start(&mut self, line: &str, cursor: usize) -> Option<Cycling> {
        let (flag, _) = self.claimed(line, cursor)?;
        cycling_over(line, &flag)
    }

    /// The keystrokes that take out what is standing and put the next offer in
    /// its place: to the end of the stretch, back over it, then the new text.
    /// They are the ones a user would have typed, because the wrapped program
    /// is only ever told what was typed.
    fn next_offer(&mut self, cycling: Cycling, cursor: usize) -> Vec<u8> {
        let Cycling {
            at,
            original,
            offers,
            offered,
            standing,
        } = cycling;
        // Past the last offer is the word the user wrote, which is how a cycle
        // that found nothing better gives the line back unchanged.
        let coming = offers.get(offered).unwrap_or(&original).clone();
        let mut keys = Vec::new();
        // Only ever forwards: a flag is claimed with the cursor inside it or at
        // its end, and the stretch always ends where the flag does.
        for _ in cursor..at + standing.chars().count() {
            keys.extend_from_slice(b"\x1b[C");
        }
        keys.resize(keys.len() + standing.chars().count(), 0x7f);
        keys.extend_from_slice(coming.as_bytes());
        self.cycling = Some(Cycling {
            at,
            original,
            offered: (offered + 1) % (offers.len() + 1),
            standing: coming,
            offers,
        });
        keys
    }
}

/// A flag is the one being asked about when the cursor is inside it or has
/// just left it — the word being typed is the word in question.
fn covers(flag: &Flag, cursor: usize) -> bool {
    (flag.span.start..=flag.span.end).contains(&cursor)
}

/// The stretch a fix stands over and the texts that can stand in it. A removal
/// takes the space before the word with it, or two words would run together
/// where one was taken out — and that widens the stretch for every offer on the
/// flag, not only the removal, or accepting one would leave the line holding
/// the shape of another.
///
/// `None` when there is nothing to offer, or when the span is not a stretch of
/// this line: a flag is a reading of one particular line, and one that does not
/// fit the line it is applied to would take out characters nobody meant.
fn cycling_over(line: &str, flag: &Flag) -> Option<Cycling> {
    let characters: Vec<char> = line.chars().collect();
    if flag.span.start > flag.span.end || flag.span.end > characters.len() {
        return None;
    }
    let word: String = characters[flag.span.clone()].iter().collect();
    let removes = flag.suggestions.contains(&Edit::Remove);
    let spaced = removes && flag.span.start > 0 && characters[flag.span.start - 1] == ' ';
    let held = match spaced {
        true => " ",
        false => "",
    };
    let offers: Vec<String> = flag
        .suggestions
        .iter()
        .map(|edit| match edit {
            Edit::Replace(text) => format!("{held}{text}"),
            Edit::InsertAfter(text) => format!("{held}{word}{text}"),
            Edit::Remove => String::new(),
        })
        .collect();
    let original = format!("{held}{word}");
    (!offers.is_empty()).then_some(Cycling {
        at: flag.span.start - usize::from(spaced),
        standing: original.clone(),
        original,
        offers,
        offered: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag(span: std::ops::Range<usize>, suggestions: &[Edit]) -> Flag {
        classed(span, Class::Spelling, suggestions)
    }

    fn classed(span: std::ops::Range<usize>, class: Class, suggestions: &[Edit]) -> Flag {
        Flag {
            span,
            class,
            suggestions: suggestions.to_vec(),
        }
    }

    fn picking(line: &str, flags: Vec<Flag>) -> Picker {
        learning("picking", line, flags).0
    }

    /// A picker with both keys, and the list a learned word is written to.
    /// A word list written where the test can reach it, named after the test so
    /// two running at once cannot collide.
    fn learning(name: &str, line: &str, flags: Vec<Flag>) -> (Picker, SharedLearned, PathBuf) {
        let checked = SharedChecked::default();
        *checked.lock().expect("checked") = Checked {
            line: line.to_string(),
            flags,
        };
        let learned = SharedLearned::default();
        let words =
            std::env::temp_dir().join(format!("blackjack-{name}-{}.txt", std::process::id()));
        let picker = Picker::new(
            Chord::parse("ctrl+.").expect("a chord"),
            Chord::parse("ctrl+,").expect("a chord"),
            checked,
            learned.clone(),
            Some(words.clone()),
        );
        (picker, learned, words)
    }

    /// The modifiers the user writes, in the order they feel like writing them,
    /// and the key last — including when the key is the `+` between them.
    #[test]
    fn a_chord_is_its_modifiers_and_its_key() {
        assert_eq!(
            Chord::parse("ctrl+."),
            Some(Chord {
                modifiers: CTRL,
                character: '.'
            })
        );
        assert_eq!(
            Chord::parse("Alt + Shift + x"),
            Some(Chord {
                modifiers: ALT | SHIFT,
                character: 'x'
            })
        );
        assert_eq!(
            Chord::parse("ctrl++"),
            Some(Chord {
                modifiers: CTRL,
                character: '+'
            })
        );
    }

    /// A config line the user got wrong costs them that line and nothing else,
    /// so it has to be recognisable as wrong rather than read as something.
    #[test]
    fn a_chord_nobody_can_press_is_not_a_chord() {
        assert_eq!(Chord::parse("hyper+x"), None);
        assert_eq!(Chord::parse("ctrl+esc"), None);
        assert_eq!(Chord::parse(""), None);
        assert_eq!(Chord::parse("."), None, "a key with no modifier is text");
    }

    /// A terminal without the kitty protocol says only that a meta key was
    /// held, never which. Rather than leave such a terminal with no fix key at
    /// all, the escape-prefixed form of the character is taken as the chord.
    #[test]
    fn a_terminal_that_cannot_say_which_modifier_is_taken_at_its_word() {
        let ctrl = Chord::parse("ctrl+f").expect("a chord");

        assert!(
            ctrl.matches(b"\x1b[102;5u"),
            "the modifier it was told about"
        );
        assert!(ctrl.matches(b"\x1bf"), "all the older form can say");
        assert!(!ctrl.matches(b"\x06"), "ctrl-f as such a terminal sends it");
    }

    /// Two encodings for one key: what a terminal speaking the kitty protocol
    /// sends, and the escape-prefixed form of one that does not.
    #[test]
    fn a_chord_answers_to_both_of_its_encodings() {
        let chord = Chord::parse("ctrl+.").expect("a chord");

        assert!(chord.matches(b"\x1b[46;5u"));
        assert!(chord.matches(b"\x1b."));
        assert!(!chord.matches(b"\x1b[46;3u"), "another modifier");
        assert!(!chord.matches(b"\x1b[13;5u"), "another key");
        assert!(!chord.matches(b"."), "the bare character is text");
        assert!(!chord.matches(b"\x1b[46;5ux"), "a burst it cannot claim");
    }

    /// The keystrokes a user would have typed to make the fix themselves: back
    /// over the word, then the word that was meant.
    #[test]
    fn a_flagged_word_is_replaced_by_the_keys_that_would_have_replaced_it() {
        let mut picker = picking("a teh", vec![flag(2..5, &[Edit::Replace("the".into())])]);

        let keys = picker.fix("a teh", 5).expect("a fix");

        assert_eq!(keys, b"\x7f\x7f\x7fthe");
    }

    /// The cursor is wherever the user left it in the word, and the fix starts
    /// by taking it to the end of what it is about to replace.
    #[test]
    fn the_cursor_is_walked_to_the_end_of_the_word_first() {
        let mut picker = picking("a teh", vec![flag(2..5, &[Edit::Replace("the".into())])]);

        let keys = picker.fix("a teh", 3).expect("a fix");

        assert_eq!(keys, b"\x1b[C\x1b[C\x7f\x7f\x7fthe");
    }

    /// Pressing again asks for the next one, and past the last one the word the
    /// user wrote comes back — a cycle that found nothing better costs nothing.
    #[test]
    fn pressing_again_cycles_the_offers_and_then_returns_the_word() {
        let offers = [
            Edit::Replace("the".into()),
            Edit::Replace("tea".into()),
            Edit::Replace("ten".into()),
        ];
        let mut picker = picking("a teh", vec![flag(2..5, &offers)]);

        assert_eq!(picker.fix("a teh", 5).expect("first"), b"\x7f\x7f\x7fthe");
        assert_eq!(picker.fix("a the", 5).expect("second"), b"\x7f\x7f\x7ftea");
        assert_eq!(picker.fix("a tea", 5).expect("third"), b"\x7f\x7f\x7ften");
        assert_eq!(picker.fix("a ten", 5).expect("back"), b"\x7f\x7f\x7fteh");
        assert_eq!(picker.fix("a teh", 5).expect("round"), b"\x7f\x7f\x7fthe");
    }

    /// Anything else the user does ends the offer, and the press after that is
    /// a fresh question rather than the next answer to the old one.
    #[test]
    fn a_keystroke_of_their_own_ends_the_cycle() {
        let offers = [Edit::Replace("the".into()), Edit::Replace("tea".into())];
        let mut picker = picking("a teh", vec![flag(2..5, &offers)]);
        assert_eq!(picker.fix("a teh", 5).expect("a fix"), b"\x7f\x7f\x7fthe");

        picker.interrupted();

        assert_eq!(
            picker.fix("a teh", 5).expect("a fix"),
            b"\x7f\x7f\x7fthe",
            "the offer after the interruption was the next one, not the first"
        );
    }

    /// A suggestion that adds rather than replaces keeps the word and types
    /// what goes after it.
    #[test]
    fn an_insertion_keeps_the_word_and_adds_to_it() {
        let mut picker = picking("its a", vec![flag(0..3, &[Edit::InsertAfter("'".into())])]);

        let keys = picker.fix("its a", 3).expect("a fix");

        assert_eq!(keys, b"\x7f\x7f\x7fits'");
    }

    /// A removal takes the space before the word with it, or two words would
    /// run together where one was taken out.
    #[test]
    fn a_removal_takes_the_space_before_the_word_too() {
        let mut picker = picking("one the the", vec![flag(8..11, &[Edit::Remove])]);

        let keys = picker.fix("one the the", 11).expect("a fix");

        assert_eq!(keys, b"\x7f\x7f\x7f\x7f", "the word and its space");
    }

    /// A word starting the line has no space before it to take.
    #[test]
    fn a_removal_at_the_start_takes_only_the_word() {
        let mut picker = picking("teh cat", vec![flag(0..3, &[Edit::Remove])]);

        assert_eq!(picker.fix("teh cat", 3).expect("a fix"), b"\x7f\x7f\x7f");
    }

    /// One flag can offer a removal and a replacement both, and the removal is
    /// what decides how much of the line the offers stand over — so the others
    /// have to put back the space it takes, or accepting one would leave the
    /// line shaped like the other.
    #[test]
    fn a_replacement_beside_a_removal_keeps_the_space_the_removal_takes() {
        let offers = [Edit::Remove, Edit::Replace("the".into())];
        let mut picker = picking("one teh", vec![flag(4..7, &offers)]);

        assert_eq!(
            picker.fix("one teh", 7).expect("removal"),
            b"\x7f\x7f\x7f\x7f"
        );
        assert_eq!(picker.fix("one", 3).expect("replacement"), b" the");
    }

    /// A word the user meant is written down where the next session will read
    /// it, once — a list that gains the same word twice is a list that grows
    /// every time they press the key.
    #[test]
    fn a_learned_word_is_written_down_once() {
        let (mut picker, learned, words) = learning(
            "written-down",
            "a parkin",
            vec![flag(2..8, &[Edit::Replace("parking".into())])],
        );

        assert_eq!(picker.learn("a parkin", 8), Some("parkin".to_string()));

        assert_eq!(
            std::fs::read_to_string(&words).expect("the list"),
            "parkin\n"
        );
        assert!(learned.lock().expect("learned").contains("parkin"));
        std::fs::remove_file(&words).ok();
    }

    /// Pressing it again on a word already known has nothing to do, so the
    /// keystroke belongs to the wrapped program after all.
    /// The command key, which the config can now name. It is the one modifier
    /// no terminal sends unasked, so the kitty form is what a terminal bound to
    /// send it produces — and the escape-prefixed fallback still answers, as it
    /// does for every chord, since a terminal that cannot say which modifier
    /// was held is exactly the one that has nothing else to offer.
    #[test]
    fn the_command_key_is_a_chord_the_config_can_name() {
        let chord = Chord::parse("cmd+.").expect("a chord");

        assert_eq!(chord.to_string(), "cmd+.");
        assert!(chord.matches(b"\x1b[46;9u"), "the kitty form of cmd+.");
        assert!(
            !chord.matches(b"\x1b[46;5u"),
            "that is ctrl+., a different key"
        );
        assert!(
            chord.matches(b"\x1b."),
            "the fallback answers for any chord"
        );
    }

    #[test]
    fn learning_a_word_twice_is_a_keystroke_the_program_gets() {
        let (mut picker, _, words) = learning(
            "learned-twice",
            "a parkin",
            vec![flag(2..8, &[Edit::Replace("parking".into())])],
        );
        picker.learn("a parkin", 8).expect("learned");

        assert_eq!(picker.learn("a parkin", 8), None);

        assert_eq!(
            std::fs::read_to_string(&words).expect("the list"),
            "parkin\n"
        );
        std::fs::remove_file(&words).ok();
    }

    /// A grammar slip is not a word, so there is nothing under the cursor to
    /// teach a dictionary: the key is the user's and the program gets it.
    #[test]
    fn a_grammar_flag_is_not_something_to_learn() {
        let (mut picker, learned, words) = learning(
            "grammar",
            "a apple",
            vec![classed(0..1, Class::Grammar, &[Edit::Replace("an".into())])],
        );

        assert_eq!(picker.learn("a apple", 1), None);

        assert!(learned.lock().expect("learned").is_empty());
        assert!(!words.exists(), "a grammar slip reached the word list");
    }

    /// The two keys agree about what is flagged. A word the user has kept is no
    /// longer underlined, so the fix key does not rewrite it either — a key
    /// acting on something the screen stopped showing is a surprise.
    #[test]
    fn the_fix_key_leaves_a_learned_word_alone() {
        let (mut picker, learned, _) = learning(
            "fix-learned",
            "a parkin",
            vec![flag(2..8, &[Edit::Replace("parking".into())])],
        );
        assert!(picker.fix("a parkin", 8).is_some(), "no fix to begin with");
        picker.interrupted();

        learned
            .lock()
            .expect("learned")
            .insert("parkin".to_string());

        assert_eq!(picker.fix("a parkin", 8), None);
    }

    /// The same rule as the fix key: it answers for the word under the cursor
    /// and for nothing else.
    #[test]
    fn learning_needs_a_flag_under_the_cursor_too() {
        let (mut picker, learned, _) = learning(
            "no-flag",
            "a parkin word",
            vec![flag(2..8, &[Edit::Replace("parking".into())])],
        );

        assert_eq!(picker.learn("a parkin word", 13), None);

        assert!(learned.lock().expect("learned").is_empty());
    }

    /// The flags are a reading of one particular line. Typed past, they are
    /// about a line that no longer exists, and their spans point anywhere.
    #[test]
    fn flags_about_another_line_are_not_offered() {
        let mut picker = picking("a teh", vec![flag(2..5, &[Edit::Replace("the".into())])]);

        assert_eq!(picker.fix("a teh word", 5), None);
    }

    /// A span reaching past the line it is applied to would take out characters
    /// nobody meant, and on this thread it would take the keystrokes with it.
    #[test]
    fn a_span_the_line_cannot_hold_is_not_a_fix() {
        let mut picker = picking("a teh", vec![flag(2..99, &[Edit::Replace("the".into())])]);

        assert_eq!(picker.fix("a teh", 5), None);
    }

    /// A cursor nowhere near anything flagged is the user pressing a key that
    /// has nothing to do here, and it goes to the program instead.
    #[test]
    fn a_cursor_outside_every_flag_offers_nothing() {
        let mut picker = picking(
            "a teh word",
            vec![flag(2..5, &[Edit::Replace("the".into())])],
        );

        assert_eq!(picker.fix("a teh word", 10), None);
    }

    /// A flag with nothing to offer is a word the checker could only object to.
    #[test]
    fn a_flag_with_no_suggestions_is_not_a_fix() {
        let mut picker = picking("a teh", vec![flag(2..5, &[])]);

        assert_eq!(picker.fix("a teh", 5), None);
    }
}
