use std::ops::Range;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::checker::{Checker, Class, Flag, worded};
use crate::log::Log;
use crate::picker::{SharedChecked, SharedLearned};
use crate::prose;
use crate::screen::{Line, Look};

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
    /// What the checker said, left where the keystroke side can read it: the
    /// picker runs there and the checking runs here.
    checked: SharedChecked,
    /// Why this painter did what it did, for a user who watched it do something
    /// else. Nothing here changes what is painted.
    log: Log,
    /// And what the user has since said back. Read on every paint rather than
    /// on every check, so a word learned now stops being underlined at the next
    /// frame instead of the next time the line is asked about.
    learned: SharedLearned,
    /// The runs this painter last drew. The program redraws only the cells a
    /// keystroke changed, so an underline over cells it does not touch stays
    /// on the terminal until this painter takes it off itself.
    painted: Vec<Run>,
    /// And the ones the box was not showing when they were due to come off,
    /// kept for one frame in case the frame that shows them is the next one.
    /// Two frames without them and the program has redrawn those cells itself.
    missing: Vec<Run>,
}

impl Painter {
    /// Whether the user wants any of this at all — a paint that fights the
    /// wrapped program for the screen has to be turnable off without a
    /// rebuild. Asked once, before anything is spent on the answer: off means
    /// no config read, no dictionary built, and nothing drawn.
    pub fn wanted() -> bool {
        std::env::var_os(PAINT_OFF).is_none()
    }

    pub fn open(
        on: bool,
        pause: Duration,
        arriving: Receiver<Box<dyn Checker + Send>>,
        checked: SharedChecked,
        learned: SharedLearned,
        log: Log,
    ) -> Self {
        Self {
            on,
            pause,
            arriving,
            checker: None,
            line: String::new(),
            changed: Instant::now(),
            flags: None,
            checked,
            log,
            learned,
            painted: Vec::new(),
            missing: Vec::new(),
        }
    }

    /// Nothing is drawn while the program has a frame open: what it is drawing
    /// is half done, and a paint closing that frame would present the half.
    /// The line is the one the user typed — the box's own wrapping already
    /// taken back out of it — while its spots are where the box is showing it,
    /// so a word broken across two rows is one word to the checker and two
    /// underlines on the screen.
    pub fn paint(&mut self, line: Option<&Line>, look: &Look) -> Option<Vec<u8>> {
        if !self.on {
            return self.refused("off");
        }
        let Some(line) = line else {
            // The box has left the screen — an alternate screen, a dialog, a
            // program that cleared it. What was cached describes a line that is
            // nowhere, and keeping it would also leave a pause running out for
            // a line nothing can paint.
            self.line.clear();
            self.flags = None;
            // Nothing can be taken off a screen this painter can no longer
            // read: the coordinates of what it drew mean nothing against
            // whatever is there now. A box that comes back is read again from
            // scratch, and what is still flagged is painted again over the
            // same cells.
            self.painted.clear();
            self.missing.clear();
            return self.refused("no box");
        };
        if self.checker.is_none() {
            self.checker = self.arriving.try_recv().ok();
        }
        if line.text != self.line {
            self.line.clone_from(&line.text);
            self.changed = Instant::now();
            self.flags = None;
            // The underlines belonged to the line that just went. They come
            // off in the same write as the frame that replaced it, or the
            // user reads them as belonging to what they are typing now.
            return self.unpaint(line, look, "line changed");
        }
        if self.flags.is_none() {
            if self.changed.elapsed() < self.pause {
                return self.refused("still typing");
            }
            // Asked about the prose only: a checker has nothing useful to say
            // about a path or a flag, and everything it says about one is noise
            // over something the user typed on purpose.
            let Some(checker) = self.checker.as_mut() else {
                return self.refused("no checker yet");
            };
            let asked = prose::filtered(&line.text);
            let (flags, across): (Vec<_>, Vec<_>) = checker
                .check(&asked)
                .into_iter()
                .partition(|flag| prose::intact(&line.text, &flag.span));
            self.checked(&line.text, &flags, &across);
            self.publish(&line.text, &flags);
            self.flags = Some(flags);
        }
        let Ok(learned) = self.learned.lock() else {
            return self.refused("learned words unreadable");
        };
        let runs: Vec<_> = self
            .flags
            .iter()
            .flatten()
            .filter(|flag| !learned.contains(&worded(&line.text, &flag.span)))
            .filter(|flag| !being_typed(line, &flag.span))
            .flat_map(|flag| runs_of(line, flag))
            .collect();
        drop(learned);
        if runs.is_empty() {
            return self.unpaint(line, look, "nothing to underline");
        }
        // The two states nothing may be written in, and the reason they are
        // tested here rather than on the way in: both are about the bytes, not
        // the line. Refusing before the check would leave the line unchecked
        // with its pause already run out, and then this painter asks to be
        // woken again and again for work it will refuse again and again.
        //
        // A frame the program still has open would be presented half-drawn by
        // this paint's own close; a glyph in the last column leaves the
        // terminal holding a wrap that the cursor move would discard, and the
        // program's next character would land on this row rather than the next.
        if look.mid_frame {
            return self.refused("mid frame");
        }
        if look.wrap_pending {
            return self.refused("wrap pending");
        }
        let off = self.stale(line, &runs);
        let paint = draw(&off, &runs, look);
        self.remember(line, runs.clone());
        if self.log.is_on() {
            self.log.line(format_args!(
                "paint {}B at [{}]",
                paint.len(),
                placed(&runs)
            ));
        }
        Some(paint)
    }

    /// The underlines standing on the screen with nothing left to justify
    /// them, taken off in this frame. A frame that may not be written in keeps
    /// them for the next one — a stale underline is worth less than a frame
    /// presented half-drawn.
    fn unpaint(&mut self, line: &Line, look: &Look, reason: &str) -> Option<Vec<u8>> {
        if self.painted.is_empty() {
            return self.refused(reason);
        }
        if look.mid_frame || look.wrap_pending {
            return self.refused(reason);
        }
        let off = self.stale(line, &[]);
        self.remember(line, Vec::new());
        if off.is_empty() {
            return self.refused(reason);
        }
        let paint = draw(&off, &[], look);
        if self.log.is_on() {
            self.log.line(format_args!(
                "unpaint {}B at [{}]: {reason}",
                paint.len(),
                placed(&off)
            ));
        }
        Some(paint)
    }

    /// The runs to take off: what was drawn last time and is not being drawn
    /// now, re-read where the box is showing those cells today rather than
    /// trusted from when they were drawn.
    fn stale(&self, line: &Line, keeping: &[Run]) -> Vec<Run> {
        self.painted
            .iter()
            .filter(|run| !keeping.contains(run))
            .flat_map(|run| now_at(line, run))
            .collect()
    }

    /// What is on the terminal after this frame: what was just drawn, and what
    /// was due to come off and could not be found. A run the box has not shown
    /// for two frames running is one the program has drawn over itself, and
    /// keeping it would cost a scan of the line every frame for good.
    fn remember(&mut self, line: &Line, drawn: Vec<Run>) {
        let missed: Vec<Run> = self
            .painted
            .iter()
            .filter(|run| !drawn.contains(run) && now_at(line, run).is_empty())
            .filter(|run| !self.missing.contains(run))
            .cloned()
            .collect();
        self.painted = drawn;
        self.painted.extend(missed.iter().cloned());
        self.missing = missed;
    }

    /// Nothing painted, and why. Every path out of `paint` that draws nothing
    /// comes through here, so the log accounts for each frame it was offered.
    fn refused(&self, reason: &str) -> Option<Vec<u8>> {
        self.log.line(format_args!("no paint: {reason}"));
        None
    }

    /// What was asked and what came back: the line as the user typed it, the
    /// tokens the filter took out before the checker saw it, and where the
    /// answers landed.
    fn checked(&self, line: &str, flags: &[Flag], across: &[Flag]) {
        if !self.log.is_on() {
            return;
        }
        self.log.line(format_args!(
            "check \"{}\" not prose {:?} flags [{}] across removed [{}]",
            crate::log::escape(line.as_bytes()),
            prose::removed(line),
            spanned(flags),
            spanned(across)
        ));
    }

    /// The answer, and the line it is about, for whoever is watching the
    /// keystrokes. A flag is a span into a particular line and means nothing
    /// against another, so the two travel together.
    fn publish(&self, line: &str, flags: &[Flag]) {
        if let Ok(mut checked) = self.checked.lock() {
            checked.line = line.to_string();
            checked.flags = flags.to_vec();
        }
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

/// Where a log line says the runs landed.
fn placed(runs: &[Run]) -> String {
    runs.iter()
        .map(|run| format!("{},{} {}", run.row, run.col, named(run.class)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Spans as a log line lists them.
fn spanned(flags: &[Flag]) -> String {
    flags
        .iter()
        .map(|flag| format!("{}..{}", flag.span.start, flag.span.end))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The colour a slip is underlined in, as the parameter of an underline-colour
/// escape: red for a word the dictionary does not have, blue for a slip in the
/// grammar. Which key answers which is what the two are for, and `Class` says
/// that.
fn coloured(class: Class) -> &'static str {
    match class {
        Class::Spelling => "58:2::255:0:0",
        Class::Grammar => "58:2::64:156:255",
    }
}

/// The same colour as a word, for the account of a session: it is read by
/// someone who watched a colour appear, not by someone decoding an escape.
fn named(class: Class) -> &'static str {
    match class {
        Class::Spelling => "red",
        Class::Grammar => "blue",
    }
}

/// A flagged stretch of the line as it can actually be drawn: the characters
/// standing next to each other on one row, at the column the first of them
/// sits in, in the colour the kind of slip is underlined in.
#[derive(Clone, PartialEq, Eq)]
struct Run {
    row: usize,
    col: usize,
    text: String,
    class: Class,
}

/// The box may have broken a flagged stretch across two rows, and the break
/// itself has no cell — either one ends a run and starts another.
fn runs_of(line: &Line, flag: &Flag) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut after = None;
    for (at, character) in line
        .text
        .chars()
        .enumerate()
        .take(flag.span.end)
        .skip(flag.span.start)
    {
        // A break has no cell of its own, and the row below is a row along:
        // whatever follows it starts a run wherever it starts.
        let Some((row, col)) = line.spots.get(at).copied().flatten() else {
            continue;
        };
        match runs.last_mut() {
            Some(run) if after == Some((row, col)) => run.text.push(character),
            _ => runs.push(Run {
                row,
                col,
                text: character.to_string(),
                class: flag.class,
            }),
        }
        after = Some((row, col + character.width().unwrap_or(1)));
    }
    runs
}

/// Whether the run put a blank in this column itself, which is the difference
/// between a cell it has to undraw and one the program has drawn over.
fn drew_a_blank(run: &Run, col: usize) -> bool {
    let mut at = run.col;
    for character in run.text.chars() {
        let width = character.width().unwrap_or(1);
        if col < at + width {
            return character == ' ';
        }
        at += width;
    }
    false
}

/// A word the user is still inside is not a mistake yet: the cursor in it, or
/// at its end with nothing but more of the word to come, means they have not
/// finished saying it. Every fragment of a long word would otherwise be
/// underlined on the way past.
fn being_typed(line: &Line, span: &Range<usize>) -> bool {
    if line.cursor <= span.start || line.cursor > span.end {
        return false;
    }
    line.cursor < span.end
        || line
            .text
            .chars()
            .nth(line.cursor)
            .is_none_or(|next| next.is_alphanumeric())
}

/// The cells a run covers, as the box is showing them now. What was drawn over
/// them is no guide: by the time an underline has to come off, the program may
/// have written something else there, and the line the box shows is the only
/// account of that this painter is given.
fn now_at(line: &Line, run: &Run) -> Vec<Run> {
    let mut found: Vec<Run> = Vec::new();
    let mut after = None;
    for (at, character) in line.text.chars().enumerate() {
        let Some((row, spot)) = line.spots.get(at).copied().flatten() else {
            continue;
        };
        let width = character.width().unwrap_or(1);
        let inside =
            row == run.row && spot >= run.col && spot + width <= run.col + run.text.width();
        // A blank the box is showing where this run drew a word is the
        // program's own — its write, or the dim placeholder the model reads as
        // a blank — and writing a space back would be this painter drawing
        // rather than undrawing. A blank where the run drew a blank is this
        // painter's own, and comes off like anything else it drew: a rule about
        // the gap between two words underlines the gap.
        if !inside || (character == ' ' && !drew_a_blank(run, spot)) {
            continue;
        }
        match found.last_mut() {
            Some(run) if after == Some(spot) => run.text.push(character),
            _ => found.push(Run {
                row,
                col: spot,
                text: character.to_string(),
                class: run.class,
            }),
        }
        after = Some(spot + width);
    }
    found
}

/// A frame of its own, so the terminal shows the underlines in one update
/// rather than a cursor jumping about. The cursor is put back by moving it
/// rather than by `ESC 7`/`ESC 8`: a terminal keeps one saved cursor per screen
/// and the program uses that slot itself, so saving here would hand the program
/// this paint's position back as its own. `ESC 7` would have carried the pen
/// too, so the one attribute this model tracks is turned off around the words
/// and put back after them. Only that one: clearing the pen outright would
/// print the words correctly and throw away every colour the model does not
/// carry, and the program — believing its pen untouched — would go on writing
/// in the wrong one. Against the program this wrapper is for it is a
/// formality either way, since neither capture has a single frame closing with
/// any attribute set at all.
fn draw(off: &[Run], on: &[Run], look: &Look) -> Vec<u8> {
    let mut paint = String::from("\x1b[?2026h");
    if look.dimmed {
        paint += "\x1b[22m";
    }
    for run in off {
        paint += &format!(
            "\x1b[{};{}H\x1b[59m\x1b[24m{}",
            run.row + 1,
            run.col + 1,
            run.text
        );
    }
    for run in on {
        paint += &format!(
            "\x1b[{};{}H\x1b[4:3m\x1b[{}m{}\x1b[59m\x1b[24m",
            run.row + 1,
            run.col + 1,
            coloured(run.class),
            run.text
        );
    }
    paint += &format!("\x1b[{};{}H", look.cursor.0 + 1, look.cursor.1 + 1);
    if look.dimmed {
        paint += "\x1b[2m";
    }
    paint += "\x1b[?2026l";
    paint.into_bytes()
}

#[cfg(test)]
impl Painter {
    /// The same painter with its decisions written down, for a test that reads
    /// the account back.
    pub fn logging(mut self, log: Log) -> Self {
        self.log = log;
        self
    }

    /// A painter holding a checker already, with no pause to wait out.
    pub fn checking(checker: Box<dyn Checker + Send>) -> Self {
        Self {
            checker: Some(checker),
            ..Self::open(
                true,
                Duration::ZERO,
                std::sync::mpsc::channel().1,
                SharedChecked::default(),
                SharedLearned::default(),
                Log::off(),
            )
        }
    }

    /// A painter that draws nothing, however it is asked.
    pub fn off() -> Self {
        Self::open(
            false,
            Duration::ZERO,
            std::sync::mpsc::channel().1,
            SharedChecked::default(),
            SharedLearned::default(),
            Log::off(),
        )
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
        let look = screen.look();
        painter.paint(look.line.as_ref(), &look)
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
                class: crate::checker::Class::Spelling,
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
        // Refused because of the bytes, not the line: the line was checked all
        // the same, or this painter would ask to be woken for a paint it is
        // going to refuse again, and again, as fast as it is asked.
        assert!(
            painter.wake_in().is_none(),
            "still waiting to check the line"
        );
        assert!(
            ask(&mut painter, &closed).is_some(),
            "and never again once it closed"
        );
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
        let mut painter = Painter::open(
            true,
            Duration::ZERO,
            arriving,
            SharedChecked::default(),
            SharedLearned::default(),
            Log::off(),
        );
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
        // The frame that changed the line carries the underline off it.
        let taken_off = ask(&mut painter, &fixed).expect("the underline taken off");
        let taken_off = String::from_utf8(taken_off).expect("utf8");
        assert!(
            taken_off.contains("\x1b[2;5H\x1b[59m\x1b[24mthe"),
            "the cells were not written back where they stand: {taken_off:?}"
        );
        assert!(!taken_off.contains("\x1b[4:3m"), "{taken_off:?}");
        assert!(ask(&mut painter, &fixed).is_none(), "painted the old flags");
    }

    /// The check is what costs — a dictionary lookup — and the box redraws for
    /// its own reasons. A line that has not changed is not asked about again,
    /// and every one of those redraws is painted over again, because each of
    /// them wiped the underline the one before it carried.
    #[test]
    fn every_redraw_of_the_same_line_is_painted_over_again() {
        let (mut painter, asked) = spanning(0..3);
        let drawn = box_holding("a teh word");
        assert!(
            ask(&mut painter, &drawn).is_none(),
            "painted a changed line"
        );
        for redraw in 0..3 {
            assert!(ask(&mut painter, &drawn).is_some(), "redraw {redraw}");
        }
        assert_eq!(asked.load(Ordering::Relaxed), 1);
    }

    /// The box is gone — an alternate screen, a dialog, a program that cleared
    /// it. There is nothing to paint on and nothing the cached flags describe,
    /// so they go: the same text coming back is a new line, checked again. The
    /// pause goes with them, or the painter would sit asking to be woken for a
    /// line that is nowhere.
    #[test]
    fn a_box_that_leaves_the_screen_takes_its_flags_with_it() {
        let (mut painter, asked) = spanning(0..3);
        let drawn = box_holding("a teh word");
        assert!(
            ask(&mut painter, &drawn).is_none(),
            "painted a changed line"
        );
        assert!(ask(&mut painter, &drawn).is_some());

        let mut gone = Screen::new(20, 3);
        crate::parser::Parser::new().advance(&mut gone, b"\x1b[?2026h\x1b[2Jbusy\x1b[?2026l");
        assert!(ask(&mut painter, &gone).is_none(), "painted with no box");
        assert!(
            painter.wake_in().is_none(),
            "still waiting on a line nobody has"
        );

        assert!(
            ask(&mut painter, &drawn).is_none(),
            "painted from a dropped cache"
        );
        assert!(ask(&mut painter, &drawn).is_some());
        assert_eq!(asked.load(Ordering::Relaxed), 2);
    }

    /// A glyph in the last column leaves the terminal holding a wrap it has not
    /// acted on yet. Moving the cursor away discards it, and the program's next
    /// character would then land on this row instead of the next one.
    #[test]
    fn a_frame_ending_on_a_pending_wrap_is_left_alone() {
        // Drawn twice: the first frame ends on a cursor move, which is the
        // terminal committing to the wrap, and it is the one that puts the line
        // in the painter's hands. Only the second ends on the glyph itself.
        let (mut painter, asked) = spanning(0..3);
        let committed = wrapping_box("\x1b[2;7H");
        assert!(!committed.look().wrap_pending, "the wrap was left pending");
        assert!(
            ask(&mut painter, &committed).is_none(),
            "painted a changed line"
        );

        let pending = wrapping_box("");
        assert!(
            pending.look().wrap_pending,
            "the box did not fill its last column"
        );
        assert!(ask(&mut painter, &pending).is_none());
        assert!(
            painter.wake_in().is_none(),
            "asking to be woken for a paint it will refuse again"
        );
        assert_eq!(asked.load(Ordering::Relaxed), 1, "the line went unchecked");
    }

    /// A box whose last row runs to the last column, with whatever the program
    /// did next before the frame closed.
    fn wrapping_box(then: &str) -> Screen {
        let mut screen = Screen::new(8, 3);
        let drawn = format!(
            "\x1b[?2026h\x1b[1;1H\u{2500}\u{2500}\u{2500}\x1b[3;1H\u{2500}\u{2500}\u{2500}\x1b[2;1H\u{276f}\u{a0}abcdef{then}\x1b[?2026l"
        );
        crate::parser::Parser::new().advance(&mut screen, drawn.as_bytes());
        screen
    }

    /// The paint prints the word itself, so an attribute the program left set
    /// would be printed with it. The one this model tracks is turned off around
    /// the words and put back after them — and only that one, so the colours it
    /// does not track are left exactly as the program set them.
    #[test]
    fn the_pen_the_program_left_is_put_back() {
        let mut screen = box_holding("a teh word");
        crate::parser::Parser::new().advance(&mut screen, b"\x1b[?2026h\x1b[2m\x1b[?2026l");
        let mut painter = checking(&["teh"]);

        assert!(
            ask(&mut painter, &screen).is_none(),
            "painted a changed line"
        );
        let paint = String::from_utf8(ask(&mut painter, &screen).expect("a paint")).expect("utf8");

        assert!(paint.starts_with("\x1b[?2026h\x1b[22m"), "{paint:?}");
        assert!(paint.ends_with("\x1b[2m\x1b[?2026l"), "{paint:?}");
    }

    /// And with no attribute set, nothing is said about the pen at all: a reset
    /// would print the words right and take every colour this model does not
    /// track down with it.
    #[test]
    fn a_pen_the_program_left_alone_is_not_touched() {
        let paint = paint_of(&mut checking(&["teh"]), &box_holding("a teh word")).expect("a paint");

        assert!(!paint.contains("\x1b[0m"), "{paint:?}");
        assert!(!paint.contains("\x1b[22m"), "{paint:?}");
        assert!(!paint.contains("\x1b[2m"), "{paint:?}");
    }

    /// The real engine on a real terminal line: a path is three words to a
    /// dictionary and every one of them is wrong, which is the noise that makes
    /// a user switch the wrapper off. Nothing of it reaches the checker.
    #[test]
    fn a_path_is_not_underlined_and_a_misspelling_beside_it_still_is() {
        let engine = || Box::new(crate::harper::HarperChecker::new(&[], None));

        // The box in these tests is twenty columns wide, so the line is short.
        let quiet = paint_of(&mut Painter::checking(engine()), &box_holding("src/pty.rs"));
        let flagged = paint_of(
            &mut Painter::checking(engine()),
            &box_holding("teh src/pty.rs"),
        );

        assert!(quiet.is_none(), "underlined a path: {quiet:?}");
        let flagged = flagged.expect("the misspelling beside it");
        assert!(flagged.contains("teh"), "{flagged:?}");
        assert!(!flagged.contains("src"), "{flagged:?}");
    }

    /// What the filter fills a hole with is not the user's text, so a flag
    /// standing over it is declined before it can be shown — and with it the
    /// fix chord and the word list, which are handed the flags this publishes.
    /// A checker that flags the filling is the only way to ask: the real engine
    /// has no opinion about it, which is exactly why it is the filler.
    #[test]
    fn a_flag_over_what_the_filter_filled_in_is_never_shown() {
        let mut painter = checking(&["00000000"]);

        let paint = paint_of(&mut painter, &box_holding("a src/x.rs word"));

        assert!(
            paint.is_none(),
            "underlined the filter's own filling: {paint:?}"
        );
    }

    /// Which colour an underline is drawn in says which kind of slip it stands
    /// over, because the two are answered by different keys: a word the
    /// dictionary does not have can be taught, and a slip in the grammar can
    /// only be fixed. The line carrying one of each carries both colours.
    #[test]
    fn a_misspelling_is_underlined_in_red_and_a_grammar_slip_in_blue() {
        let engine = || {
            let classes: Vec<String> = crate::harper::classes().map(str::to_string).collect();
            Box::new(crate::harper::HarperChecker::new(&classes, None))
        };

        let spelled = paint_of(&mut Painter::checking(engine()), &box_holding("a teh word"))
            .expect("the misspelling");
        let both = paint_of(
            &mut Painter::checking(engine()),
            &box_holding("its a teh word"),
        )
        .expect("one of each");

        assert!(spelled.contains("\x1b[58:2::255:0:0mteh"), "{spelled:?}");
        assert!(!spelled.contains("58:2::64:156:255"), "{spelled:?}");
        assert!(both.contains("\x1b[58:2::64:156:255mits"), "{both:?}");
        assert!(both.contains("\x1b[58:2::255:0:0mteh"), "{both:?}");
    }

    /// Taking an underline off is the same in either colour: the escape that
    /// ends it names none, so this painter leaves none of its own on the pen.
    #[test]
    fn an_underline_comes_off_the_same_whichever_colour_it_was() {
        let classes: Vec<String> = crate::harper::classes().map(str::to_string).collect();
        let mut painter =
            Painter::checking(Box::new(crate::harper::HarperChecker::new(&classes, None)));
        assert!(paint_of(&mut painter, &box_holding("its a teh word")).is_some());

        let taken_off = ask(&mut painter, &box_holding("it's a the word")).expect("both off");
        let taken_off = String::from_utf8(taken_off).expect("utf8");

        assert!(taken_off.contains("\x1b[59m\x1b[24m"), "{taken_off:?}");
        assert!(!taken_off.contains("58:2::"), "{taken_off:?}");
    }

    /// A long word is a run of misspellings on the way to being one word, and
    /// underlining each fragment as it goes is the wrapper talking over the
    /// user. Nothing is said about the word until they have finished saying it.
    #[test]
    fn the_word_under_the_cursor_is_left_alone_until_it_is_finished() {
        let mut typing = checking(&["teh"]);
        assert!(
            paint_of(&mut typing, &box_holding("teh")).is_none(),
            "underlined a word still being typed"
        );

        let mut finished = checking(&["teh"]);
        let painted = paint_of(&mut finished, &box_holding("teh word")).expect("a paint");
        assert!(painted.contains("teh"), "{painted:?}");
    }

    /// A misspelling at the very start of the line, with the program holding
    /// its cursor above the box: the cursor is at the line's first character
    /// and the word is not being typed — it is sitting there whole.
    #[test]
    fn a_word_the_cursor_sits_in_front_of_is_underlined() {
        let mut screen = Screen::new(20, 3);
        let drawn = "\x1b[?2026h\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}teh word\r\n\u{2500}\u{2500}\u{2500}\x1b[1;1H\x1b[?2026l";
        crate::parser::Parser::new().advance(&mut screen, drawn.as_bytes());
        let painted = paint_of(&mut checking(&["teh"]), &screen).expect("a paint");
        assert!(painted.contains("teh"), "{painted:?}");
    }

    /// Taking an underline off means writing the box's own characters back
    /// where they stand. A blank where the underline stood over a word is not
    /// one of those: it is the box's padding, or the placeholder the model
    /// reads as blank, and a space written there is this painter drawing over
    /// the program.
    #[test]
    fn a_cell_the_box_left_blank_is_not_written_back_over() {
        let mut painter = checking(&["teh"]);
        assert!(paint_of(&mut painter, &box_holding("a teh word")).is_some());
        let blanked = box_holding("a     word");
        assert_eq!(
            ask(&mut painter, &blanked).map(|paint| String::from_utf8_lossy(&paint).into_owned()),
            None,
            "wrote over cells the box is holding blank"
        );
    }

    /// And an underline whose cells the box is not showing has not come off:
    /// the frame that can be read is the one that takes it away, however many
    /// frames later that is.
    #[test]
    fn an_underline_that_could_not_be_found_comes_off_later() {
        let mut painter = checking(&["teh"]);
        assert!(paint_of(&mut painter, &box_holding("a teh word")).is_some());
        assert!(ask(&mut painter, &box_holding("a     word")).is_none());

        let showing = box_holding("a the word");
        let taken_off = ask(&mut painter, &showing).expect("the underline taken off");
        assert!(
            String::from_utf8_lossy(&taken_off).contains("\x1b[2;5H\x1b[59m\x1b[24mthe"),
            "{taken_off:?}"
        );
    }

    /// A doubled word is one flag over two words with the space between them
    /// underlined too, and it comes off the way it went on: in one piece,
    /// space included. Taking off only the words would leave the gap underlined
    /// for the rest of the session.
    #[test]
    fn a_doubled_word_comes_off_the_way_it_went_on() {
        let learned = SharedLearned::default();
        let (mut painter, _) = spanning(0..7);
        painter.learned = learned.clone();
        let drawn = box_holding("the the cat");
        assert!(paint_of(&mut painter, &drawn).is_some(), "never underlined");

        learned
            .lock()
            .expect("learned")
            .insert("the the".to_string());
        let taken_off =
            String::from_utf8_lossy(&ask(&mut painter, &drawn).expect("taken off")).into_owned();
        assert!(
            taken_off.contains("\x1b[2;3H\x1b[59m\x1b[24mthe the"),
            "the gap or the second word kept its underline: {taken_off:?}"
        );
    }

    /// The rule whose subject is the gap underlines the gap, and an underline
    /// on a blank cell has to come off like any other. It could not: the erase
    /// re-read the line and skipped every blank, so it wrote nothing at all
    /// while the painter forgot it had drawn there.
    #[test]
    fn an_underline_drawn_on_a_blank_comes_off_too() {
        let learned = SharedLearned::default();
        let (mut painter, _) = spanning(1..3);
        painter.learned = learned.clone();
        let drawn = box_holding("a  double");
        assert!(paint_of(&mut painter, &drawn).is_some(), "never underlined");

        learned.lock().expect("learned").insert("  ".to_string());
        let taken_off =
            String::from_utf8_lossy(&ask(&mut painter, &drawn).expect("taken off")).into_owned();

        assert!(
            taken_off.contains("\x1b[2;4H\x1b[59m\x1b[24m  "),
            "the gap kept its underline: {taken_off:?}"
        );
    }

    /// The word being typed, at each of the places the cursor can be in and
    /// around it: the rule is about the word the user is still saying, and the
    /// character after the cursor is what says whether they are.
    #[test]
    fn the_cursor_at_each_edge_of_a_word_says_whether_it_is_finished() {
        let held = |text: &str, cursor: usize| {
            let line = Line {
                text: text.to_string(),
                cursor,
                spots: Vec::new(),
            };
            being_typed(&line, &(2..5))
        };

        assert!(!held("a teh", 2), "the cursor is in front of the word");
        assert!(held("a teh", 3), "the cursor is inside the word");
        assert!(held("a teh", 5), "the word ends where the typing stopped");
        assert!(held("a tehx", 5), "more of the word follows the cursor");
        assert!(!held("a teh word", 5), "the word is finished and spaced");
        assert!(!held("a teh, x", 5), "the word is finished and punctuated");
        assert!(!held("a teh word", 10), "the cursor has moved on");
    }

    /// And the fix chord is not held back with it: the word is published to the
    /// keystroke side either way, so the moment the cursor leaves the word the
    /// chord has something to claim without waiting for another frame.
    #[test]
    fn a_word_held_back_from_the_paint_is_still_offered_to_the_chord() {
        let checked = SharedChecked::default();
        let mut painter = checking(&["teh"]);
        painter.checked = checked.clone();
        assert!(
            paint_of(&mut painter, &box_holding("teh")).is_none(),
            "underlined a word still being typed"
        );

        let published = checked.lock().expect("checked");
        assert_eq!(published.line, "teh");
        assert_eq!(published.flags.len(), 1);
    }

    /// A flag that reaches across a hole the filter left is about the hole and
    /// not the line, whatever put it there, and nothing is drawn for it — while
    /// the same words on their own are still underlined.
    #[test]
    fn a_flag_reaching_across_a_removed_path_is_not_painted() {
        let drawn = box_holding("to ~/a to");

        let (mut painter, _) = spanning(0..9);
        assert!(
            ask(&mut painter, &drawn).is_none(),
            "painted a changed line"
        );
        assert!(
            ask(&mut painter, &drawn).is_none(),
            "underlined the filter's own work"
        );

        let (mut painter, _) = spanning(0..2);
        assert!(
            ask(&mut painter, &drawn).is_none(),
            "painted a changed line"
        );
        assert!(
            ask(&mut painter, &drawn).is_some(),
            "the word on its own is still flagged"
        );
    }

    /// A word the user has taught the wrapper is no longer underlined, and it
    /// stops being underlined at the next paint rather than the next check —
    /// the line has not changed, so nothing would ask about it again.
    #[test]
    fn a_word_the_user_has_learned_is_not_underlined() {
        let learned = SharedLearned::default();
        let mut painter = Painter::checking(Box::new(FakeChecker::spelling(&["teh"])));
        painter.learned = learned.clone();
        let drawn = box_holding("a teh word");
        assert!(
            ask(&mut painter, &drawn).is_none(),
            "painted a changed line"
        );
        assert!(
            ask(&mut painter, &drawn).is_some(),
            "the flag was never there"
        );

        learned.lock().expect("learned").insert("teh".to_string());

        let taken_off =
            String::from_utf8_lossy(&ask(&mut painter, &drawn).expect("taken off")).into_owned();
        assert!(
            taken_off.contains("\x1b[2;5H\x1b[59m\x1b[24mteh"),
            "the learned word was not written back plain: {taken_off:?}"
        );
        assert!(!taken_off.contains("\x1b[4:3m"), "{taken_off:?}");
        assert!(ask(&mut painter, &drawn).is_none());
    }

    /// What is learned is a word, not a place in the line: another word at the
    /// same span is a different word and is underlined as before.
    #[test]
    fn learning_one_word_leaves_the_next_one_flagged() {
        let learned = SharedLearned::default();
        learned.lock().expect("learned").insert("teh".to_string());
        let mut painter = Painter::checking(Box::new(FakeChecker::spelling(&["teh", "hte"])));
        painter.learned = learned;
        let drawn = box_holding("a hte word");

        assert!(
            ask(&mut painter, &drawn).is_none(),
            "painted a changed line"
        );
        assert!(ask(&mut painter, &drawn).is_some());
    }

    /// One flag can cover more than one row — a phrase the box wrapped — and is
    /// underlined on each of them, at the column it starts in there. The break
    /// itself has no cell and joins neither run.
    #[test]
    fn a_flag_the_box_broke_across_rows_is_drawn_on_each() {
        let mut screen = Screen::new(8, 4);
        let drawn = "\x1b[?2026h\u{2500}\u{2500}\u{2500}\r\n\u{276f}\u{a0}abcdef\r\n  ghij\r\n\u{2500}\u{2500}\u{2500}\x1b[3;8H\x1b[?2026l";
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
