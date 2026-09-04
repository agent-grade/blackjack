use std::ops::Range;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthChar;

use crate::checker::{Checker, Flag, worded};
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
            return self.refused("no box");
        };
        if self.checker.is_none() {
            self.checker = self.arriving.try_recv().ok();
        }
        if line.text != self.line {
            self.line.clone_from(&line.text);
            self.changed = Instant::now();
            self.flags = None;
            return self.refused("line changed");
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
            let asked = prose::blanked(&line.text);
            let (flags, across): (Vec<_>, Vec<_>) = checker
                .check(&asked)
                .into_iter()
                .partition(|flag| prose::intact(&line.text, &asked, &flag.span));
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
            .flat_map(|flag| runs_of(line, &flag.span))
            .collect();
        drop(learned);
        if runs.is_empty() {
            return self.refused("nothing to underline");
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
        let paint = draw(&runs, look);
        if self.log.is_on() {
            let places: Vec<String> = runs
                .iter()
                .map(|run| format!("{},{}", run.row, run.col))
                .collect();
            self.log.line(format_args!(
                "paint {}B at [{}]",
                paint.len(),
                places.join(" ")
            ));
        }
        Some(paint)
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

/// Spans as a log line lists them.
fn spanned(flags: &[Flag]) -> String {
    flags
        .iter()
        .map(|flag| format!("{}..{}", flag.span.start, flag.span.end))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A flagged stretch of the line as it can actually be drawn: the characters
/// standing next to each other on one row, at the column the first of them
/// sits in.
struct Run {
    row: usize,
    col: usize,
    text: String,
}

/// The box may have broken a flagged stretch across two rows, and the break
/// itself has no cell — either one ends a run and starts another.
fn runs_of(line: &Line, span: &Range<usize>) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
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
            Some(run) if after == Some((row, col)) => run.text.push(character),
            _ => runs.push(Run {
                row,
                col,
                text: character.to_string(),
            }),
        }
        after = Some((row, col + character.width().unwrap_or(1)));
    }
    runs
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
fn draw(runs: &[Run], look: &Look) -> Vec<u8> {
    let mut paint = String::from("\x1b[?2026h");
    if look.dimmed {
        paint += "\x1b[22m";
    }
    for run in runs {
        paint += &format!(
            "\x1b[{};{}H\x1b[4:3m\x1b[58:2::255:0:0m{}\x1b[59m\x1b[24m",
            run.row + 1,
            run.col + 1,
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
        assert!(
            ask(&mut painter, &fixed).is_none(),
            "painted the change itself"
        );
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
            &box_holding("src/pty.rs teh"),
        );

        assert!(quiet.is_none(), "underlined a path: {quiet:?}");
        let flagged = flagged.expect("the misspelling beside it");
        assert!(flagged.contains("teh"), "{flagged:?}");
        assert!(!flagged.contains("src"), "{flagged:?}");
    }

    /// A checker reads the filter's blanks as ordinary space, so it can report
    /// the words either side of a removed path as neighbours. Such a flag is
    /// about the hole, not the line, and nothing is drawn for it — while the
    /// same words on their own are still underlined.
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
