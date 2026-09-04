//! Turning a flagged word into one of its suggestions, without the user
//! leaving the keyboard. No menu is drawn: pressing the key again is how the
//! next suggestion is asked for, and pressing anything else is how the offer
//! is declined.

use std::sync::{Arc, Mutex};

use crate::checker::{Edit, Flag};
use crate::input::{ALT, CTRL, SHIFT};

/// The key that fixes the word under the cursor, as the user writes it in the
/// config: the modifiers held down, joined by `+`, and then the character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    modifiers: u16,
    character: char,
}

impl Chord {
    /// `ctrl+.`, `alt+shift+x`. The key is the last character written, which
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
    checked: SharedChecked,
    cycling: Option<Cycling>,
}

impl Picker {
    pub fn new(chord: Chord, checked: SharedChecked) -> Self {
        Self {
            chord,
            checked,
            cycling: None,
        }
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
        let checked = self.checked.lock().ok()?;
        if checked.line != line {
            return None;
        }
        let flag = checked
            .flags
            .iter()
            .find(|flag| (flag.span.start..=flag.span.end).contains(&cursor))?;
        cycling_over(line, flag)
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
        Flag {
            span,
            message: String::new(),
            suggestions: suggestions.to_vec(),
        }
    }

    fn picking(line: &str, flags: Vec<Flag>) -> Picker {
        let checked = SharedChecked::default();
        *checked.lock().expect("checked") = Checked {
            line: line.to_string(),
            flags,
        };
        Picker::new(Chord::parse("ctrl+.").expect("a chord"), checked)
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
