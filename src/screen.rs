use std::sync::{Arc, Mutex};

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

const TAB_STOP: usize = 8;
const ALTERNATE_SCREEN: u16 = 1049;
const SYNCHRONIZED_OUTPUT: u16 = 2026;
/// What Claude Code puts in front of the line being typed, and the two columns
/// it costs — on the rows continuing it too, which are indented to match.
const PROMPT: [char; 2] = ['\u{276f}', '\u{a0}'];
/// The box is drawn with the same character above the prompt row and below the
/// last row of the line, which is what says where it ends.
const BORDER: char = '\u{2500}';
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
    /// Whether each cell was written dim. Claude Code draws its placeholder
    /// that way, and it is the only thing the capture shows dim inside the box,
    /// so this is what tells a hint the program is showing from a typed line.
    dim: Vec<bool>,
    /// Whether what is printed now is dim.
    dimmed: bool,
    row: usize,
    col: usize,
    /// A character written into the last column leaves the cursor sitting on it;
    /// the wrap happens only when the next one arrives. Wrapping eagerly instead
    /// scrolls the screen on every full line the program merely finished.
    wrap_pending: bool,
    saved: (usize, usize, bool),
    /// The inclusive row bounds scrolling is confined to. A TUI sets them to
    /// hold a header or a status line still; scrolling the whole grid instead
    /// corrupts every row outside them, from code that reads as correct.
    margins: (usize, usize),
    /// The main grid, held aside while the program draws on the alternate
    /// screen. Without it that drawing lands on the model of the main screen
    /// and destroys it, with nothing left to restore from.
    main: Option<MainScreen>,
    /// How many synchronized-output frames the program has finished. A redraw
    /// lands between them, so this is the only moment the grid is worth reading.
    frames: u64,
    /// Whether the program has a frame open. Synchronized output is a flag and
    /// not a count, so anything written between the open and the close ends the
    /// program's frame instead of its own — tearing a half-drawn redraw onto
    /// the screen.
    mid_frame: bool,
    /// The input line as the frame counted above left it. A pty read ends
    /// wherever the kernel filled the buffer, so a reader that looks when its
    /// own read is done sees a half-drawn box; this is taken at the boundary.
    frame_line: Option<Line>,
}

/// The line the box is showing: its text, where the cursor sits in it counted
/// in characters, and where each of those characters is on the grid. A break
/// the box drew stands for a row ending and has no cell of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub cursor: usize,
    pub spots: Vec<Option<(usize, usize)>>,
}

/// What `CSI ? 1049 h` puts aside and `CSI ? 1049 l` gives back.
struct MainScreen {
    cells: Vec<char>,
    dim: Vec<bool>,
    dimmed: bool,
    row: usize,
    col: usize,
    saved: (usize, usize, bool),
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Self {
            cols,
            rows,
            cells: vec![' '; cols * rows],
            dim: vec![false; cols * rows],
            dimmed: false,
            row: 0,
            col: 0,
            wrap_pending: false,
            saved: (0, 0, false),
            margins: (0, rows - 1),
            main: None,
            frames: 0,
            mid_frame: false,
            frame_line: None,
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
        let (from, to) = ((self.cols, self.rows), (cols, rows));
        self.cells = reshape(&self.cells, ' ', from, to);
        self.dim = reshape(&self.dim, false, from, to);
        if let Some(main) = &mut self.main {
            main.cells = reshape(&main.cells, ' ', from, to);
            main.dim = reshape(&main.dim, false, from, to);
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

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn mid_frame(&self) -> bool {
        self.mid_frame
    }

    /// What `input_row` said at the end of the last frame, which is the only
    /// moment the box is whole. Its line breaks are the rows the box drew and
    /// not yet the ones the user typed — only the line being replaced can tell
    /// those apart, so `Input` is where they are read.
    pub fn frame_line(&self) -> Option<Line> {
        self.frame_line.clone()
    }

    /// The line being typed as the program is showing it, and where in it the
    /// cursor sits, counted in characters. The box moves down the screen as the
    /// tip above it changes, so it is found by the prompt standing under a
    /// border rather than by row, and it ends at the border under its last row.
    /// A prompt with no border under it is a box drawn to somewhere this grid
    /// does not reach, and reading it would take the rows below for the line.
    /// A cursor outside the box goes to the nearer end of the line, which is
    /// the most that can be said of it.
    fn read_box(&self) -> Option<Line> {
        let (first, last) = self.box_rows()?;
        let mut text = String::new();
        let mut spots = Vec::new();
        let mut cursor = None;
        for row in first..=last {
            // The box lays its rows out inside a width of its own — the
            // capture wraps at 98 columns of a 100-column terminal — so where
            // a row ends says nothing about why. Every row after the first
            // offers a break, and the line being replaced decides what it was.
            if row > first {
                text.push('\n');
                spots.push(None);
            }
            let mut end = self.written_to(row);
            if row == self.row {
                cursor = Some(text.chars().count() + self.chars_before(row, self.col));
                // Everything up to the cursor was typed, a trailing space
                // included; past it the row is the box's own padding.
                end = end.max(self.col);
            }
            let at = row * self.cols;
            for col in PROMPT.len()..end {
                if let Some(character) = self.char_at(at + col) {
                    text.push(character);
                    spots.push(Some((row, col)));
                }
            }
        }
        let end_of_line = text.chars().count();
        Some(Line {
            cursor: cursor.unwrap_or(if self.row < first { 0 } else { end_of_line }),
            text,
            spots,
        })
    }

    /// The first and last row of the box: the prompt standing under a border,
    /// down to the last row holding anything — or the one the cursor is on,
    /// which belongs to the line even while it holds nothing.
    fn box_rows(&self) -> Option<(usize, usize)> {
        let first = (1..self.rows)
            .rev()
            .find(|&row| self.starts_with_prompt(row) && self.is_border(row - 1))?;
        let below = (first + 1..self.rows).find(|&row| self.is_border(row))?;
        let last = (first..below)
            .rev()
            .find(|&row| row == self.row || self.written_to(row) > PROMPT.len())
            .unwrap_or(first);
        Some((first, last))
    }

    fn starts_with_prompt(&self, row: usize) -> bool {
        let at = row * self.cols;
        self.cols > PROMPT.len() && self.cells[at..at + PROMPT.len()] == PROMPT
    }

    /// A row drawn as an edge of the box — which a row of untouched blanks,
    /// holding no border character at all, is not. Both captures draw both
    /// edges as unbroken runs with no corner pieces, so a box that grew corners
    /// would not be found at all rather than found wrongly.
    fn is_border(&self, row: usize) -> bool {
        let cells = &self.cells[row * self.cols..(row + 1) * self.cols];
        cells.contains(&BORDER) && cells.iter().all(|&cell| cell == BORDER || cell == ' ')
    }

    /// One column past the last one a row holds anything in. A dim cell holds
    /// the placeholder Claude Code draws in an empty box, which is not text of
    /// the line, and the second cell of a double-width character is not blank.
    fn written_to(&self, row: usize) -> usize {
        let at = row * self.cols;
        (0..self.cols)
            .rev()
            .find(|&col| !self.dim[at + col] && self.cells[at + col] != ' ')
            .map_or(0, |col| col + 1)
    }

    /// What a cell holds as text of the line: a dim cell is the placeholder,
    /// which stands in for a blank, and the second cell of a double-width
    /// character is held by the first.
    fn char_at(&self, cell: usize) -> Option<char> {
        match self.cells[cell] {
            CONTINUATION => None,
            _ if self.dim[cell] => Some(' '),
            character => Some(character),
        }
    }

    /// How many characters of a row stand before a column. A double-width
    /// character holds two cells and counts once.
    fn chars_before(&self, row: usize, col: usize) -> usize {
        let at = row * self.cols;
        (PROMPT.len()..col)
            .filter(|&col| self.cells[at + col] != CONTINUATION)
            .count()
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
            self.dim[start..end].rotate_left(self.cols);
            self.erase(end - self.cols..end);
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
            self.dim[start..end].rotate_right(self.cols);
            self.erase(start..start + self.cols);
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
        let unlit = vec![false; self.cols * self.rows];
        self.main = Some(MainScreen {
            cells: std::mem::replace(&mut self.cells, blank),
            dim: std::mem::replace(&mut self.dim, unlit),
            dimmed: std::mem::take(&mut self.dimmed),
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
        self.dim = main.dim;
        self.dimmed = main.dimmed;
        self.saved = main.saved;
        self.move_to(main.row, main.col);
    }

    /// A terminal saves the pen with the cursor, so a program that draws dim
    /// between the two gets its own attribute back and not the model's.
    fn save_cursor(&mut self) {
        self.saved = (self.row, self.col, self.dimmed);
    }

    fn restore_cursor(&mut self) {
        let (row, col, dimmed) = self.saved;
        self.dimmed = dimmed;
        self.move_to(row, col);
    }

    fn move_to(&mut self, row: usize, col: usize) {
        self.row = row.min(self.rows - 1);
        self.col = col.min(self.cols - 1);
        self.wrap_pending = false;
    }

    fn erase_in_display(&mut self, mode: u16) {
        let at = self.row * self.cols + self.col;
        match mode {
            0 => self.erase(at..self.cells.len()),
            1 => self.erase(0..at + 1),
            // 3 also drops the scrollback, which a model of the visible grid
            // does not keep.
            2 | 3 => self.erase(0..self.cells.len()),
            _ => {}
        }
    }

    fn erase_in_line(&mut self, mode: u16) {
        let start = self.row * self.cols;
        let at = start + self.col;
        let end = start + self.cols;
        match mode {
            0 => self.erase(at..end),
            1 => self.erase(start..at + 1),
            2 => self.erase(start..end),
            _ => {}
        }
    }

    /// Both planes are written together wherever either is, so that a cell's
    /// dim never outlives the character it was set for.
    fn erase(&mut self, cells: std::ops::Range<usize>) {
        self.cells[cells.clone()].fill(' ');
        self.dim[cells].fill(false);
    }

    /// Only the one attribute the input line is read through. A reset and an
    /// explicit undim both end it, and either can ride in a sequence that sets
    /// a colour in the same breath.
    fn select_graphics(&mut self, params: &Params) {
        let mut rest = params.iter();
        while let Some(attribute) = rest.next() {
            match attribute {
                [0] | [22] => self.dimmed = false,
                [2] => self.dimmed = true,
                // A colour spells itself out in the parameters after it, and
                // the first of those is a 2 for a true colour — which is not
                // the dim attribute, and would latch it on for the whole frame.
                [38 | 48 | 58] => {
                    let spelling = match rest.next() {
                        Some([5]) => 1,
                        Some([2]) => 3,
                        _ => 0,
                    };
                    for _ in 0..spelling {
                        rest.next();
                    }
                }
                _ => {}
            }
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
        self.dim[at] = self.dimmed;
        for continued in 1..width {
            self.cells[at + continued] = CONTINUATION;
            self.dim[at + continued] = self.dimmed;
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
            let mode = |wanted| params.iter().any(|param| param.first() == Some(&wanted));
            if mode(ALTERNATE_SCREEN) {
                match action {
                    'h' => self.enter_alternate(),
                    'l' => self.leave_alternate(),
                    _ => {}
                }
            }
            if mode(SYNCHRONIZED_OUTPUT) {
                self.mid_frame = action == 'h';
                if action == 'l' {
                    self.frames += 1;
                    self.frame_line = self.read_box();
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
            'm' => self.select_graphics(params),
            'r' => self.set_margins(params),
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
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
            b'7' => self.save_cursor(),
            b'8' => self.restore_cursor(),
            _ => {}
        }
    }
}

fn reshape<T: Copy>(cells: &[T], blank: T, from: (usize, usize), to: (usize, usize)) -> Vec<T> {
    let ((old_cols, old_rows), (cols, rows)) = (from, to);
    let mut reshaped = vec![blank; cols * rows];
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

    /// The two halves of the box the assertions below are about.
    fn input_row(screen: &Screen) -> Option<(String, usize)> {
        screen.read_box().map(|line| (line.text, line.cursor))
    }

    fn screen(cols: usize, rows: usize, bytes: &[u8]) -> Screen {
        let mut screen = Screen::new(cols, rows);
        crate::parser::Parser::new().advance(&mut screen, bytes);
        screen
    }

    /// An input box as Claude Code draws one: the prompt and the rows
    /// continuing it, between the two borders that say where it ends.
    fn boxed(cols: usize, rows: usize, lines: &[&str], then: &str) -> Screen {
        let border = "\u{2500}".repeat(3);
        let mut bytes = border.clone();
        for (row, line) in lines.iter().enumerate() {
            let prompt = if row == 0 { "\u{276f}\u{a0}" } else { "" };
            bytes += &format!("\r\n{prompt}{line}");
        }
        bytes += &format!("\r\n{border}{then}");
        screen(cols, rows, bytes.as_bytes())
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

    /// Every line the capture's own frames put anything in the box for, in
    /// order — the model read out of a real program's redraws rather than of
    /// bytes written by hand to exercise it.
    fn captured_lines() -> Vec<(String, usize)> {
        let mut screen = Screen::new(100, 40);
        let mut parser = crate::parser::Parser::new();
        let mut rest = include_bytes!("../tests/fixtures/claude-tui-40x100.bin").as_slice();
        let (mut frames, mut lines) = (0, Vec::new());
        while !rest.is_empty() {
            parser.advance(&mut screen, &rest[..1]);
            rest = &rest[1..];
            if screen.frames() != frames {
                frames = screen.frames();
                if let Some((line, cursor)) =
                    input_row(&screen).filter(|(line, _)| !line.is_empty())
                {
                    lines.push((line, cursor));
                }
            }
        }
        lines
    }

    /// The captured session typed a line, went back two characters, and put a
    /// phrase with two double-width characters in at that point — so the last
    /// frame holding anything carries the tail of the first past the insertion.
    #[test]
    fn the_captured_frames_reconstruct_the_line_that_was_typed() {
        let lines = captured_lines();
        assert_eq!(
            lines.first().map(|(line, cursor)| (line.as_str(), *cursor)),
            Some(("hello there, just a capture run", 31))
        );
        assert_eq!(
            lines.last().map(|(line, cursor)| (line.as_str(), *cursor)),
            Some((
                "hello there, just a c with a \u{4e2d}\u{6587} wide wordap",
                41
            ))
        );
    }

    /// The first frame of the same capture, where the box is empty and holds
    /// the placeholder. Nothing has been typed, so the line is empty — a
    /// submit at this moment must not carry the hint the user is being shown.
    #[test]
    fn the_first_captured_frame_shows_a_placeholder_and_an_empty_line() {
        let mut screen = Screen::new(100, 40);
        let mut parser = crate::parser::Parser::new();
        let mut rest = include_bytes!("../tests/fixtures/claude-tui-40x100.bin").as_slice();
        while screen.frames() == 0 && !rest.is_empty() {
            parser.advance(&mut screen, &rest[..1]);
            rest = &rest[1..];
        }
        assert_eq!(
            screen.line(36),
            "\u{276f}\u{a0}Try \"write a test for <filepath>\""
        );
        assert_eq!(input_row(&screen), Some((String::new(), 0)));
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
        assert_eq!(input_row(&screen), Some((String::new(), 0)));
    }

    /// The box is found by its prompt and not by its row — it moves down the
    /// screen as the tip above it grows — and it ends at the border drawn under
    /// it, so the mode line below is not part of the line being typed.
    #[test]
    fn the_input_row_is_the_prompt_row_and_the_rows_continuing_it() {
        let screen = boxed(20, 5, &["one", "  two"], "\r\nmode");
        assert_eq!(input_row(&screen), Some(("one\ntwo".into(), 7)));
    }

    /// The cursor is a column on the grid and a character index in the line, and
    /// a double-width character is two columns of one character.
    #[test]
    fn a_wide_character_is_two_columns_of_the_same_character() {
        let screen = boxed(20, 3, &["a\u{4e2d}b"], "\x1b[2;6H");
        assert_eq!(input_row(&screen), Some(("a\u{4e2d}b".into(), 2)));
    }

    /// The rows below the first are indented to clear the prompt, and that
    /// indent is columns — as is what follows it, where a double-width
    /// character covers two.
    #[test]
    fn a_row_continuing_the_line_counts_wide_characters_as_columns_too() {
        let screen = boxed(8, 4, &["abcdef", "  \u{4e2d}\u{6587}x"], "\x1b[3;7H");
        assert_eq!(
            input_row(&screen),
            Some(("abcdef\n\u{4e2d}\u{6587}x".into(), 9))
        );
    }

    /// Every row after the first offers a break, whether the row above it ran
    /// to the edge or stopped short: the box wraps inside a width of its own,
    /// so where a row ends says nothing about why.
    #[test]
    fn every_row_continuing_the_line_offers_a_break_for_the_line_to_answer() {
        let screen = boxed(8, 4, &["abcdef", "  ghij"], "\x1b[3;7H");
        assert_eq!(input_row(&screen), Some(("abcdef\nghij".into(), 11)));
    }

    /// The cursor is a column of one row, so what stands before it is that
    /// row's own text and the rows above it — never the text of the row below,
    /// which is where a cursor counted against the whole box lands.
    #[test]
    fn the_cursor_counts_the_row_it_is_on_and_not_the_one_under_it() {
        let screen = boxed(20, 5, &["ab   ", "  cdef"], "\x1b[2;8H");
        assert_eq!(input_row(&screen), Some(("ab   \ncdef".into(), 5)));
    }

    /// A line the user has just broken ends in an empty row, and the cursor
    /// standing on it is the whole of what says it is there.
    #[test]
    fn the_empty_row_the_cursor_broke_onto_stays_in_the_line() {
        let screen = boxed(20, 4, &["ab", "  "], "\x1b[3;3H");
        assert_eq!(input_row(&screen), Some(("ab\n".into(), 3)));
    }

    /// The box is padded to the width, so a row's blanks are the program's own
    /// — except the ones the cursor stands after, which the user typed.
    #[test]
    fn a_trailing_space_the_cursor_stands_after_stays_in_the_line() {
        let screen = boxed(20, 3, &["ab "], "\x1b[2;6H");
        assert_eq!(input_row(&screen), Some(("ab ".into(), 3)));
    }

    /// A transcript above can hold a line that starts the same way, and the box
    /// is the bottom-most thing on the screen.
    #[test]
    fn the_box_read_is_the_last_one_on_the_screen() {
        let screen = boxed(
            20,
            5,
            &["old"],
            "\r\n\u{276f}\u{a0}new\r\n\u{2500}\u{2500}\u{2500}",
        );
        assert_eq!(input_row(&screen), Some(("new".into(), 3)));
    }

    /// The box is a prompt row drawn under a border, so a prompt in a
    /// transcript with no box around it is nobody's input line.
    #[test]
    fn a_prompt_row_with_no_box_drawn_round_it_is_not_the_line() {
        let screen = screen(20, 3, "said\r\n\u{276f}\u{a0}old\r\nand more".as_bytes());
        assert_eq!(input_row(&screen), None);
    }

    /// Without the border that closes it the box has no end, and everything
    /// drawn under it — the mode line, a status line — would be read as the
    /// line being typed and handed on as what the user submitted.
    #[test]
    fn a_box_with_no_border_closing_it_is_not_read_at_all() {
        let screen = screen(
            20,
            3,
            "\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}typed\r\nmode line".as_bytes(),
        );
        assert_eq!(input_row(&screen), None);
    }

    /// Claude Code draws its placeholder into the empty box dim, and nothing
    /// else in the box that way, so a hint the program is showing is told from
    /// a line the user typed by the one attribute the grid carries.
    #[test]
    fn the_placeholder_drawn_dim_is_not_read_as_a_typed_line() {
        let screen = screen(
            20,
            3,
            "\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}\x1b[2mTry it\x1b[22m\r\n\u{2500}\u{2500}\u{2500}\x1b[2;3H".as_bytes(),
        );
        assert_eq!(input_row(&screen), Some((String::new(), 0)));
    }

    /// A colour spells itself out in the parameters after it, and the first of
    /// those is the number the dim attribute has — so a scan that reads every
    /// parameter alike dims the whole frame the moment the program paints.
    #[test]
    fn a_colour_written_out_in_parameters_is_not_the_dim_attribute() {
        let screen = screen(
            20,
            3,
            "\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}\x1b[38;5;244mabc\x1b[38;2;153;153;153mdef\r\n\u{2500}\u{2500}\u{2500}\x1b[2;9H".as_bytes(),
        );
        assert_eq!(input_row(&screen), Some(("abcdef".into(), 6)));
    }

    /// A dim run between two bright ones is not the user's text either, and
    /// the columns after it are still where the box put them — so it stands
    /// for the blanks it covers rather than dropping out of the line.
    #[test]
    fn a_dim_run_between_bright_text_stands_for_the_blanks_it_covers() {
        let screen = screen(
            20,
            3,
            "\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}ab\x1b[2mcd\x1b[22mef\r\n\u{2500}\u{2500}\u{2500}\x1b[2;9H".as_bytes(),
        );
        assert_eq!(input_row(&screen), Some(("ab  ef".into(), 6)));
    }

    /// A terminal puts the pen back with the cursor, so a program that saves a
    /// dim one and restores it is drawing dim again — and what it draws then
    /// is its own text rather than the user's.
    #[test]
    fn the_saved_cursor_carries_the_pen_that_was_set_with_it() {
        let screen = screen(
            20,
            3,
            "\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}\x1b[2m\x1b7\x1b[22m\x1b8hint\r\n\u{2500}\u{2500}\u{2500}\x1b[2;3H".as_bytes(),
        );
        assert_eq!(input_row(&screen), Some((String::new(), 0)));
    }

    #[test]
    fn a_dim_word_after_a_bright_one_is_no_part_of_the_line() {
        let screen = screen(
            20,
            3,
            "\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}ab\x1b[2mcd\x1b[22m\r\n\u{2500}\u{2500}\u{2500}\x1b[2;5H".as_bytes(),
        );
        assert_eq!(input_row(&screen), Some(("ab".into(), 2)));
    }

    /// Scrolling moves the text of a row, and the dim saying whether that text
    /// is the user's has to travel with it or it goes on hiding another row.
    #[test]
    fn scrolling_up_carries_the_dim_with_the_row() {
        let screen = screen(
            20,
            5,
            "\x1b[3;1H\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}\x1b[2mhint\x1b[22m\r\n\u{2500}\u{2500}\u{2500}\x1b[5;1H\n".as_bytes(),
        );
        assert_eq!(screen.line(2), "\u{276f}\u{a0}hint");
        assert_eq!(input_row(&screen), Some((String::new(), 0)));
    }

    #[test]
    fn scrolling_down_carries_the_dim_with_the_row() {
        let screen = screen(
            20,
            5,
            "\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}\x1b[2mhint\x1b[22m\r\n\u{2500}\u{2500}\u{2500}\x1b[1;1H\x1bM".as_bytes(),
        );
        assert_eq!(screen.line(2), "\u{276f}\u{a0}hint");
        assert_eq!(input_row(&screen), Some((String::new(), 0)));
    }

    #[test]
    fn a_resize_reshapes_the_dim_with_the_text() {
        let mut screen = screen(
            20,
            3,
            "\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}\x1b[2mhint\x1b[22m\r\n\u{2500}\u{2500}\u{2500}".as_bytes(),
        );
        screen.resize(10, 3);
        assert_eq!(input_row(&screen), Some((String::new(), 0)));
    }

    /// The pen is put aside with the screen it was set on, both ways: what a
    /// program draws on the alternate screen starts bright however it left the
    /// main one, and the main one gets its own pen back when the program
    /// returns to it.
    #[test]
    fn the_alternate_screen_gets_its_own_pen_and_gives_the_first_one_back() {
        let leaving_main_dim = "\x1b[2m\x1b[?1049h\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}typed\r\n\u{2500}\u{2500}\u{2500}\x1b[2;8H";
        let alternate = screen(20, 3, leaving_main_dim.as_bytes());
        assert_eq!(input_row(&alternate), Some(("typed".into(), 5)));
        let returned = screen(20, 3, b"\x1b[?1049h\x1b[2m\x1b[?1049lx");
        assert_eq!(returned.cursor(), (0, 1));
        assert!(!returned.dim[0]);
    }

    #[test]
    fn leaving_the_alternate_screen_gives_back_the_dim_put_aside_with_it() {
        let screen = screen(
            20,
            3,
            "\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}\x1b[2mhint\x1b[22m\r\n\u{2500}\u{2500}\u{2500}\x1b[?1049h\x1b[1;1Hother\x1b[?1049l".as_bytes(),
        );
        assert_eq!(input_row(&screen), Some((String::new(), 0)));
    }

    #[test]
    fn a_screen_without_the_prompt_has_no_input_row() {
        assert_eq!(input_row(&screen(20, 2, b"nothing typed here")), None);
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
