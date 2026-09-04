use std::ops::Range;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthChar;

use crate::checker::{Checker, Flag};
use crate::screen::Line;

const PAINT_OFF: &str = "BLACKJACK_PAINT_OFF";

/// Underlines drawn over the wrapped program's own screen. They go on after the
/// program has finished a frame, and never into its pty: the program is not
/// told about them, because it did not ask for them and would redraw over them
/// anyway.
pub struct Painter {
    on: bool,
    /// How long the line has to hold still before it is worth checking. Every
    /// keystroke redraws the box, and checking each one would spend a
    /// dictionary lookup on a word the user is still in the middle of.
    pause: Duration,
    /// The checker is built on another thread and arrives when it is ready.
    arriving: Receiver<Box<dyn Checker + Send>>,
    checker: Option<Box<dyn Checker + Send>>,
    /// The text the flags were found in, and when it last changed. Flags belong
    /// to their text: the moment it changes they are gone, so a line mid-edit
    /// carries no underline rather than the one before it.
    line: String,
    changed: Instant,
    flags: Option<Vec<Flag>>,
}

impl Painter {
    /// Whether the user wants any of this at all: a paint that ever fights the
    /// wrapped program for the screen has to be turnable off without a
    /// rebuild, and off means the dictionary is never built either.
    pub fn wanted() -> bool {
        std::env::var_os(PAINT_OFF).is_none()
    }

    /// The switch is read once, at the start.
    pub fn open(pause: Duration, arriving: Receiver<Box<dyn Checker + Send>>) -> Self {
        Self {
            on: Self::wanted(),
            pause,
            arriving,
            checker: None,
            line: String::new(),
            changed: Instant::now(),
            flags: None,
        }
    }

    /// Nothing is drawn while the program has a frame open: what it is drawing
    /// is half done, and a paint closing that frame would present the half.
    /// The line is the one the user typed — the box's own wrapping already
    /// taken back out of it — while its spots are where the box is showing it,
    /// so a word broken across two rows is one word to the checker and two
    /// underlines on the screen.
    pub fn paint(
        &mut self,
        line: Option<&Line>,
        cursor: (usize, usize),
        mid_frame: bool,
    ) -> Option<Vec<u8>> {
        if !self.on || mid_frame {
            return None;
        }
        let line = line?;
        if self.checker.is_none() {
            self.checker = self.arriving.try_recv().ok();
        }
        if line.text != self.line {
            self.line.clone_from(&line.text);
            self.changed = Instant::now();
            self.flags = None;
            return None;
        }
        if self.flags.is_none() {
            if self.changed.elapsed() < self.pause {
                return None;
            }
            self.flags = Some(self.checker.as_mut()?.check(&line.text));
        }
        let runs: Vec<_> = self
            .flags
            .iter()
            .flatten()
            .flat_map(|flag| runs_of(line, &flag.span))
            .collect();
        (!runs.is_empty()).then(|| draw(&runs, cursor))
    }

    /// How long the output side may wait on the wrapped program before this
    /// painter has something to do regardless. `None` is "nothing pending":
    /// the line has been checked, or there is no line, and only the program
    /// drawing again can change that. Everything else is the debounce, which
    /// is the painter's own clock — the last frame a keystroke produces is the
    /// one that changed the line, so a user who stops typing leaves nothing
    /// behind to wake it.
    pub fn wake_in(&self) -> Option<Duration> {
        if !self.on || self.line.is_empty() || self.flags.is_some() {
            return None;
        }
        match self.checker.is_none() {
            // Waiting on the dictionary rather than on the user, and a channel
            // that is not being watched is looked in on a pause at a time.
            true => Some(self.pause),
            false => Some(self.pause.saturating_sub(self.changed.elapsed())),
        }
    }
}

/// A flagged stretch of the line as it can actually be drawn: the characters
/// standing next to each other on one row, at the column the first of them sits
/// in. The box may have broken the stretch across two rows, and the break
/// itself has no cell — either ends the run and starts another.
fn runs_of(line: &Line, span: &Range<usize>) -> Vec<(usize, usize, String)> {
    let mut runs: Vec<(usize, usize, String)> = Vec::new();
    let mut after = None;
    for (at, character) in line
        .text
        .chars()
        .enumerate()
        .take(span.end)
        .skip(span.start)
    {
        // A break has no cell of its own, and the row below is a row along:
        // whatever follows it starts a run wherever it starts.
        let Some((row, col)) = line.spots.get(at).copied().flatten() else {
            continue;
        };
        match runs.last_mut() {
            Some((.., text)) if after == Some((row, col)) => text.push(character),
            _ => runs.push((row, col, character.to_string())),
        }
        after = Some((row, col + character.width().unwrap_or(1)));
    }
    runs
}

/// A frame of its own, so the terminal shows the underlines in one update
/// rather than a cursor jumping about. The cursor is put back by moving it
/// rather than by `ESC 7`/`ESC 8`: a terminal keeps one saved cursor per screen
/// and the program uses that slot itself, so saving here would hand the program
/// this paint's position back as its own. The cost of not using it is that it
/// would have carried the pen too — so the words are re-printed in whatever
/// colour the program had set, and the two attributes this does set are turned
/// off one each because they are two attributes.
fn draw(runs: &[(usize, usize, String)], cursor: (usize, usize)) -> Vec<u8> {
    let mut paint = String::from("\x1b[?2026h");
    for (row, col, text) in runs {
        paint += &format!(
            "\x1b[{};{}H\x1b[4:3m\x1b[58:2::255:0:0m{text}\x1b[59m\x1b[24m",
            row + 1,
            col + 1
        );
    }
    paint += &format!("\x1b[{};{}H\x1b[?2026l", cursor.0 + 1, cursor.1 + 1);
    paint.into_bytes()
}

#[cfg(test)]
impl Painter {
    /// A painter holding a checker already, with no pause to wait out.
    pub fn checking(checker: Box<dyn Checker + Send>) -> Self {
        Self {
            on: true,
            checker: Some(checker),
            ..Self::off()
        }
    }

    /// A painter that draws nothing, however it is asked.
    pub fn off() -> Self {
        Self {
            on: false,
            pause: Duration::ZERO,
            arriving: std::sync::mpsc::channel().1,
            checker: None,
            line: String::new(),
            changed: Instant::now(),
            flags: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::checker::FakeChecker;
    use crate::screen::Screen;

    /// What the tap hands over after a frame: the line the box is showing —
    /// with no input model behind these tests, the screen's own reading of it
    /// stands in for the user's — and where the program left its cursor.
    fn ask(painter: &mut Painter, screen: &Screen) -> Option<Vec<u8>> {
        painter.paint(
            screen.frame_line().as_ref(),
            screen.cursor(),
            screen.mid_frame(),
        )
    }

    /// A frame of the box, closed — the line is read at the close and nowhere
    /// else, so a box drawn without one is a box nobody has seen.
    fn box_holding(text: &str) -> Screen {
        let mut screen = Screen::new(20, 3);
        let drawn = format!(
            "\x1b[?2026h\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}{text}\r\n\u{2500}\u{2500}\u{2500}\x1b[?2026l"
        );
        crate::parser::Parser::new().advance(&mut screen, drawn.as_bytes());
        screen
    }

    /// A checker that flags one fixed stretch of whatever it is handed, and
    /// counts how often it was asked — a word-based fake can say neither.
    struct Spanning(Range<usize>, Arc<AtomicUsize>);

    impl Checker for Spanning {
        fn check(&mut self, _: &str) -> Vec<Flag> {
            self.1.fetch_add(1, Ordering::Relaxed);
            vec![Flag {
                span: self.0.clone(),
                kind: crate::checker::Kind::Spelling,
                message: String::new(),
                suggestions: Vec::new(),
            }]
        }
    }

    fn spanning(span: Range<usize>) -> (Painter, Arc<AtomicUsize>) {
        let asked = Arc::new(AtomicUsize::new(0));
        let checker = Spanning(span, asked.clone());
        (Painter::checking(Box::new(checker)), asked)
    }

    fn checking(flagged: &[&str]) -> Painter {
        Painter::checking(Box::new(FakeChecker::spelling(flagged)))
    }

    /// The frame that first carries a line is the one that changed it, and a
    /// line that has just changed has no flags — so the paint, if there is one,
    /// is on the frame after.
    fn paint_of(painter: &mut Painter, screen: &Screen) -> Option<String> {
        assert!(ask(painter, screen).is_none(), "painted a changed line");
        ask(painter, screen).map(|paint| String::from_utf8(paint).expect("utf8"))
    }

    /// What the checker flags is drawn where the box is showing it.
    #[test]
    fn a_flagged_word_is_underlined_where_it_stands() {
        let paint = paint_of(&mut checking(&["teh"]), &box_holding("a teh word"));
        assert_eq!(
            paint.expect("a paint"),
            "\x1b[?2026h\x1b[2;5H\x1b[4:3m\x1b[58:2::255:0:0mteh\x1b[59m\x1b[24m\x1b[3;4H\x1b[?2026l"
        );
    }

    #[test]
    fn a_line_the_checker_passes_is_not_painted_on() {
        let paint = paint_of(&mut checking(&["teh"]), &box_holding("all spelled right"));
        assert!(paint.is_none());
    }

    /// The placeholder the box shows while the line is empty is the program's
    /// own text, not the user's.
    #[test]
    fn a_word_the_box_drew_dim_is_not_underlined() {
        let paint = paint_of(&mut checking(&["teh"]), &box_holding("\x1b[2mteh\x1b[22m"));
        assert!(paint.is_none());
    }

    /// The program is in the middle of drawing: closing a frame it opened would
    /// present half a redraw, so nothing goes out until it closes its own.
    #[test]
    fn nothing_is_drawn_into_a_frame_the_program_has_open() {
        let mut painter = checking(&["teh"]);
        let closed = box_holding("a teh word");
        assert!(
            ask(&mut painter, &closed).is_none(),
            "painted a changed line"
        );
        let mut redrawing = box_holding("a teh word");
        crate::parser::Parser::new().advance(&mut redrawing, b"\x1b[?2026h");
        assert!(ask(&mut painter, &redrawing).is_none());
        assert!(ask(&mut painter, &closed).is_some(), "and never again");
    }

    #[test]
    fn the_switch_turns_the_paint_off_altogether() {
        let mut painter = Painter::off();
        let drawn = box_holding("a teh word");
        assert!(ask(&mut painter, &drawn).is_none());
        assert!(ask(&mut painter, &drawn).is_none());
    }

    /// Until the checker arrives there is nothing to ask, and the frames that
    /// land in the meantime pass unpainted rather than waiting for it.
    #[test]
    fn nothing_is_painted_before_the_checker_arrives() {
        let (built, arriving) = std::sync::mpsc::channel();
        let mut painter = Painter::open(Duration::ZERO, arriving);
        let drawn = box_holding("a teh word");
        assert!(
            ask(&mut painter, &drawn).is_none(),
            "painted a changed line"
        );
        assert!(
            ask(&mut painter, &drawn).is_none(),
            "painted with no checker"
        );
        built
            .send(Box::new(FakeChecker::spelling(&["teh"])) as Box<dyn Checker + Send>)
            .expect("deliver the checker");
        assert!(ask(&mut painter, &drawn).is_some());
    }

    /// A line still being typed is not worth a dictionary lookup: the check
    /// waits for the typing to stop, and a change starts the wait again.
    #[test]
    fn the_check_waits_for_the_line_to_hold_still() {
        let mut painter = checking(&["teh"]);
        painter.pause = Duration::from_millis(50);
        let drawn = box_holding("a teh word");
        assert!(
            ask(&mut painter, &drawn).is_none(),
            "painted a changed line"
        );
        assert!(
            ask(&mut painter, &drawn).is_none(),
            "painted inside the pause"
        );
        std::thread::sleep(Duration::from_millis(60));
        assert!(ask(&mut painter, &drawn).is_some());
    }

    /// Flags belong to the text they were found in. A change drops them, so
    /// neither the frame carrying it nor the ones after it can put yesterday's
    /// underline under a line that has moved on.
    #[test]
    fn a_changed_line_drops_the_flags_it_had() {
        let mut painter = checking(&["teh"]);
        let drawn = box_holding("a teh word");
        assert!(
            ask(&mut painter, &drawn).is_none(),
            "painted a changed line"
        );
        assert!(ask(&mut painter, &drawn).is_some());
        let fixed = box_holding("a the word");
        assert!(
            ask(&mut painter, &fixed).is_none(),
            "painted the change itself"
        );
        assert!(ask(&mut painter, &fixed).is_none(), "painted the old flags");
    }

    /// The check is what costs — a dictionary lookup — and the box redraws for
    /// its own reasons. A line that has not changed is not asked about again.
    #[test]
    fn a_line_that_has_not_changed_is_not_checked_again() {
        let (mut painter, asked) = spanning(0..3);
        let drawn = box_holding("a teh word");
        for _ in 0..4 {
            ask(&mut painter, &drawn);
        }
        assert_eq!(asked.load(Ordering::Relaxed), 1);
    }

    /// One flag can cover more than one row — a phrase the box wrapped — and is
    /// underlined on each of them, at the column it starts in there. The break
    /// itself has no cell and joins neither run.
    #[test]
    fn a_flag_the_box_broke_across_rows_is_drawn_on_each() {
        let mut screen = Screen::new(8, 4);
        let drawn = "\x1b[?2026h\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}abcdef\r\n  ghij\r\n\u{2500}\u{2500}\u{2500}\x1b[3;7H\x1b[?2026l";
        crate::parser::Parser::new().advance(&mut screen, drawn.as_bytes());
        let (mut painter, _) = spanning(0..11);
        let paint = paint_of(&mut painter, &screen).expect("a paint");
        assert!(
            paint.contains("\x1b[2;3H\x1b[4:3m\x1b[58:2::255:0:0mabcdef"),
            "{paint:?}"
        );
        assert!(
            paint.contains("\x1b[3;3H\x1b[4:3m\x1b[58:2::255:0:0mghij"),
            "{paint:?}"
        );
    }
}
