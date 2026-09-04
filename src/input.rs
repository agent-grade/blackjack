use vte::{Params, Parser, Perform};

use crate::screen::param_as_written;

const ESC: u8 = 0x1b;
const DEL: char = '\u{7f}';
/// The ctrl bit of a kitty key's modifier field.
const CTRL: u16 = 0b100;

/// Recorded on the way past, never applied. `edited` means an edit key was seen
/// on this line; a paste never sets it.
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
                    self.buffer.edit();
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

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.buffer.events)
    }
}

#[derive(Default)]
struct Buffer {
    text: String,
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
        self.events.push(Event::Submit { line, edited });
    }

    fn edit(&mut self) {
        if self.pasting {
            return;
        }
        self.edited = true;
        self.events.push(Event::Edit);
    }
}

impl Perform for Buffer {
    fn print(&mut self, c: char) {
        self.after_return = false;
        if self.pasting || std::mem::take(&mut self.shifted) {
            return;
        }
        if c == DEL {
            self.edit();
            return;
        }
        self.text.push(c);
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
            // Backspace, and the ctrl chords that move the cursor or cut text:
            // a, e, k, u, w.
            0x08 | 0x01 | 0x05 | 0x0b | 0x15 | 0x17 => self.edit(),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        // A marked form is a report rather than a key — `CSI < … M` is the
        // mouse. Every sequence reaching here is skipped whole, which is what
        // keeps a key's final byte out of the text.
        if ignore || !intermediates.is_empty() {
            return;
        }
        match action {
            '~' => match param_as_written(params, 0) {
                200 => self.pasting = true,
                201 => self.pasting = false,
                // Delete, and Home and End in their numbered forms.
                1 | 3 | 4 | 7 | 8 => self.edit(),
                _ => {}
            },
            // Arrows, Home and End as plain keys.
            'A'..='D' | 'H' | 'F' => self.edit(),
            'u' if edits_the_line(params) => self.edit(),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if !self.pasting && intermediates.is_empty() && byte == b'O' {
            self.shifted = true;
        }
    }
}

/// The kitty encoding Claude Code asks for, in which the keys that move the
/// cursor or cut text arrive as their own codepoint rather than as a control
/// byte. Its modifier field is the bitmask plus one.
fn edits_the_line(params: &Params) -> bool {
    let modifiers = param_as_written(params, 1).saturating_sub(1);
    param_as_written(params, 0) == 127
        || (modifiers & CTRL != 0
            && matches!(param_as_written(params, 0), 97 | 101 | 107 | 117 | 119))
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
        let mut input = typed(&[b"ab\x1b[200~pasted\r\x7ftext\x1b[201~cd"]);
        assert_eq!(input.line(), "abcd");
        assert_eq!(input.take_events(), vec![]);
    }

    #[test]
    fn backspace_is_recorded_without_changing_the_line() {
        let mut input = typed(&[b"ab\x7fc"]);
        assert_eq!(input.line(), "abc");
        assert_eq!(input.take_events(), vec![Event::Edit]);
    }

    /// Alt+Backspace is `ESC DEL`, and the parser reads a DEL inside a sequence
    /// as no byte at all — so the character after it would become the escape's
    /// final byte and leave the line without ever being seen.
    #[test]
    fn alt_backspace_does_not_swallow_the_next_character() {
        let mut input = typed(&[b"abc\x1b\x7fd"]);
        assert_eq!(input.line(), "abcd");
        assert_eq!(input.take_events(), vec![Event::Edit]);
    }

    #[test]
    fn an_escape_split_across_two_reads_is_still_alt_backspace() {
        let mut input = typed(&[b"abc\x1b", b"\x7fd"]);
        assert_eq!(input.line(), "abcd");
        assert_eq!(input.take_events(), vec![Event::Edit]);
    }

    /// A DEL inside a sequence is not a keystroke, and the parser drops it — so
    /// reading the byte stream for DELs on its own would invent an edit here.
    #[test]
    fn a_del_inside_a_sequence_is_not_an_edit() {
        let mut input = typed(&[b"ab\x1b[2\x7f7uc"]);
        assert_eq!(input.line(), "abc");
        assert_eq!(input.take_events(), vec![]);
    }

    /// The edits are recorded and not applied, so the buffer is not the line
    /// the user meant to send. Ctrl-U cut everything before `world`.
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
        let mut input = piped(true, &[b"a\x15b\nc\n"]);
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
        assert_eq!(input.take_events(), vec![]);
    }

    #[test]
    fn a_line_that_was_edited_submits_dirty() {
        assert_eq!(
            submitted(&[b"hello\x15world\r"]),
            vec![submit("helloworld", true)]
        );
        assert_eq!(
            submitted(&[b"hello\x1b[117;5uworld\r"]),
            vec![submit("helloworld", true)],
            "the kitty encoding of the same chord"
        );
        assert_eq!(
            submitted(&[b"helloworld\r"]),
            vec![submit("helloworld", false)]
        );
    }

    #[test]
    fn a_recorded_edit_key_is_seen_without_changing_the_line() {
        for edit in [
            &b"\x08"[..],
            b"\x01",
            b"\x05",
            b"\x0b",
            b"\x15",
            b"\x17",
            b"\x1b[A",
            b"\x1b[B",
            b"\x1b[C",
            b"\x1b[D",
            b"\x1b[1~",
            b"\x1b[3~",
            b"\x1b[4~",
            b"\x1b[7~",
            b"\x1b[8~",
            b"\x1b[H",
            b"\x1b[F",
            b"\x1b[97;5u",
            b"\x1b[101;5u",
            b"\x1b[107;5u",
            b"\x1b[117;5u",
            b"\x1b[119;5u",
            b"\x1b[127u",
        ] {
            let mut input = typed(&[b"ab", edit]);
            assert_eq!(input.line(), "ab", "for {edit:?}");
            assert_eq!(input.take_events(), vec![Event::Edit], "for {edit:?}");
        }
    }
}
