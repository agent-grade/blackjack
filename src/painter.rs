use crate::screen::Screen;

const PAINT_OFF: &str = "BLACKJACK_PAINT_OFF";
/// The one misspelling this paint knows. The checker that will find them for
/// real is not wired to it yet, and a fixed word is what makes the drawing
/// itself provable before anything decides what to draw on.
const MISSPELLING: &str = "teh";

/// Underlines drawn over the wrapped program's own screen. They go on after the
/// program has finished a frame, and never into its pty: the program is not
/// told about them, because it did not ask for them and would redraw over them
/// anyway.
pub struct Painter(pub(crate) bool);

impl Painter {
    /// The switch is read once, at the start: a paint that ever fights the
    /// wrapped program for the screen has to be turnable off without a rebuild.
    pub fn open() -> Self {
        Self(std::env::var_os(PAINT_OFF).is_none())
    }

    /// Nothing is drawn while the program has a frame open: what it is drawing
    /// is half done, and a paint closing that frame would present the half.
    pub fn paint(&self, screen: &Screen) -> Option<Vec<u8>> {
        if !self.0 || screen.mid_frame() {
            return None;
        }
        let (row, col) = screen.word_in_box(MISSPELLING)?;
        Some(underline(row, col, MISSPELLING, screen.cursor()))
    }
}

/// A frame of its own, so the terminal shows the underline in one update rather
/// than a cursor jumping about. The cursor is put back by moving it rather than
/// by `ESC 7`/`ESC 8`: a terminal keeps one saved cursor per screen and the
/// program uses that slot itself, so saving here would hand the program this
/// paint's position back as its own. The cost of not using it is that it would
/// have carried the pen too — so the word is re-printed in whatever colour the
/// program had set, and the two attributes this does set are turned off one
/// each because they are two attributes.
fn underline(row: usize, col: usize, word: &str, cursor: (usize, usize)) -> Vec<u8> {
    format!(
        "\x1b[?2026h\x1b[{};{}H\x1b[4:3m\x1b[58:2::255:0:0m{word}\x1b[59m\x1b[24m\x1b[{};{}H\x1b[?2026l",
        row + 1,
        col + 1,
        cursor.0 + 1,
        cursor.1 + 1
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_holding(text: &str) -> Screen {
        let mut screen = Screen::new(20, 3);
        let drawn =
            format!("\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}{text}\r\n\u{2500}\u{2500}\u{2500}");
        crate::parser::Parser::new().advance(&mut screen, drawn.as_bytes());
        screen
    }

    #[test]
    fn a_misspelling_in_the_box_is_underlined_where_it_stands() {
        let paint = Painter(true)
            .paint(&box_holding("a teh word"))
            .expect("a paint");
        assert_eq!(
            String::from_utf8(paint).expect("utf8"),
            "\x1b[?2026h\x1b[2;5H\x1b[4:3m\x1b[58:2::255:0:0mteh\x1b[59m\x1b[24m\x1b[3;4H\x1b[?2026l"
        );
    }

    /// The program is in the middle of drawing: closing a frame it opened would
    /// present half a redraw, so nothing goes out until it closes its own.
    #[test]
    fn nothing_is_drawn_into_a_frame_the_program_has_open() {
        let mut redrawing = box_holding("a teh word");
        crate::parser::Parser::new().advance(&mut redrawing, b"\x1b[?2026h");
        assert!(Painter(true).paint(&redrawing).is_none());
    }

    /// A run of the right letters inside a longer word is not that word.
    #[test]
    fn a_longer_word_holding_the_letters_is_not_underlined() {
        assert!(Painter(true).paint(&box_holding("notehere")).is_none());
    }

    #[test]
    fn a_box_without_the_word_is_not_painted_on() {
        assert!(
            Painter(true)
                .paint(&box_holding("all spelled right"))
                .is_none()
        );
    }

    /// The placeholder the box shows while the line is empty is the program's
    /// own text, not the user's.
    #[test]
    fn a_word_the_box_drew_dim_is_not_underlined() {
        let placeholder = box_holding("\x1b[2mteh\x1b[22m");
        assert!(Painter(true).paint(&placeholder).is_none());
    }

    #[test]
    fn the_switch_turns_the_paint_off_altogether() {
        assert!(Painter(false).paint(&box_holding("a teh word")).is_none());
    }
}
