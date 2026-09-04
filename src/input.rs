use std::sync::{Arc, Mutex};

use vte::{Params, Perform};

use crate::parser::Parser;
use crate::screen::{Line, param_as_written};

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

/// Shared because the line is rebuilt on the thread forwarding the keystrokes
/// and corrected on the one reading the program's output, at the frame boundary
/// where the program has finished saying what it is showing.
pub type SharedInput = Arc<Mutex<Input>>;

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
                match rest[0] as char {
                    DEL => self.buffer.apply(Key::KillWord),
                    // Alt+Enter as a terminal without the kitty protocol sends
                    // it, and the same line break that protocol spells out.
                    // Escape then Enter, typed as two keys, is the same two
                    // bytes and cannot be told apart without a clock — read as
                    // a break it is corrected by the next frame, where the box
                    // shows the line it really has. Letting the escape reach
                    // the parser instead, which is what happened before, was
                    // wrong for both: it submitted on the carriage return and
                    // then ate the byte after it as the escape's own final.
                    '\r' => self.buffer.apply(Key::Break),
                    _ => {
                        self.parser.advance(&mut self.buffer, &[ESC]);
                        continue;
                    }
                }
                rest = &rest[1..];
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

    pub fn stale(&self) -> bool {
        self.buffer.stale
    }

    /// Whether the user has touched the line since this was last asked. The
    /// screen it will be drawn on is the next one, so a line that merely
    /// disagrees with this one is the model running ahead rather than wrong.
    pub fn raced_the_frame(&mut self) -> bool {
        std::mem::take(&mut self.buffer.fed)
    }

    /// Take the line from the screen. The program draws what it is really
    /// holding, so where the two disagree this model is the one that is wrong.
    /// Takes the screen's line as [`Input::rejoin`] hands it back, so that what
    /// is stored and what is checked are the one reading. Answers whether the
    /// screen had anything to say that was not already here.
    pub fn resync(&mut self, typed: &Line) -> bool {
        let buffer = &mut self.buffer;
        let corrected = typed.text != buffer.text || typed.cursor != buffer.cursor;
        buffer.at = typed
            .text
            .char_indices()
            .nth(typed.cursor)
            .map_or(typed.text.len(), |(at, _)| at);
        buffer.text.clone_from(&typed.text);
        buffer.cursor = typed.cursor;
        buffer.stale = false;
        buffer.edited = false;
        corrected
    }

    /// The screen's line as the user typed it, each character still carrying
    /// the cell it was drawn in. Every row of the box after the first is one
    /// the screen offers a break for, and the grid draws three different things
    /// that way: a word wrapped with the space it broke at elided, a word too
    /// long to fit broken in the middle, and a break the user typed. Only the
    /// line being replaced tells them apart — at that point it holds the space,
    /// nothing, or the break. The middle case is the one that shortens what the
    /// screen handed over, which is why the cursor comes back through here as
    /// well; the spots come with it so that a checker reading the joined line
    /// and a painter drawing on the grid are talking about the same characters.
    pub fn rejoin(&self, line: &Line) -> Line {
        let mut replaced = self.buffer.text.chars().peekable();
        let mut text = String::with_capacity(line.text.len());
        let mut spots = Vec::with_capacity(line.spots.len());
        let mut cursor = line.cursor;
        for (at, character) in line.text.chars().enumerate() {
            let kept = match (character, replaced.peek()) {
                // A line this model has no account of — recalled from history,
                // or told to it by a key it could not carry out — is one whose
                // breaks it cannot read: each stands for the space, which at
                // least keeps words apart.
                ('\n', _) if self.buffer.stale => Some(' '),
                ('\n', Some('\n')) => Some('\n'),
                ('\n', Some(' ')) => Some(' '),
                // A word the box broke: nothing was typed where it broke, so
                // the line takes nothing — and neither does the cursor, which
                // is counted in what the screen drew.
                ('\n', Some(_)) => None,
                // The screen is holding more than this model ever saw.
                ('\n', None) => Some(' '),
                _ => Some(character),
            };
            match kept {
                Some(kept) => {
                    text.push(kept);
                    spots.push(line.spots.get(at).copied().flatten());
                    replaced.next();
                }
                None => cursor -= usize::from(at < line.cursor),
            }
        }
        Line {
            text,
            cursor,
            spots,
        }
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
    /// Shift+Enter, which breaks the line inside the box instead of sending it.
    Break,
    Unapplied,
}

#[derive(Default)]
struct Buffer {
    text: String,
    cursor: usize,
    /// Where `cursor` is in bytes. Carried rather than counted, because finding
    /// it by walking the line makes every insertion cost the length so far, and
    /// a line can arrive in bulk — piped, or pasted into a terminal that does
    /// not bracket it.
    at: usize,
    events: Vec<Event>,
    edited: bool,
    /// A pipe has no Enter key: its lines end with a line feed, which on a
    /// terminal is ctrl-J and ends nothing.
    from_a_pipe: bool,
    /// So that the line feed of a CRLF is read as part of that one ending.
    /// Anything typed since the return ends the pairing.
    after_return: bool,
    /// Between `CSI 200~` and `CSI 201~` the bytes are a paste. Its printable
    /// characters are text of the line, and so are the tabs and line endings
    /// the terminal let through; every other control byte is dropped, because
    /// the box holds none of them, and the chords an escape sequence in the
    /// body would spell were never pressed.
    pasting: bool,
    /// Set by a key this model records but does not apply, and by a submit —
    /// both leave it holding a line the program has since redrawn from
    /// somewhere this model cannot see.
    stale: bool,
    /// Set by what the user does to the line and taken by the frame that asks,
    /// so that a frame drawn before them can be told from one that disagrees.
    /// The terminal answers on this channel too — focus reports, device
    /// replies — and none of that is anybody typing.
    fed: bool,
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
        self.at = 0;
        self.stale = true;
        self.events.push(Event::Submit { line, edited });
    }

    /// Back over the run of boundary characters the cursor sits in, then back
    /// over the word before it. Both kills work this way and differ only in
    /// what they count as a boundary.
    fn start_of_word(&self, boundary: impl Fn(char) -> bool) -> (usize, usize) {
        let (mut cursor, mut at) = (self.cursor, self.at);
        let mut in_word = false;
        for c in self.text[..self.at].chars().rev() {
            if boundary(c) && in_word {
                break;
            }
            in_word |= !boundary(c);
            cursor -= 1;
            at -= c.len_utf8();
        }
        (cursor, at)
    }

    fn end_of_word(&self) -> (usize, usize) {
        let (mut cursor, mut at) = (self.cursor, self.at);
        let mut in_word = false;
        for c in self.text[self.at..].chars() {
            if word_boundary(c) && in_word {
                break;
            }
            in_word |= !word_boundary(c);
            cursor += 1;
            at += c.len_utf8();
        }
        (cursor, at)
    }

    fn insert(&mut self, c: char) {
        self.fed = true;
        self.text.insert(self.at, c);
        self.cursor += 1;
        self.at += c.len_utf8();
    }

    fn remove_at_cursor(&mut self) {
        if self.at < self.text.len() {
            self.text.remove(self.at);
        }
    }

    fn cut_back_to(&mut self, (cursor, at): (usize, usize)) {
        self.text.replace_range(at..self.at, "");
        self.cursor = cursor;
        self.at = at;
    }

    fn step_left(&mut self) {
        if let Some(c) = self.text[..self.at].chars().next_back() {
            self.cursor -= 1;
            self.at -= c.len_utf8();
        }
    }

    fn apply(&mut self, key: Key) {
        if self.pasting {
            return;
        }
        self.fed = true;
        match key {
            Key::Backspace => {
                if self.cursor > 0 {
                    self.step_left();
                    self.remove_at_cursor();
                }
            }
            Key::Delete => self.remove_at_cursor(),
            Key::Left => self.step_left(),
            Key::Right => {
                if let Some(c) = self.text[self.at..].chars().next() {
                    self.cursor += 1;
                    self.at += c.len_utf8();
                }
            }
            Key::WordLeft => (self.cursor, self.at) = self.start_of_word(word_boundary),
            Key::WordRight => (self.cursor, self.at) = self.end_of_word(),
            Key::Home => (self.cursor, self.at) = (0, 0),
            Key::End => (self.cursor, self.at) = (self.text.chars().count(), self.text.len()),
            Key::KillToStart => self.cut_back_to((0, 0)),
            Key::KillToEnd => self.text.truncate(self.at),
            Key::KillToWhitespace => {
                let start = self.start_of_word(char::is_whitespace);
                self.cut_back_to(start);
            }
            Key::KillWord => {
                let start = self.start_of_word(word_boundary);
                self.cut_back_to(start);
            }
            Key::Break => self.insert('\n'),
            Key::Unapplied => {
                self.edited = true;
                self.stale = true;
                self.events.push(Event::Edit);
            }
        }
    }
}

impl Perform for Buffer {
    fn print(&mut self, c: char) {
        self.after_return = false;
        if std::mem::take(&mut self.shifted) {
            return;
        }
        if c == DEL {
            self.apply(Key::Backspace);
            return;
        }
        self.insert(c);
    }

    fn execute(&mut self, byte: u8) {
        let after_return = std::mem::take(&mut self.after_return);
        if self.pasting {
            // The input box takes more than one line and renders a tab, so both
            // are text of the pasted line rather than the keys they would be.
            // A `\0` would collide with the screen model's own sentinel.
            match byte {
                0x09 => self.insert('\t'),
                0x0d => {
                    self.insert('\n');
                    self.after_return = true;
                }
                0x0a if !after_return => self.insert('\n'),
                _ => {}
            }
            return;
        }
        match byte {
            0x0d => {
                self.submit();
                self.after_return = true;
            }
            0x0a if self.from_a_pipe && !after_return => self.submit(),
            // What ends piped input: the first of the two flushes a line the
            // last newline never closed, and the wrapped program does receive
            // it, so the account of what was sent has to carry it too.
            0x04 if self.from_a_pipe && !self.text.is_empty() => self.submit(),
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
            // A line ending never straddles the bracket — the terminal sends the
            // whole body before closing it — and a shift owed from before the
            // paste is not owed to a character inside it.
            match param_as_written(params, 0) {
                200 => {
                    self.after_return = false;
                    self.shifted = false;
                    return self.pasting = true;
                }
                201 => {
                    self.after_return = false;
                    return self.pasting = false;
                }
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
    // Enter reaches this encoding only when it is not a submit — plain Enter
    // stays a carriage return — so a modified one is the box's line break,
    // whichever modifier the terminal reports it under.
    if codepoint == 13 && modifiers != NONE {
        return Some(Key::Break);
    }
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

    /// The screen's account of a line with no grid behind it: these are about
    /// what the model makes of the breaks, not where the box drew them.
    fn shown(text: &str, cursor: usize) -> Line {
        Line {
            text: text.into(),
            cursor,
            spots: Vec::new(),
        }
    }

    /// The box breaks a word too long for its width, and nothing was typed
    /// where it broke — so the line closes back up, and the cursor with it.
    #[test]
    fn a_word_the_box_broke_is_put_back_together() {
        let mut input = Input::new(false);
        input.feed(b"abcdefghij");
        // The screen is saying exactly what this model holds, in rows.
        let typed = input.rejoin(&shown("abcdef\nghij", 11));
        assert!(!input.resync(&typed));
        assert_eq!(input.line(), "abcdefghij");
        assert_eq!(input.cursor(), 10);
    }

    /// A wrap at a space elides it, and the line being replaced is what says a
    /// space was there to elide.
    #[test]
    fn the_space_the_box_wrapped_at_comes_back() {
        let mut input = Input::new(false);
        input.feed(b"abc def");
        let typed = input.rejoin(&shown("abc\ndef", 7));
        input.resync(&typed);
        assert_eq!(input.line(), "abc def");
    }

    /// A recalled line is one this model has no account of: the line it holds
    /// is the one being replaced, so reading the new line's breaks against it
    /// answers nothing. They are read as the spaces that keep words apart.
    #[test]
    fn the_breaks_of_a_recalled_line_are_read_as_spaces() {
        let mut input = Input::new(false);
        input.feed(b"abcdef");
        input.feed(b"\x1b[A");
        let typed = input.rejoin(&shown("abc\ndef", 7));
        input.resync(&typed);
        assert_eq!(input.line(), "abc def");
    }

    /// Enter with any modifier is the box's line break, not a submit: the
    /// terminal reports the modifier it saw, and which one it was is between
    /// the user and their keyboard.
    #[test]
    fn a_modified_enter_breaks_the_line_whichever_modifier_it_was() {
        for modified in [b"\x1b[13;2u".as_slice(), b"\x1b[13;3u", b"\x1b[13;5u"] {
            let mut input = Input::new(false);
            input.feed(b"ab");
            input.feed(modified);
            input.feed(b"cd");
            assert_eq!(input.line(), "ab\ncd");
            assert!(input.take_events().is_empty(), "a break submitted the line");
        }
    }

    /// The terminal answers on the channel the user types on — a focus report,
    /// a device reply — and none of that is anybody racing a frame.
    #[test]
    fn a_report_from_the_terminal_is_not_the_user_typing() {
        let mut input = Input::new(false);
        input.feed(b"\x1b[I");
        assert!(!input.raced_the_frame());
        input.feed(b"a");
        assert!(input.raced_the_frame());
        input.feed(b"\x1b[D");
        assert!(input.raced_the_frame());
    }

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
    /// in half often enough to matter for the wide words that started this. Fed
    /// straight to vte 0.15, the second case reconstructs as `é中`.
    #[test]
    fn a_character_split_across_two_reads_arrives_once() {
        let wide = "中".as_bytes();
        assert_eq!(typed(&[&wide[..1], &wide[1..]]).line(), "中");
        assert_eq!(typed(&[b"\xc3", b"\xa9/\xe4\xb8\xad"]).line(), "é/中");
    }

    #[test]
    fn enter_submits_the_line_and_starts_the_next() {
        let mut input = typed(&[b"hello\rworld"]);
        assert_eq!(input.take_events(), vec![submit("hello", false)]);
        assert_eq!(input.line(), "world");
    }

    /// On a terminal the same byte is the user pressing ctrl-D, which is the
    /// wrapped program's business and not a line being sent. Only the wrapper's
    /// own end-of-input, written for piped stdin, means a line was flushed.
    #[test]
    fn a_typed_end_of_input_submits_nothing() {
        let mut input = piped(false, &[b"one", &[0x04]]);

        assert!(input.take_events().is_empty());
        assert_eq!(input.line(), "one");
    }

    /// A piped input whose last line has no newline still reaches the wrapped
    /// program — the wrapper flushes it with an end-of-input byte, which is
    /// what a terminal does for a half-typed line. The account of what was sent
    /// has to carry that line too, or it is short by exactly the one the user
    /// is most likely to care about.
    #[test]
    fn the_last_piped_line_is_submitted_though_no_newline_closed_it() {
        let mut input = piped(true, &[b"one\ntwo", &[0x04, 0x04]]);

        assert_eq!(
            input.take_events(),
            [submit("one", false), submit("two", false)]
        );
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

    /// SS3 inside a paste sets no shift, or the very next pasted character would
    /// claim it and be eaten. The `A` stays in the line: once the parser has
    /// taken `ESC O`, its final byte is an ordinary printable — and no terminal
    /// sends an escape through a paste filter anyway.
    #[test]
    fn a_shift_inside_a_paste_does_not_outlive_it() {
        assert_eq!(typed(&[b"\x1b[200~\x1bOA\x1b[201~hello"]).line(), "Ahello");
    }

    /// A paste is text and not keys: the body goes into the line, while the
    /// chords inside it were never pressed and do nothing.
    #[test]
    fn a_paste_is_text_and_the_keys_inside_it_are_not() {
        let mut input = typed(&[b"ab\x1b[200~pasted\x7f\x1b[3~text\x1b[201~cd"]);
        assert_eq!(input.line(), "abpastedtextcd");
        assert_eq!(input.take_events(), vec![]);
    }

    #[test]
    fn a_paste_lands_where_the_cursor_is() {
        let input = typed(&[b"ac\x1b[D\x1b[200~b\x1b[201~"]);
        assert_eq!(input.line(), "abc");
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn a_multi_line_paste_keeps_its_line_breaks_and_submits_once() {
        let mut input = typed(&[b"\x1b[200~one\rtwo\n\x1b[201~\r"]);
        assert_eq!(input.take_events(), vec![submit("one\ntwo\n", false)]);
    }

    /// Pasted code is indented and the terminal sends the tab through, so
    /// dropping it would leave the line short of the box for the rest of that
    /// line.
    /// The pairing is between a return and the line feed that closes it, and a
    /// bracket comes between them: the terminal sends a whole paste body before
    /// it closes one, so a break on either side of a bracket is its own.
    #[test]
    fn a_line_ending_does_not_pair_across_a_paste_bracket() {
        assert_eq!(typed(&[b"ab\r\x1b[200~\ncd\x1b[201~"]).line(), "\ncd");
        let mut piped_in = piped(true, &[b"\x1b[200~x\r\x1b[201~\nnext\n"]);
        assert_eq!(
            piped_in.take_events(),
            vec![submit("x\n", false), submit("next", false)]
        );
    }

    /// A shift owed from before a paste is not owed to a character inside it,
    /// or the body would pay it and arrive without its first character.
    #[test]
    fn a_shift_does_not_outlive_the_start_of_a_paste() {
        assert_eq!(typed(&[b"\x1bO\x1b[200~Ab\x1b[201~"]).line(), "Ab");
    }

    #[test]
    fn a_pasted_tab_is_a_character_of_the_line() {
        let input = typed(&[b"\x1b[200~if x:\r\tpass\x1b[201~"]);
        assert_eq!(input.line(), "if x:\n\tpass");
    }

    /// A clipboard from Windows or a browser carries CRLF, and the box shows
    /// one break for it — counting two would run the cursor ahead of what the
    /// user is looking at.
    #[test]
    fn a_pasted_crlf_is_one_line_break() {
        let input = typed(&[b"\x1b[200~one\r\ntwo\x1b[201~"]);
        assert_eq!(input.line(), "one\ntwo");
        assert_eq!(input.cursor(), 7);
    }

    #[test]
    fn a_paste_of_several_characters_lands_where_the_cursor_is() {
        let input = typed(&[b"ad\x1b[D\x1b[200~bc\x1b[201~"]);
        assert_eq!(input.line(), "abcd");
        assert_eq!(input.cursor(), 3);
    }

    /// A read ends wherever it ends, so a paste outlives the one it started in
    /// — the opening bracket included.
    #[test]
    fn a_paste_split_across_two_reads_arrives_whole() {
        let mut input = typed(&[b"\x1b[20", b"0~one", b"\rtwo\x1b[201~\r"]);
        assert_eq!(input.take_events(), vec![submit("one\ntwo", false)]);
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

    /// Every character after the first has to move the cursor's byte offset as
    /// well as its char index — a shortcut that keeps only the char index puts
    /// them into the line backwards.
    #[test]
    fn characters_typed_mid_line_go_in_in_order() {
        let input = typed(&[b"a\xe4\xb8\xadd\x1b[Dbc"]);
        assert_eq!(input.line(), "a\u{4e2d}bcd");
        assert_eq!(input.cursor(), 4);
    }

    /// Bulk input arrives one character at a time, so an insertion that costs
    /// the length so far costs the square of the line — and the model is fed on
    /// the thread that forwards the keystrokes.
    #[test]
    fn a_hundred_thousand_characters_go_in_under_a_second() {
        let body = vec![b'x'; 100_000];
        let start = std::time::Instant::now();
        let mut input = Input::new(false);
        for chunk in body.chunks(4096) {
            input.feed(chunk);
        }
        assert_eq!(input.line().len(), body.len());
        let took = start.elapsed();
        assert!(took < std::time::Duration::from_secs(1), "took {took:?}");
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

    /// Alt+Enter on a terminal that does not speak the kitty protocol arrives as
    /// `ESC CR`, and it is the same line break. Read as an escape sequence it
    /// was two faults at once: the carriage return submitted, and the byte
    /// after it was eaten as the sequence's final.
    #[test]
    fn a_legacy_alt_enter_breaks_the_line_and_keeps_what_follows() {
        let mut input = piped(false, &[b"ab\x1b\rcd\r"]);

        assert_eq!(input.take_events(), [submit("ab\ncd", false)]);
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
