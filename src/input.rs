use vte::{Params, Parser, Perform};

use crate::screen::param_as_written;

const ESC: u8 = 0x1b;
const DEL: char = '\u{7f}';
const NONE: u16 = 0;
const ALT: u16 = 0b10;
const CTRL: u16 = 0b100;
/// Caps lock and num lock ride in the modifier field beside the keys held down,
/// so a latched lock would otherwise make every key a combination none of the
/// tables below name.
const LOCKS: u16 = 0b1100_0000;

/// `Submit` carries the line as it was typed. `edited` means a key was seen
/// that this model records but does not apply, so the line is no longer the one
/// being looked at. A paste never sets it.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Submit { line: String, edited: bool },
    Edit,
}

/// The line the user is typing, rebuilt from the bytes on their way to the
/// wrapped program. Nothing tells a wrapper what was typed either, so this is
/// the only account of the input line there is.
pub struct Input {
    parser: Parser,
    buffer: Buffer,
    /// An escape byte is held back until the byte after it is known: `ESC DEL`
    /// is Alt+Backspace, and fed as it stands the parser takes the character
    /// after it as that sequence's final byte and the text loses it.
    held_escape: bool,
}

impl Input {
    pub fn new(from_a_pipe: bool) -> Self {
        Self {
            parser: Parser::new(),
            buffer: Buffer {
                from_a_pipe,
                ..Buffer::default()
            },
            held_escape: false,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while !rest.is_empty() {
            if self.held_escape {
                self.held_escape = false;
                if rest[0] as char == DEL {
                    self.buffer.apply(Key::KillWord);
                    rest = &rest[1..];
                } else {
                    self.parser.advance(&mut self.buffer, &[ESC]);
                }
                continue;
            }
            let Some(at) = rest.iter().position(|&byte| byte == ESC) else {
                self.parser.advance(&mut self.buffer, rest);
                return;
            };
            self.parser.advance(&mut self.buffer, &rest[..at]);
            self.held_escape = true;
            rest = &rest[at + 1..];
        }
    }

    pub fn line(&self) -> &str {
        &self.buffer.text
    }

    /// Where in the line the next character goes, counted in characters — the
    /// columns a painter needs come from the screen model, not from here.
    pub fn cursor(&self) -> usize {
        self.buffer.cursor
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.buffer.events)
    }
}

/// What a key does to the line. `Unapplied` is the rest: recorded so that the
/// submit carrying the line can say it is no longer the one being looked at.
#[derive(Clone, Copy)]
enum Key {
    Backspace,
    Delete,
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    KillToStart,
    KillToEnd,
    /// ctrl-W, whose word ends at whitespace.
    KillToWhitespace,
    /// Alt+Backspace, whose word ends where readline's does — at the first
    /// character that is not alphanumeric.
    KillWord,
    Unapplied,
}

#[derive(Default)]
struct Buffer {
    text: String,
    cursor: usize,
    events: Vec<Event>,
    edited: bool,
    /// A pipe has no Enter key: its lines end with a line feed, which on a
    /// terminal is ctrl-J and ends nothing.
    from_a_pipe: bool,
    /// So that the line feed of a CRLF is read as part of that one ending.
    /// Anything typed since the return ends the pairing.
    after_return: bool,
    /// Between `CSI 200~` and `CSI 201~` the bytes are a paste, not typing.
    /// Counting them would reconstruct a line nobody wrote.
    pasting: bool,
    /// SS3 (`ESC O`, how an arrow arrives without the kitty protocol) shifts
    /// exactly the next character out of the text — and the parser reports the
    /// shift, then prints that character like any other.
    shifted: bool,
}

impl Buffer {
    fn submit(&mut self) {
        let line = std::mem::take(&mut self.text);
        let edited = std::mem::take(&mut self.edited);
        self.cursor = 0;
        self.events.push(Event::Submit { line, edited });
    }

    /// Where the character at `chars` starts, or the end of the line.
    fn byte_of(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(at, _)| at)
    }

    fn chars(&self) -> usize {
        self.text.chars().count()
    }

    /// Back over the run of boundary characters the cursor sits in, then back
    /// over the word before it. Both kills work this way and differ only in
    /// what they count as a boundary.
    fn start_of_word(&self, boundary: impl Fn(char) -> bool) -> usize {
        let mut at = self.cursor;
        let mut in_word = false;
        for c in self.text[..self.byte_of(self.cursor)].chars().rev() {
            if boundary(c) && in_word {
                break;
            }
            in_word |= !boundary(c);
            at -= 1;
        }
        at
    }

    fn end_of_word(&self) -> usize {
        let mut at = self.cursor;
        let mut in_word = false;
        for c in self.text[self.byte_of(self.cursor)..].chars() {
            if word_boundary(c) && in_word {
                break;
            }
            in_word |= !word_boundary(c);
            at += 1;
        }
        at
    }

    fn remove_at_cursor(&mut self) {
        let at = self.byte_of(self.cursor);
        if at < self.text.len() {
            self.text.remove(at);
        }
    }

    fn cut_back_to(&mut self, start: usize) {
        let range = self.byte_of(start)..self.byte_of(self.cursor);
        self.text.replace_range(range, "");
        self.cursor = start;
    }

    fn apply(&mut self, key: Key) {
        if self.pasting {
            return;
        }
        match key {
            Key::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.remove_at_cursor();
                }
            }
            Key::Delete => self.remove_at_cursor(),
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(self.chars()),
            Key::WordLeft => self.cursor = self.start_of_word(word_boundary),
            Key::WordRight => self.cursor = self.end_of_word(),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = self.chars(),
            Key::KillToStart => self.cut_back_to(0),
            Key::KillToEnd => self.text.truncate(self.byte_of(self.cursor)),
            Key::KillToWhitespace => {
                let start = self.start_of_word(char::is_whitespace);
                self.cut_back_to(start);
            }
            Key::KillWord => {
                let start = self.start_of_word(word_boundary);
                self.cut_back_to(start);
            }
            Key::Unapplied => {
                self.edited = true;
                self.events.push(Event::Edit);
            }
        }
    }
}

impl Perform for Buffer {
    fn print(&mut self, c: char) {
        self.after_return = false;
        if self.pasting || std::mem::take(&mut self.shifted) {
            return;
        }
        if c == DEL {
            self.apply(Key::Backspace);
            return;
        }
        let at = self.byte_of(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    fn execute(&mut self, byte: u8) {
        if self.pasting {
            return;
        }
        let after_return = std::mem::take(&mut self.after_return);
        match byte {
            0x0d => {
                self.submit();
                self.after_return = true;
            }
            0x0a if self.from_a_pipe && !after_return => self.submit(),
            _ => {
                if let Some(key) = control_key(byte) {
                    self.apply(key);
                }
            }
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        // A marked form is a report rather than a key — `CSI < … M` is the
        // mouse. Every sequence reaching here is skipped whole, which is what
        // keeps a key's final byte out of the text.
        if ignore || !intermediates.is_empty() {
            return;
        }
        if action == '~' {
            match param_as_written(params, 0) {
                200 => return self.pasting = true,
                201 => return self.pasting = false,
                _ => {}
            }
        }
        if let Some(key) = sequence_key(params, action) {
            self.apply(key);
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if !self.pasting && intermediates.is_empty() && byte == b'O' {
            self.shifted = true;
        }
    }
}

/// Where readline's word kills and word moves stop: a word is a run of
/// alphanumerics, and everything else is between words.
fn word_boundary(c: char) -> bool {
    !c.is_alphanumeric()
}

/// The ctrl chords a line editor binds, as the C0 bytes they arrive as.
fn control_key(byte: u8) -> Option<Key> {
    Some(match byte {
        0x08 => Key::Backspace,
        0x01 => Key::Home,
        0x05 => Key::End,
        0x0b => Key::KillToEnd,
        0x15 => Key::KillToStart,
        0x17 => Key::KillToWhitespace,
        // Yank and transpose, which need a kill ring and a swap.
        0x14 | 0x19 => Key::Unapplied,
        _ => return None,
    })
}

/// The editing keys as a terminal sends them. A modifier field is the bitmask
/// plus one, and absent it reads as no modifier at all; a combination none of
/// these arms names is a key this model cannot claim to know.
fn sequence_key(params: &Params, action: char) -> Option<Key> {
    let modifiers = param_as_written(params, 1).saturating_sub(1) & !LOCKS;
    Some(match (action, modifiers) {
        // History recall replaces the whole line, which needs the history.
        ('A' | 'B', _) => Key::Unapplied,
        ('C', NONE) => Key::Right,
        ('D', NONE) => Key::Left,
        ('C', ALT) => Key::WordRight,
        ('D', ALT) => Key::WordLeft,
        ('H', NONE) => Key::Home,
        ('F', NONE) => Key::End,
        ('C' | 'D' | 'H' | 'F', _) => Key::Unapplied,
        ('~', _) => match (param_as_written(params, 0), modifiers) {
            (1 | 7, NONE) => Key::Home,
            (4 | 8, NONE) => Key::End,
            (3, NONE) => Key::Delete,
            (1 | 3 | 4 | 5 | 6 | 7 | 8, _) => Key::Unapplied,
            _ => return None,
        },
        ('u', _) => return kitty_key(param_as_written(params, 0), modifiers),
        _ => return None,
    })
}

/// `CSI <codepoint> ; <modifiers> u`, the encoding Claude Code asks for.
/// Backspace arrives here as 127; Delete keeps its `CSI 3~` form.
fn kitty_key(codepoint: u16, modifiers: u16) -> Option<Key> {
    if codepoint == 127 {
        return Some(match modifiers {
            NONE => Key::Backspace,
            ALT => Key::KillWord,
            _ => Key::Unapplied,
        });
    }
    let chord = match codepoint {
        97 => Key::Home,
        101 => Key::End,
        107 => Key::KillToEnd,
        117 => Key::KillToStart,
        119 => Key::KillToWhitespace,
        // Yank and transpose, which need a kill ring and a swap.
        116 | 121 => Key::Unapplied,
        _ => return None,
    };
    Some(match modifiers {
        CTRL => chord,
        // The character itself, reported as a sequence.
        NONE => return None,
        _ => Key::Unapplied,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(chunks: &[&[u8]]) -> Input {
        piped(false, chunks)
    }

    fn piped(from_a_pipe: bool, chunks: &[&[u8]]) -> Input {
        let mut input = Input::new(from_a_pipe);
        for chunk in chunks {
            input.feed(chunk);
        }
        input
    }

    fn submitted(chunks: &[&[u8]]) -> Vec<Event> {
        let mut input = typed(chunks);
        input
            .take_events()
            .into_iter()
            .filter(|event| matches!(event, Event::Submit { .. }))
            .collect()
    }

    fn submit(line: &str, edited: bool) -> Event {
        Event::Submit {
            line: line.into(),
            edited,
        }
    }

    #[test]
    fn collects_what_was_typed() {
        assert_eq!(typed(&[b"hello"]).line(), "hello");
    }

    /// A read returns whatever has arrived, which cuts a multi-byte character
    /// in half often enough to matter for the wide words that started this.
    #[test]
    fn a_character_split_across_two_reads_arrives_once() {
        let wide = "中".as_bytes();
        assert_eq!(typed(&[&wide[..1], &wide[1..]]).line(), "中");
    }

    #[test]
    fn enter_submits_the_line_and_starts_the_next() {
        let mut input = typed(&[b"hello\rworld"]);
        assert_eq!(input.take_events(), vec![submit("hello", false)]);
        assert_eq!(input.line(), "world");
    }

    /// A pipe has no Enter key, and nothing else would ever submit its lines.
    #[test]
    fn a_line_feed_submits_a_piped_line() {
        let mut input = piped(true, &[b"one\ntwo\n"]);
        assert_eq!(
            input.take_events(),
            vec![submit("one", false), submit("two", false)]
        );
    }

    #[test]
    fn the_line_feed_of_a_crlf_does_not_submit_an_empty_line() {
        let mut input = piped(true, &[b"one\r\ntwo\r\n"]);
        assert_eq!(
            input.take_events(),
            vec![submit("one", false), submit("two", false)]
        );
    }

    /// On a terminal a line feed is ctrl-J, which ends nothing — reading it as
    /// Enter would report one line as two.
    #[test]
    fn a_line_feed_typed_at_a_terminal_is_not_a_submit() {
        let mut input = typed(&[b"hel\nlo\r"]);
        assert_eq!(input.take_events(), vec![submit("hello", false)]);
    }

    /// Claude Code pushes the kitty keyboard flags, so Esc and the modified
    /// keys arrive encoded while plain text still arrives as UTF-8.
    #[test]
    fn a_kitty_encoded_key_is_not_typing() {
        assert_eq!(
            submitted(&[b"hello wor\x1b[27uld\r"]),
            vec![submit("hello world", false)]
        );
    }

    #[test]
    fn a_mouse_report_is_not_typing() {
        let mut input = typed(&[b"ab\x1b[<0;10;5Mcd"]);
        assert_eq!(input.line(), "abcd");
        assert_eq!(input.take_events(), vec![]);
    }

    /// Without the kitty protocol an arrow arrives as SS3, whose final byte is
    /// an ordinary character as far as the parser is concerned.
    #[test]
    fn an_ss3_arrow_leaves_its_final_byte_out_of_the_line() {
        assert_eq!(typed(&[b"ab\x1bOAcd"]).line(), "abcd");
    }

    /// The shift is owed to a character, and inside a paste no character comes
    /// to claim it — so it would be spent on the first one typed afterwards.
    #[test]
    fn a_shift_inside_a_paste_does_not_outlive_it() {
        assert_eq!(typed(&[b"\x1b[200~\x1bOA\x1b[201~hello"]).line(), "hello");
    }

    /// A paste is not typing. Counting its body here would
    /// reconstruct a line the user never wrote — submissions and edits included,
    /// since a pasted `\r` and a pasted DEL are ordinary bytes.
    #[test]
    fn a_paste_is_skipped_whole() {
        let mut input = typed(&[b"ab\x1b[200~pasted\r\x7f\x1b[3~text\x1b[201~cd"]);
        assert_eq!(input.line(), "abcd");
        assert_eq!(input.take_events(), vec![]);
    }

    #[test]
    fn backspace_deletes_the_character_before_the_cursor() {
        for backspace in [&b"\x7f"[..], b"\x08", b"\x1b[127u"] {
            let mut input = typed(&[b"ab", backspace, b"c"]);
            assert_eq!(input.line(), "ac", "for {backspace:?}");
            assert_eq!(input.take_events(), vec![], "for {backspace:?}");
        }
    }

    #[test]
    fn delete_takes_the_character_the_cursor_is_on() {
        let input = typed(&[b"abc\x1b[D\x1b[D\x1b[3~"]);
        assert_eq!(input.line(), "ac");
        assert_eq!(input.cursor(), 1);
    }

    #[test]
    fn text_goes_in_where_the_cursor_is() {
        let input = typed(&[b"helo\x1b[Dl"]);
        assert_eq!(input.line(), "hello");
        assert_eq!(input.cursor(), 4);
    }

    #[test]
    fn the_arrows_stop_at_the_ends_of_the_line() {
        let input = typed(&[b"ab\x1b[C\x1b[C\x1b[D\x1b[D\x1b[D"]);
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn home_and_end_jump_to_the_ends() {
        for (home, end) in [
            (&b"\x1b[H"[..], &b"\x1b[F"[..]),
            (b"\x1b[1~", b"\x1b[4~"),
            (b"\x1b[7~", b"\x1b[8~"),
            (b"\x01", b"\x05"),
            (b"\x1b[97;5u", b"\x1b[101;5u"),
        ] {
            let mut input = typed(&[b"abc", home]);
            assert_eq!(input.cursor(), 0, "for {home:?}");
            input.feed(end);
            assert_eq!(input.cursor(), 3, "for {end:?}");
        }
    }

    #[test]
    fn a_kill_takes_the_line_to_one_side_of_the_cursor() {
        for (chord, kept) in [
            (&b"\x15"[..], "cd"),
            (b"\x1b[117;5u", "cd"),
            (b"\x0b", "ab"),
            (b"\x1b[107;5u", "ab"),
        ] {
            let input = typed(&[b"abcd\x1b[D\x1b[D", chord]);
            assert_eq!(input.line(), kept, "for {chord:?}");
            assert_eq!(input.cursor(), if kept == "cd" { 0 } else { 2 });
        }
    }

    /// ctrl-W's word ends at whitespace; Alt+Backspace's ends where readline's
    /// does, at the first character that is not alphanumeric.
    #[test]
    fn the_two_word_kills_end_their_word_differently() {
        assert_eq!(typed(&[b"one two/three\x17"]).line(), "one ");
        assert_eq!(typed(&[b"one two/three\x1b[119;5u"]).line(), "one ");
        assert_eq!(typed(&[b"one two/three\x1b\x7f"]).line(), "one two/");
        assert_eq!(typed(&[b"one two/three\x1b[127;3u"]).line(), "one two/");
    }

    #[test]
    fn alt_moves_the_cursor_a_word_at_a_time() {
        let mut input = typed(&[b"one two\x1b[1;3D"]);
        assert_eq!(input.cursor(), 4);
        input.feed(b"\x1b[1;3D");
        assert_eq!(input.cursor(), 0);
        input.feed(b"\x1b[1;3C");
        assert_eq!(input.cursor(), 3);
    }

    /// Alt+Backspace is `ESC DEL`, and the parser reads a DEL inside a sequence
    /// as no byte at all — so the character after it would become the escape's
    /// final byte and leave the line without ever being seen.
    #[test]
    fn alt_backspace_does_not_swallow_the_next_character() {
        let mut input = typed(&[b"one two\x1b\x7fd"]);
        assert_eq!(input.line(), "one d");
        assert_eq!(input.take_events(), vec![]);
    }

    #[test]
    fn an_escape_split_across_two_reads_is_still_alt_backspace() {
        let mut input = typed(&[b"one two\x1b", b"\x7fd"]);
        assert_eq!(input.line(), "one d");
        assert_eq!(input.take_events(), vec![]);
    }

    /// A DEL inside a sequence is not a keystroke, and the parser drops it — so
    /// reading the byte stream for DELs on its own would invent an edit here.
    #[test]
    fn a_del_inside_a_sequence_is_not_an_edit() {
        let mut input = typed(&[b"ab\x1b[2\x7f7uc"]);
        assert_eq!(input.line(), "abc");
        assert_eq!(input.take_events(), vec![]);
    }

    /// The pairing is between a return and the line feed that closes it, so a
    /// line typed after one still ends where its own return says.
    #[test]
    fn a_return_pairs_with_its_own_line_feed_and_no_other() {
        let mut input = piped(true, &[b"a\rb\n"]);
        assert_eq!(
            input.take_events(),
            vec![submit("a", false), submit("b", false)]
        );
    }

    #[test]
    fn a_blank_line_after_a_crlf_is_still_a_line() {
        let mut input = piped(true, &[b"a\r\n\n"]);
        assert_eq!(
            input.take_events(),
            vec![submit("a", false), submit("", false)]
        );
    }

    /// Whether a line was edited is a fact about that line, so it cannot
    /// outlive the submit that carried it.
    #[test]
    fn a_clean_line_after_an_edited_one_submits_clean() {
        let mut input = piped(true, &[b"a\x19b\nc\n"]);
        assert_eq!(
            input.take_events(),
            vec![Event::Edit, submit("ab", true), submit("c", false)]
        );
    }

    /// An unmodified key reported through the kitty encoding is the character
    /// itself arriving as a sequence — nothing was edited.
    #[test]
    fn an_unmodified_kitty_key_is_not_an_edit() {
        let mut input = typed(&[b"ab\x1b[97u\x1b[117;1u"]);
        assert_eq!(input.line(), "ab");
        assert_eq!(input.cursor(), 2);
        assert_eq!(input.take_events(), vec![]);
    }

    #[test]
    fn an_applied_edit_leaves_the_submit_clean() {
        assert_eq!(
            submitted(&[b"hello\x15world\r"]),
            vec![submit("world", false)]
        );
        assert_eq!(
            submitted(&[b"hello\x1b[117;5uworld\r"]),
            vec![submit("world", false)],
            "the kitty encoding of the same chord"
        );
    }

    /// History recall replaces the whole line from somewhere this model cannot
    /// see, so what it holds is no longer what the user is looking at.
    #[test]
    fn a_key_that_cannot_be_applied_makes_the_submit_dirty() {
        assert_eq!(
            submitted(&[b"hello\x1b[Aworld\r"]),
            vec![submit("helloworld", true)]
        );
    }

    /// ctrl-←, shift-←, ctrl-Delete, ctrl-Backspace and shift+ctrl-U: a modifier
    /// combination none of the tables names is not the bare key, because the
    /// editor may move by a word where the bare key moves by one. The model
    /// records the doubt instead of asserting a line it cannot know.
    #[test]
    fn a_modifier_the_tables_do_not_name_is_not_the_bare_key() {
        for key in [
            &b"\x1b[1;5D"[..],
            b"\x1b[1;2D",
            b"\x1b[1;5C",
            b"\x1b[1;5H",
            b"\x1b[1;5F",
            b"\x1b[1;5~",
            b"\x1b[3;5~",
            b"\x1b[4;5~",
            b"\x1b[7;5~",
            b"\x1b[8;5~",
            b"\x1b[127;5u",
            b"\x1b[117;6u",
        ] {
            let mut input = typed(&[b"one two", key]);
            assert_eq!(input.cursor(), 7, "for {key:?}");
            input.feed(b"\r");
            assert_eq!(
                input.take_events(),
                vec![Event::Edit, submit("one two", true)],
                "for {key:?}"
            );
        }
    }

    /// A lock is reported in the same field as the keys actually held down, so
    /// latching one must not turn a key the tables name into one they do not.
    #[test]
    fn a_latched_lock_is_not_a_modifier() {
        for (key, line, cursor) in [
            (&b"\x1b[1;65D"[..], "one two", 6),
            (b"\x1b[1;129D", "one two", 6),
            (b"\x1b[1;67D", "one two", 4),
            (b"\x1b[3;65~", "one two", 7),
            (b"\x1b[127;65u", "one tw", 6),
            (b"\x1b[97;69u", "one two", 0),
        ] {
            let mut input = typed(&[b"one two", key]);
            assert_eq!(input.line(), line, "for {key:?}");
            assert_eq!(input.cursor(), cursor, "for {key:?}");
            assert_eq!(input.take_events(), vec![], "for {key:?}");
        }
    }

    /// The keys this model records but cannot carry out: history recall, the
    /// pages it has no history for, and the chords needing a kill ring or a
    /// swap.
    #[test]
    fn a_key_that_cannot_be_applied_is_recorded_and_changes_nothing() {
        for key in [
            &b"\x1b[A"[..],
            b"\x1b[B",
            b"\x1b[5~",
            b"\x1b[6~",
            b"\x14",
            b"\x19",
            b"\x1b[116;5u",
            b"\x1b[121;5u",
        ] {
            let mut input = typed(&[b"ab", key]);
            assert_eq!(input.line(), "ab", "for {key:?}");
            assert_eq!(input.take_events(), vec![Event::Edit], "for {key:?}");
        }
    }
}
