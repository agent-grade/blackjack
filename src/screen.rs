use std::sync::{Arc, Mutex};

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

const TAB_STOP: usize = 8;
const ALTERNATE_SCREEN: u16 = 1049;
/// Written into the second cell of a double-width character, so that the cells a
/// character occupies and the columns it covers stay the same count.
const CONTINUATION: char = '\0';

/// Shared because a resize reaches the signal thread, not the one reading the
/// pty, and a grid of the wrong shape is worse than none.
pub type SharedScreen = Arc<Mutex<Screen>>;

/// The grid the wrapped program *believes* it is drawing on. A wrapper is told
/// nothing directly, so this is rebuilt from the program's own output — the only
/// account of the screen there is.
pub struct Screen {
    cols: usize,
    rows: usize,
    cells: Vec<char>,
    row: usize,
    col: usize,
    /// A character written into the last column leaves the cursor sitting on it;
    /// the wrap happens only when the next one arrives. Wrapping eagerly instead
    /// scrolls the screen on every full line the program merely finished.
    wrap_pending: bool,
    saved: (usize, usize),
    /// The inclusive row bounds scrolling is confined to. A TUI sets them to
    /// hold a header or a status line still; scrolling the whole grid instead
    /// corrupts every row outside them, from code that reads as correct.
    margins: (usize, usize),
    /// The main grid, held aside while the program draws on the alternate
    /// screen. Without it that drawing lands on the model of the main screen
    /// and destroys it, with nothing left to restore from.
    main: Option<MainScreen>,
}

/// What `CSI ? 1049 h` puts aside and `CSI ? 1049 l` gives back.
struct MainScreen {
    cells: Vec<char>,
    row: usize,
    col: usize,
    saved: (usize, usize),
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Self {
            cols,
            rows,
            cells: vec![' '; cols * rows],
            row: 0,
            col: 0,
            wrap_pending: false,
            saved: (0, 0),
            margins: (0, rows - 1),
            main: None,
        }
    }

    /// A real terminal reflows wrapped lines instead of keeping coordinates, but
    /// the program redraws after a resize anyway — the grid only has to stay the
    /// right shape until it does.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        self.cells = reshape(&self.cells, (self.cols, self.rows), (cols, rows));
        if let Some(main) = &mut self.main {
            main.cells = reshape(&main.cells, (self.cols, self.rows), (cols, rows));
        }
        self.cols = cols;
        self.rows = rows;
        self.row = self.row.min(rows - 1);
        self.col = self.col.min(cols - 1);
        self.wrap_pending = false;
        // A margin from the old shape can name a row this one does not have,
        // and a terminal drops the region on a resize anyway.
        self.margins = (0, rows - 1);
    }

    /// Zero-based `(row, column)`, in display columns rather than characters.
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// The row's text. A double-width character holds one cell here and two
    /// columns on screen, so an index into this is not a column — painting
    /// reads cells.
    pub fn line(&self, row: usize) -> String {
        let start = row.min(self.rows - 1) * self.cols;
        let text: String = self.cells[start..start + self.cols]
            .iter()
            .filter(|&&cell| cell != CONTINUATION)
            .collect();
        text.trim_end().to_string()
    }

    pub fn render(&self) -> String {
        (0..self.rows)
            .map(|row| self.line(row))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn index(&mut self) {
        let (top, bottom) = self.margins;
        if self.row == bottom {
            let (start, end) = self.region(top, bottom);
            self.cells[start..end].rotate_left(self.cols);
            self.cells[end - self.cols..end].fill(' ');
        } else if self.row + 1 < self.rows {
            // Nothing scrolls a cursor parked below the region off the last row.
            self.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        let (top, bottom) = self.margins;
        if self.row == top {
            let (start, end) = self.region(top, bottom);
            self.cells[start..end].rotate_right(self.cols);
            self.cells[start..start + self.cols].fill(' ');
        } else if self.row > 0 {
            self.row -= 1;
        }
    }

    fn region(&self, top: usize, bottom: usize) -> (usize, usize) {
        (top * self.cols, (bottom + 1) * self.cols)
    }

    fn set_margins(&mut self, params: &Params) {
        let top = count(params, 0) - 1;
        let bottom = match param_as_written(params, 1) {
            0 => self.rows - 1,
            row => (row as usize - 1).min(self.rows - 1),
        };
        // A region a terminal would reject leaves the current one standing.
        if top >= bottom {
            return;
        }
        self.margins = (top, bottom);
        self.move_to(0, 0);
    }

    fn enter_alternate(&mut self) {
        if self.main.is_some() {
            return;
        }
        // The cursor stays where it was: the program positions itself before it
        // draws on a screen that starts empty.
        let blank = vec![' '; self.cols * self.rows];
        self.main = Some(MainScreen {
            cells: std::mem::replace(&mut self.cells, blank),
            row: self.row,
            col: self.col,
            saved: std::mem::take(&mut self.saved),
        });
    }

    fn leave_alternate(&mut self) {
        let Some(main) = self.main.take() else {
            return;
        };
        self.cells = main.cells;
        self.saved = main.saved;
        self.move_to(main.row, main.col);
    }

    fn move_to(&mut self, row: usize, col: usize) {
        self.row = row.min(self.rows - 1);
        self.col = col.min(self.cols - 1);
        self.wrap_pending = false;
    }

    fn erase_in_display(&mut self, mode: u16) {
        let at = self.row * self.cols + self.col;
        match mode {
            0 => self.cells[at..].fill(' '),
            1 => self.cells[..=at].fill(' '),
            // 3 also drops the scrollback, which a model of the visible grid
            // does not keep.
            2 | 3 => self.cells.fill(' '),
            _ => {}
        }
    }

    fn erase_in_line(&mut self, mode: u16) {
        let start = self.row * self.cols;
        let at = start + self.col;
        let end = start + self.cols;
        match mode {
            0 => self.cells[at..end].fill(' '),
            1 => self.cells[start..=at].fill(' '),
            2 => self.cells[start..end].fill(' '),
            _ => {}
        }
    }
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        // A zero-width mark needs a cell to combine into, which a grid of plain
        // chars has nowhere to keep. Dropping it loses the accent and holds the
        // columns; writing it would shift the rest of the row.
        let Some(width) = c.width().filter(|&width| width > 0) else {
            return;
        };
        if self.wrap_pending || self.col + width > self.cols {
            self.col = 0;
            self.index();
            self.wrap_pending = false;
        }
        let at = self.row * self.cols + self.col;
        self.cells[at] = c;
        for continued in 1..width {
            self.cells[at + continued] = CONTINUATION;
        }
        if self.col + width == self.cols {
            self.wrap_pending = true;
        } else {
            self.col += width;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => self.col = self.col.saturating_sub(1),
            0x09 => self.col = ((self.col / TAB_STOP + 1) * TAB_STOP).min(self.cols - 1),
            0x0a..=0x0c => self.index(),
            0x0d => self.col = 0,
            // BEL and the rest move no cursor, so a wrap they interrupt is
            // still owed to the next character.
            _ => return,
        }
        self.wrap_pending = false;
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        // `ignore` is vte's own verdict on a sequence past 32 parameters, and a
        // terminal discards those.
        if ignore {
            return;
        }
        // A private marker makes a different sequence of the same final byte —
        // `CSI ? J` is the selective erase — and of the modes a TUI toggles on
        // every redraw, only the alternate screen moves cells.
        if intermediates == b"?" {
            if params
                .iter()
                .any(|param| param.first() == Some(&ALTERNATE_SCREEN))
            {
                match action {
                    'h' => self.enter_alternate(),
                    'l' => self.leave_alternate(),
                    _ => {}
                }
            }
            return;
        }
        // An intermediate does the same — `CSI SP A` scrolls right where
        // `CSI A` moves up.
        if !intermediates.is_empty() {
            return;
        }
        match action {
            'A' => self.move_to(self.row.saturating_sub(count(params, 0)), self.col),
            'B' => self.move_to(self.row + count(params, 0), self.col),
            'C' => self.move_to(self.row, self.col + count(params, 0)),
            'D' => self.move_to(self.row, self.col.saturating_sub(count(params, 0))),
            'E' => self.move_to(self.row + count(params, 0), 0),
            'F' => self.move_to(self.row.saturating_sub(count(params, 0)), 0),
            'G' => self.move_to(self.row, count(params, 0) - 1),
            'd' => self.move_to(count(params, 0) - 1, self.col),
            'H' | 'f' => self.move_to(count(params, 0) - 1, count(params, 1) - 1),
            'J' => self.erase_in_display(param_as_written(params, 0)),
            'K' => self.erase_in_line(param_as_written(params, 0)),
            'r' => self.set_margins(params),
            's' => self.saved = (self.row, self.col),
            'u' => self.move_to(self.saved.0, self.saved.1),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if !intermediates.is_empty() {
            return;
        }
        match byte {
            b'D' => {
                self.index();
                self.wrap_pending = false;
            }
            b'E' => {
                self.index();
                self.move_to(self.row, 0);
            }
            b'M' => {
                self.reverse_index();
                self.wrap_pending = false;
            }
            b'7' => self.saved = (self.row, self.col),
            b'8' => self.move_to(self.saved.0, self.saved.1),
            _ => {}
        }
    }
}

fn reshape(cells: &[char], from: (usize, usize), to: (usize, usize)) -> Vec<char> {
    let ((old_cols, old_rows), (cols, rows)) = (from, to);
    let mut reshaped = vec![' '; cols * rows];
    for row in 0..rows.min(old_rows) {
        for col in 0..cols.min(old_cols) {
            reshaped[row * cols + col] = cells[row * old_cols + col];
        }
    }
    reshaped
}

/// A parameter as written, so that a sequence whose zero means something (the
/// erase modes) can tell it from an omitted one.
pub fn param_as_written(params: &Params, index: usize) -> u16 {
    params
        .iter()
        .nth(index)
        .and_then(|param| param.first().copied())
        .unwrap_or(0)
}

/// Counts and one-based positions read an absent parameter and an explicit zero
/// alike, as one.
fn count(params: &Params, index: usize) -> usize {
    match param_as_written(params, index) {
        0 => 1,
        n => n as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(cols: usize, rows: usize, bytes: &[u8]) -> Screen {
        let mut screen = Screen::new(cols, rows);
        crate::parser::Parser::new().advance(&mut screen, bytes);
        screen
    }

    #[test]
    fn writes_text_and_leaves_the_cursor_after_it() {
        let screen = screen(10, 3, b"hi");
        assert_eq!(screen.line(0), "hi");
        assert_eq!(screen.cursor(), (0, 2));
    }

    #[test]
    fn wraps_when_a_character_follows_the_last_column_and_not_before() {
        let filled = screen(4, 3, b"abcd");
        assert_eq!(filled.cursor(), (0, 3));
        assert_eq!(filled.line(0), "abcd");

        let overflowed = screen(4, 3, b"abcde");
        assert_eq!(overflowed.cursor(), (1, 1));
        assert_eq!(overflowed.line(1), "e");
    }

    /// The cursor is a column, not a character count — the quantity painting
    /// will consume. A CJK ideograph covers two of them.
    #[test]
    fn a_double_width_character_covers_two_columns() {
        let screen = screen(10, 2, "中文abc".as_bytes());
        assert_eq!(screen.cursor(), (0, 7));
        assert_eq!(screen.line(0), "中文abc");
    }

    #[test]
    fn a_double_width_character_wraps_rather_than_straddling_the_edge() {
        let screen = screen(3, 2, "ab中".as_bytes());
        assert_eq!(screen.line(0), "ab");
        assert_eq!(screen.line(1), "中");
        assert_eq!(screen.cursor(), (1, 2));
    }

    #[test]
    fn backspace_moves_without_erasing() {
        let screen = screen(10, 3, b"abc\x08X");
        assert_eq!(screen.line(0), "abX");
    }

    #[test]
    fn carriage_return_overwrites_from_the_start_of_the_line() {
        let screen = screen(10, 3, b"hello\rbye");
        assert_eq!(screen.line(0), "byelo");
    }

    #[test]
    fn a_newline_at_the_bottom_scrolls_the_grid() {
        let screen = screen(10, 2, b"one\r\ntwo\r\nthree");
        assert_eq!(screen.line(0), "two");
        assert_eq!(screen.line(1), "three");
        assert_eq!(screen.cursor(), (1, 5));
    }

    #[test]
    fn absolute_positioning_is_one_based() {
        let screen = screen(10, 4, b"\x1b[3;5Hx");
        assert_eq!(screen.cursor(), (2, 5));
        assert_eq!(screen.line(2), "    x");
    }

    #[test]
    fn a_bare_position_goes_home() {
        let screen = screen(10, 4, b"\r\n\r\nlow\x1b[Hx");
        assert_eq!(screen.line(0), "x");
        assert_eq!(screen.line(2), "low");
    }

    #[test]
    fn erases_from_the_cursor_to_the_end_of_the_line() {
        let screen = screen(10, 2, b"abcdef\x1b[3G\x1b[K");
        assert_eq!(screen.line(0), "ab");
    }

    /// What a spinner emits on every frame.
    #[test]
    fn erases_the_whole_line() {
        let screen = screen(10, 2, b"working\x1b[2K\x1b[1Gdone");
        assert_eq!(screen.line(0), "done");
    }

    #[test]
    fn erases_the_start_of_the_line_up_to_the_cursor() {
        let screen = screen(10, 2, b"abcdef\x1b[3G\x1b[1K");
        assert_eq!(screen.line(0), "   def");
    }

    #[test]
    fn erases_from_the_cursor_to_the_end_of_the_display() {
        let screen = screen(10, 3, b"one\r\ntwo\r\nsix\x1b[2;2H\x1b[J");
        assert_eq!(screen.line(0), "one");
        assert_eq!(screen.line(1), "t");
        assert_eq!(screen.line(2), "");
    }

    #[test]
    fn erases_the_display_up_to_the_cursor() {
        let screen = screen(10, 3, b"one\r\ntwo\r\nsix\x1b[2;2H\x1b[1J");
        assert_eq!(screen.line(0), "");
        assert_eq!(screen.line(1), "  o");
        assert_eq!(screen.line(2), "six");
    }

    #[test]
    fn erases_the_whole_display() {
        let screen = screen(10, 2, b"abc\r\ndef\x1b[2J");
        assert_eq!(screen.render(), "\n");
    }

    /// A marker or an intermediate makes a different sequence out of the same
    /// final byte, and a TUI emits these among the mode toggles it sets on every
    /// redraw.
    #[test]
    fn a_marked_sequence_is_not_the_plain_one_it_ends_like() {
        let screen = screen(10, 3, b"ab\x1b[?25l\x1b[?2004h\x1b[2;3H\x1b[ A\x1b[2 qc");
        assert_eq!(screen.cursor(), (1, 3));
        assert_eq!(screen.line(0), "ab", "a mode toggle moved cells");
        assert_eq!(screen.line(1), "  c");
    }

    /// The one private mode that does move cells. Claude Code emits it, and
    /// without it the alt-screen drawing lands on the main model and stays.
    #[test]
    fn the_alternate_screen_keeps_its_drawing_off_the_main_grid() {
        let screen = screen(10, 3, b"main text\x1b[?1049h\x1b[2Jfull\x1b[?1049l");
        assert_eq!(screen.line(0), "main text");
        assert_eq!(screen.cursor(), (0, 9));
    }

    #[test]
    fn the_alternate_screen_starts_empty() {
        let screen = screen(10, 2, b"main\x1b[?1049h");
        assert_eq!(screen.render(), "\n");
    }

    /// A program clearing its modes at startup emits the exit with no entry.
    #[test]
    fn leaving_an_alternate_screen_never_entered_changes_nothing() {
        let screen = screen(10, 2, b"main\x1b[?1049l");
        assert_eq!(screen.line(0), "main");
    }

    /// The save slot travels with the buffer, as it does in xterm: an editor
    /// saving its own cursor on the alternate screen must not be what the
    /// shell's restore finds waiting for it.
    #[test]
    fn a_cursor_saved_on_the_alternate_screen_stays_there() {
        let screen = screen(
            10,
            4,
            b"\x1b[3;1H\x1b7\x1b[?1049h\x1b[1;5H\x1b7\x1b[?1049l\x1b8x",
        );
        assert_eq!(screen.line(2), "x");
        assert_eq!(screen.cursor(), (2, 1));
    }

    /// Nothing scrolls a cursor below the region, so the row must stay on the
    /// grid on its own — a TUI holding a status line puts it exactly there.
    #[test]
    fn a_wrap_below_the_scroll_region_stays_on_the_last_row() {
        let screen = screen(4, 4, b"\x1b[1;3r\x1b[4;1Habcde");
        assert_eq!(screen.line(3), "ebcd", "the wrap left the last row");
        assert_eq!(screen.cursor(), (3, 1));
    }

    #[test]
    fn a_reverse_index_above_the_scroll_region_stays_on_the_first_row() {
        let screen = screen(10, 4, b"\x1b[2;4r\x1bMx");
        assert_eq!(screen.line(0), "x");
        assert_eq!(screen.cursor(), (0, 1));
    }

    /// One `CSI ? … h` may carry several modes; tmux sets its mouse modes that
    /// way.
    #[test]
    fn a_combined_mode_set_still_finds_the_alternate_screen() {
        let screen = screen(10, 2, b"main\x1b[?1000;1049h");
        assert_eq!(screen.render(), "\n");
    }

    #[test]
    fn a_second_entry_does_not_displace_the_saved_main_grid() {
        let screen = screen(
            10,
            2,
            b"main\x1b[?1049h\x1b[2Jfirst\x1b[?1049h\x1b[2Jsecond\x1b[?1049l",
        );
        assert_eq!(screen.line(0), "main");
    }

    /// The saved grid is a grid too: kept at the old shape, restoring it reads
    /// past the end of a screen that has since grown.
    #[test]
    fn a_resize_reaches_the_grid_held_behind_the_alternate_screen() {
        let mut screen = screen(10, 2, b"mainline\x1b[?1049h");
        screen.resize(20, 3);
        crate::parser::Parser::new().advance(&mut screen, b"\x1b[?1049l");
        assert_eq!(screen.line(0), "mainline");
        assert_eq!(screen.line(2), "");
    }

    /// `CSI 2;3r` over four rows: a line feed at the bottom of the region
    /// scrolls those two rows and leaves the header and footer standing.
    #[test]
    fn a_scroll_region_scrolls_only_its_own_rows() {
        let screen = screen(
            10,
            4,
            b"\x1b[2;3rhead\x1b[2;1Hone\x1b[3;1Htwo\x1b[4;1Hfoot\x1b[3;4H\n",
        );
        assert_eq!(screen.line(0), "head");
        assert_eq!(screen.line(1), "two");
        assert_eq!(screen.line(2), "");
        assert_eq!(screen.line(3), "foot");
        assert_eq!(screen.cursor(), (2, 3));
    }

    #[test]
    fn a_reverse_index_scrolls_only_the_region_it_is_in() {
        let screen = screen(
            10,
            4,
            b"\x1b[2;3rhead\x1b[2;1Hone\x1b[3;1Htwo\x1b[4;1Hfoot\x1b[2;1H\x1bM",
        );
        assert_eq!(screen.line(0), "head");
        assert_eq!(screen.line(1), "");
        assert_eq!(screen.line(2), "one");
        assert_eq!(screen.line(3), "foot");
    }

    #[test]
    fn setting_a_scroll_region_homes_the_cursor() {
        let screen = screen(10, 4, b"\x1b[4;5H\x1b[2;3rx");
        assert_eq!(screen.line(0), "x");
    }

    #[test]
    fn an_inverted_region_leaves_the_whole_screen_scrolling() {
        let screen = screen(10, 3, b"\x1b[3;2rone\r\ntwo\r\nthree\r\nfour");
        assert_eq!(screen.line(0), "two");
        assert_eq!(screen.line(2), "four");
    }

    #[test]
    fn a_bare_region_reset_restores_the_whole_screen() {
        let screen = screen(10, 3, b"\x1b[2;3r\x1b[rone\r\ntwo\r\nthree\r\nfour");
        assert_eq!(screen.line(0), "two");
        assert_eq!(screen.line(2), "four");
    }

    /// A region naming rows the new shape does not have would otherwise sit
    /// there unreachable, and nothing would ever scroll again.
    #[test]
    fn a_resize_drops_the_scroll_region() {
        let mut screen = screen(10, 4, b"\x1b[3;4r");
        screen.resize(10, 2);
        crate::parser::Parser::new().advance(&mut screen, b"\x1b[1;1Ha\r\nb\r\nc");
        assert_eq!(screen.line(0), "b");
        assert_eq!(screen.line(1), "c");
    }

    /// vte reports a sequence past 32 parameters by setting `ignore`, with no
    /// intermediate to give it away — and a real terminal discards it.
    #[test]
    fn an_overlong_sequence_is_discarded_the_way_a_terminal_discards_it() {
        let overlong = format!("\x1b[3;3H\x1b[{}1Hx", "1;".repeat(33));
        let screen = screen(10, 4, overlong.as_bytes());
        assert_eq!(
            screen.cursor(),
            (2, 3),
            "the discarded jump home was obeyed"
        );
        assert_eq!(screen.line(2), "  x");
    }

    #[test]
    fn returns_to_a_saved_cursor() {
        let screen = screen(10, 3, b"\x1b[2;3H\x1b7\x1b[1;1Hhome\x1b8back");
        assert_eq!(screen.line(0), "home");
        assert_eq!(screen.line(1), "  back");
    }

    #[test]
    fn returns_to_a_cursor_saved_by_the_csi_form() {
        let screen = screen(10, 3, b"\x1b[2;3H\x1b[s\x1b[1;1Hhome\x1b[uback");
        assert_eq!(screen.line(1), "  back");
    }

    #[test]
    fn a_reverse_index_at_the_top_scrolls_the_other_way() {
        let screen = screen(10, 2, b"top\r\nbottom\x1b[1;1H\x1bM");
        assert_eq!(screen.line(0), "");
        assert_eq!(screen.line(1), "top");
    }

    #[test]
    fn a_next_line_starts_the_row_below_at_the_left_margin() {
        let screen = screen(10, 3, b"\x1b[1;5Habc\x1bEx");
        assert_eq!(screen.line(1), "x");
    }

    #[test]
    fn tab_stops_every_eighth_column() {
        let screen = screen(20, 2, b"a\tb\tc");
        assert_eq!(screen.line(0), "a       b       c");
    }

    /// A real Claude Code frame, captured through the wrapper at 40x100: the
    /// bytes are what the terminal saw. The pinned rows are the ones a painter
    /// has to find — the header drawn once, the input line redrawn on every
    /// keystroke, and the mode line under it.
    #[test]
    fn reconstructs_a_captured_claude_code_frame() {
        let mut screen = Screen::new(100, 40);
        crate::parser::Parser::new().advance(
            &mut screen,
            include_bytes!("../tests/fixtures/claude-tui-40x100.bin"),
        );
        assert_eq!(screen.line(1), " ▐▛███▛█   Claude Code v2.1.259");
        assert_eq!(screen.line(35), "─".repeat(100));
        assert_eq!(screen.line(36), "❯");
        assert_eq!(screen.line(37), "─".repeat(100));
        assert_eq!(
            screen.line(39),
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents"
        );
        assert_eq!(screen.cursor(), (36, 2));
    }

    #[test]
    fn resizing_keeps_what_still_fits() {
        let mut screen = screen(10, 3, b"abcdefgh\r\nsecond");
        screen.resize(4, 2);
        assert_eq!(screen.line(0), "abcd");
        assert_eq!(screen.line(1), "seco");
        assert_eq!(screen.cursor(), (1, 3));
    }
}
