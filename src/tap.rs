use std::fmt::Arguments;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::input::{Event, SharedInput};
use crate::painter::Painter;
use crate::parser::Parser;
use crate::screen::{Line, Look, SharedScreen};

const LOG_VAR: &str = "BLACKJACK_DEBUG_LOG";

/// One handle behind one lock, because the two taps run on different threads:
/// independent appenders interleave inside a line, and the grid dump is far too
/// long to rely on a write landing whole.
#[derive(Clone)]
pub struct Log(Option<Arc<Mutex<File>>>);

impl Log {
    pub fn open() -> Self {
        Self(
            std::env::var_os(LOG_VAR)
                .and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok())
                .map(|file| Arc::new(Mutex::new(file))),
        )
    }

    fn is_on(&self) -> bool {
        self.0.is_some()
    }

    fn line(&self, line: Arguments) {
        let Some(file) = &self.0 else {
            return;
        };
        if let Ok(mut file) = file.lock() {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// The wrapped program's output on its way to the terminal, with a copy fed to
/// the screen model. The terminal is written first and nothing is held back or
/// altered, so tracking the grid cannot change what the user sees.
pub struct Tap<W: Write> {
    out: W,
    parser: Parser,
    screen: SharedScreen,
    input: SharedInput,
    /// The last frame this tap has answered. Claude Code redraws the whole
    /// input box, so between frames the model is a guess and at the end of one
    /// the screen is the answer.
    frames: u64,
    painter: Painter,
    log: Log,
}

impl<W: Write> Tap<W> {
    pub fn new(
        out: W,
        screen: SharedScreen,
        input: SharedInput,
        painter: Painter,
        log: Log,
    ) -> Self {
        Self {
            out,
            parser: Parser::new(),
            screen,
            input,
            frames: 0,
            painter,
            log,
        }
    }

    /// At the end of a frame the program has finished saying what it is
    /// showing, so where the two accounts of the line disagree the screen wins:
    /// history recall and anything else this model could only record arrives
    /// here as a line it never saw typed. A disagreement while the user is
    /// typing is only the frame being older than the keystrokes, so that one is
    /// left alone; a key the model could not apply is not, and asks for the
    /// correction outright. What counts as a disagreement is the input model's
    /// to say, and it covers the cursor, which nothing else ever reopens. The
    /// line it read the screen's account as goes on to the painter either way:
    /// corrected or not, that is the line the user typed.
    fn resync(&mut self, frame_line: Option<&Line>) -> Option<Line> {
        let Ok(mut input) = self.input.lock() else {
            return frame_line.cloned();
        };
        // Taken on every frame, box or none, or a keystroke from an older frame
        // eats the first correction the box comes back for.
        let raced = input.raced_the_frame();
        let typed = input.rejoin(frame_line?);
        if (input.stale() || !raced) && input.resync(&typed) {
            self.log.line(format_args!(
                "resync \"{}\" @{}",
                escape(input.line().as_bytes()),
                input.cursor()
            ));
        }
        Some(typed)
    }

    /// The wrapped program has gone quiet and the painter's own clock has run
    /// out. Nothing has closed a frame, so the input model is left alone —
    /// there is nothing new for the screen to correct it against — and the line
    /// the last frame left is read again, which is the one that has been held
    /// still long enough to be worth checking.
    pub fn tick(&mut self) {
        let Some(look) = self.screen.lock().ok().map(|screen| screen.look()) else {
            return;
        };
        let typed = look.line.as_ref().map(|line| self.rejoined(line));
        self.paint(typed.as_ref(), &look);
    }

    /// The line as the input model reads it, which is the line the user typed
    /// rather than the one the box wrapped. A model that cannot be read — a
    /// thread panicked holding it — leaves the screen's own reading standing:
    /// it carries the box's wrapping, which is worse than the model's answer
    /// and much better than telling the painter the box has gone.
    fn rejoined(&self, line: &Line) -> Line {
        match self.input.lock() {
            Ok(input) => input.rejoin(line),
            Err(_) => line.clone(),
        }
    }

    fn paint(&mut self, line: Option<&Line>, look: &Look) {
        let Some(paint) = self.painter.paint(line, look) else {
            return;
        };
        // Written past the parser, so the model of the screen the program
        // believes it is drawing on is never told about it; and a paint that
        // will not go is dropped rather than failing output that has already
        // landed.
        let _ = self.out.write_all(&paint);
        let _ = self.out.flush();
        self.log.line(format_args!("paint {}B", paint.len()));
    }

    /// How long this tap may wait on the wrapped program before it has
    /// something to draw on its own.
    pub fn wake_in(&self) -> Option<Duration> {
        self.painter.wake_in()
    }

    /// The grid as the wrapped program left it — what a real session can be
    /// checked against while there is still nothing painted to look at.
    pub fn log_final_grid(&mut self) {
        // The render is the whole grid, built before `line` can decline it.
        if !self.log.is_on() {
            return;
        }
        let Ok(screen) = self.screen.lock() else {
            return;
        };
        let (row, col) = screen.cursor();
        self.log.line(format_args!(
            "--- final grid, cursor at {row},{col} ---\n{}",
            screen.render()
        ));
    }
}

impl<W: Write> Write for Tap<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.out.write(buf)?;
        let ended = {
            let Ok(mut screen) = self.screen.lock() else {
                return Ok(written);
            };
            self.parser.advance(&mut *screen, &buf[..written]);
            let (row, col) = screen.cursor();
            self.log
                .line(format_args!("out {written}B -> cursor {row},{col}"));
            let frames = screen.frames();
            let closed = frames != self.frames;
            self.frames = frames;
            closed.then(|| screen.look())
        };
        let Some(look) = ended else {
            return Ok(written);
        };
        let typed = self.resync(look.line.as_ref());
        self.paint(typed.as_ref(), &look);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// The user's keystrokes on their way to the wrapped program, with a copy fed
/// to the input model. Same order as the output side: the pty is written first
/// and nothing is held back or altered.
pub struct InputTap<W: Write> {
    to: W,
    input: SharedInput,
    log: Log,
}

impl<W: Write> InputTap<W> {
    pub fn new(to: W, input: SharedInput, log: Log) -> Self {
        Self { to, input, log }
    }
}

impl<W: Write> Write for InputTap<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.to.write(buf)?;
        let Ok(mut input) = self.input.lock() else {
            return Ok(written);
        };
        input.feed(&buf[..written]);
        // Drained whether or not anything is listening, or a session with the
        // log off would hold every line it ever submitted.
        let events = input.take_events();
        // Escaping allocates, and this runs per keystroke.
        if !self.log.is_on() {
            return Ok(written);
        }
        self.log.line(format_args!(
            "in {written}B \"{}\" -> \"{}\" @{}",
            escape(&buf[..written]),
            escape(input.line().as_bytes()),
            input.cursor()
        ));
        for event in events {
            match event {
                Event::Submit { line, edited } => {
                    let shape = if edited { "submit-dirty" } else { "submit" };
                    self.log
                        .line(format_args!("{shape} \"{}\"", escape(line.as_bytes())));
                }
                Event::Edit => self.log.line(format_args!("edit")),
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.to.flush()
    }
}

/// Bytes as a log line can carry them: an escape byte has to survive legibly
/// while typed text stays readable.
fn escape(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    for &byte in bytes {
        match byte {
            b'"' | b'\\' => out.extend_from_slice(&[b'\\', byte]),
            0x20..=0x7e | 0x80.. => out.push(byte),
            _ => out.extend_from_slice(format!("\\x{byte:02x}").as_bytes()),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checker::Checker;
    use crate::screen::Screen;

    fn shared_input() -> SharedInput {
        Arc::new(Mutex::new(crate::input::Input::new(false)))
    }

    /// Both taps are fed by a pty read, which cuts a character wherever it
    /// happens to end. The models are only as true as the byte stream they are
    /// handed, so each tap has to survive the cut.
    fn split_reads() -> [&'static [u8]; 2] {
        [b"\xc3", b"\xa9/\xe4\xb8\xad"]
    }

    /// A frame of Claude Code's input box: the prompt row between its two
    /// borders, the cursor left where the program is holding it, and the frame
    /// closed.
    fn boxed(text: &str, cursor: usize) -> String {
        format!(
            "\x1b[1;1H\u{2500}\u{2500}\u{2500}\x1b[2;1H\u{276f}\u{a0}{text}\x1b[3;1H\u{2500}\u{2500}\u{2500}\x1b[2;{}H\x1b[?2026l",
            cursor + 3
        )
    }

    /// The box wrapping one line onto two rows: the row above ends where the
    /// box broke it, the row below carries the rest, and the cursor is left
    /// where the program is holding it.
    fn wrapped(above: &str, below: &str, cursor: (usize, usize)) -> String {
        format!(
            "\x1b[1;1H\u{2500}\u{2500}\u{2500}\x1b[2;1H\u{276f}\u{a0}{above}\x1b[3;1H  {below}\x1b[4;1H\u{2500}\u{2500}\u{2500}\x1b[{};{}H\x1b[?2026l",
            cursor.0 + 1,
            cursor.1 + 1
        )
    }

    /// Everything the terminal received, paints included.
    fn painted(out: &Tap<Vec<u8>>) -> String {
        String::from_utf8_lossy(&out.out).into_owned()
    }

    /// What the underline for `teh` in a one-row box looks like on the wire.
    const UNDERLINED: &[u8] =
        b"\x1b[?2026h\x1b[2;3H\x1b[4:3m\x1b[58:2::255:0:0mteh\x1b[59m\x1b[24m\x1b[2;6H\x1b[?2026l";

    /// A tap over a box of the given shape, painting what its checker flags.
    fn painting(
        checker: Box<dyn Checker + Send>,
        rows: usize,
        input: &SharedInput,
    ) -> Tap<Vec<u8>> {
        let screen = Arc::new(Mutex::new(Screen::new(20, rows)));
        Tap::new(
            Vec::new(),
            screen,
            input.clone(),
            Painter::checking(checker),
            Log(None),
        )
    }

    /// The real engine behind the paint, which is the only thing that can say
    /// whether the line the checker was handed is a line English knows.
    fn harper(input: &SharedInput) -> Tap<Vec<u8>> {
        painting(
            Box::new(crate::harper::HarperChecker::new(&[], None)),
            4,
            input,
        )
    }

    fn fake(flagged: &[&str]) -> Tap<Vec<u8>> {
        let checker = Box::new(crate::checker::FakeChecker::spelling(flagged));
        painting(checker, 3, &shared_input())
    }

    fn typed(input: &SharedInput, keys: &[u8]) {
        InputTap::new(Vec::new(), input.clone(), Log(None))
            .write_all(keys)
            .expect("type");
    }

    /// A session that never ends: both captures replayed through the whole
    /// stack — screen, input model, real dictionary, painter — thousands of
    /// frames deep. What is being watched for is anything that accumulates,
    /// and the signal for that is memory: a structure appended to once a frame
    /// and never read costs no time at all, so a clock would never see it.
    /// Time is checked too, warm against warm — the first pass of anything is
    /// slower than the rest and comparing to it would flatter any result.
    #[test]
    fn a_session_thousands_of_frames_long_neither_grows_nor_slows() {
        const CAPTURES: [&[u8]; 2] = [
            include_bytes!("../tests/fixtures/claude-tui-40x100.bin"),
            include_bytes!("../tests/fixtures/claude-wrap-40x100.bin"),
        ];
        let input = shared_input();
        let checker = Box::new(crate::harper::HarperChecker::new(&[], None));
        let screen = Arc::new(Mutex::new(Screen::new(100, 40)));
        let mut out = Tap::new(
            Vec::new(),
            screen,
            input,
            Painter::checking(checker),
            Log(None),
        );

        // One frame per write, which is what the tap sees from a pty: handing
        // it a whole capture at once would close one frame and skip the other
        // seventeen, and the per-frame work is the point.
        let frames: Vec<&[u8]> = CAPTURES
            .iter()
            .flat_map(|capture| frames_of(capture))
            .collect();
        let pass = |out: &mut Tap<Vec<u8>>, passes: usize| {
            let started = std::time::Instant::now();
            for _ in 0..passes {
                for frame in &frames {
                    out.write_all(frame).expect("replay");
                    out.out.clear();
                }
            }
            started.elapsed() / passes as u32
        };

        pass(&mut out, 30);
        let (warm, settled) = (pass(&mut out, 30), resident());
        pass(&mut out, 240);
        let (late, held) = (pass(&mut out, 30), resident());

        // The one queue in this path that nothing here drains — the input tap
        // takes them, and there is no input tap in a replay. A frame that left
        // something in it would fill it for the length of a session, and it is
        // small enough per frame that resident memory would not notice.
        assert!(
            out.input.lock().expect("input").take_events().is_empty(),
            "a frame left something in the event queue"
        );
        let grown = held.saturating_sub(settled);
        // Measured drift over this run is tens of kilobytes. The bar catches
        // gross accumulation — the kind a day-long session would feel — and
        // not a slow trickle, which hides in an allocator already holding the
        // dictionary; that is what the queue assertion above is for.
        assert!(
            grown < 512 * 1024,
            "{grown} bytes more resident after 8000 frames"
        );
        assert!(
            late < warm * 3,
            "a pass costs {late:?} after 8000 frames against {warm:?} warm"
        );
    }

    /// The capture cut where the program ends each frame, so a replay arrives
    /// the way the program sent it rather than in one piece.
    fn frames_of(capture: &'static [u8]) -> Vec<&'static [u8]> {
        const CLOSE: &[u8] = b"\x1b[?2026l";
        let mut frames = Vec::new();
        let mut rest = capture;
        while let Some(at) = rest.windows(CLOSE.len()).position(|window| window == CLOSE) {
            let (frame, after) = rest.split_at(at + CLOSE.len());
            frames.push(frame);
            rest = after;
        }
        frames
    }

    /// This process's resident memory. Other tests run beside this one, so it
    /// is only worth reading as a difference across a run long enough to
    /// dwarf them.
    fn resident() -> usize {
        let reported = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .expect("ps");
        String::from_utf8_lossy(&reported.stdout)
            .trim()
            .parse::<usize>()
            .expect("a resident size")
            * 1024
    }

    /// A real session at 40x100: 115 characters typed into the box, which wraps
    /// them onto a second row, then shift+enter and one more word.
    fn wrap_frames(from: usize, to: usize) -> &'static [u8] {
        const CAPTURE: &[u8] = include_bytes!("../tests/fixtures/claude-wrap-40x100.bin");
        const CLOSE: &[u8] = b"\x1b[?2026l";
        let end = |frames: usize| {
            let mut at = 0;
            for _ in 0..frames {
                let rest = &CAPTURE[at..];
                at += rest
                    .windows(CLOSE.len())
                    .position(|w| w == CLOSE)
                    .expect("a frame")
                    + CLOSE.len();
            }
            at
        };
        &CAPTURE[end(from)..end(to)]
    }

    /// What the capture's session typed, up to the break.
    const TYPED: &str = "one two three four five six seven eight nine ten eleven twelve \
                         thirteen fourteen fifteen sixteen seventeen eighteen";

    /// The box wraps at a space and does not draw it, so the row above ends
    /// short — exactly as it does when the user broke the line. A model with
    /// nothing to say about that place gets the space back, not a break.
    #[test]
    fn a_wrapped_row_is_rejoined_with_the_space_the_box_did_not_draw() {
        let input = shared_input();
        let mut keys = InputTap::new(Vec::new(), input.clone(), Log(None));
        keys.write_all(b"\x1b[A").expect("recall");
        let screen = Arc::new(Mutex::new(Screen::new(100, 40)));
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Painter::off(), Log(None));
        out.write_all(wrap_frames(0, 6)).expect("draw");
        assert_eq!(input.lock().expect("input").line(), TYPED);
    }

    /// The same rows, drawn the same way, where the model does know: it holds
    /// the break the user typed at that index, so that one survives the frame
    /// and only the wrap becomes a space.
    #[test]
    fn a_break_the_user_typed_survives_the_frame_that_redraws_it() {
        let input = shared_input();
        let mut keys = InputTap::new(Vec::new(), input.clone(), Log(None));
        keys.write_all(TYPED.as_bytes()).expect("type");
        keys.write_all(b"\x1b[13;2u").expect("shift+enter");
        keys.write_all(b"after\x1b[D").expect("type on");
        let screen = Arc::new(Mutex::new(Screen::new(100, 40)));
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Painter::off(), Log(None));
        out.write_all(wrap_frames(0, 7)).expect("draw");
        out.write_all(wrap_frames(7, 8)).expect("redraw");
        let input = input.lock().expect("input");
        assert_eq!(input.line(), format!("{TYPED}\nafter"));
        assert_eq!(input.cursor(), TYPED.chars().count() + 6);
    }

    /// A pty read ends wherever the kernel filled the buffer, which is often in
    /// the middle of a redraw — the cursor parked at home, the box half
    /// rewritten. The line is taken at the frame boundary instead, so what a
    /// read happens to catch never reaches the model.
    #[test]
    fn a_read_ending_mid_redraw_is_answered_by_the_last_whole_frame() {
        let input = shared_input();
        let mut keys = InputTap::new(Vec::new(), input.clone(), Log(None));
        keys.write_all(b"\x1b[A").expect("recall");
        let screen = Arc::new(Mutex::new(Screen::new(100, 40)));
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Painter::off(), Log(None));
        let redrawing = [wrap_frames(0, 6), &wrap_frames(6, 7)[..80]].concat();
        out.write_all(&redrawing).expect("draw");
        let input = input.lock().expect("input");
        assert_eq!(input.line(), TYPED);
        assert_eq!(input.cursor(), TYPED.chars().count());
    }

    /// A frame with no box on it — a dialog, a program that draws no prompt at
    /// all — is still a frame: the keystrokes before it are answered by it,
    /// not saved up to eat the correction the box comes back for.
    #[test]
    fn a_frame_without_a_box_still_answers_the_keystrokes_before_it() {
        let input = shared_input();
        let mut keys = InputTap::new(Vec::new(), input.clone(), Log(None));
        keys.write_all(b"hi").expect("type");
        let screen = Arc::new(Mutex::new(Screen::new(20, 3)));
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Painter::off(), Log(None));
        out.write_all(b"\x1b[?2026h\x1b[2Jbusy\x1b[?2026l")
            .expect("no box");
        let redrawn = format!("\x1b[2J{}", boxed("other", 5));
        out.write_all(redrawn.as_bytes()).expect("draw");
        assert_eq!(input.lock().expect("input").line(), "other");
    }

    /// The debug log is a record of corrections: the frame that fills the empty
    /// line makes one, the frame repeating it makes none, and the frame that
    /// only moves the cursor makes one — the cursor being half the account.
    #[test]
    fn only_a_frame_that_corrects_the_line_is_logged() {
        let path = std::env::temp_dir().join(format!("blackjack-resync-{}", std::process::id()));
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .expect("a log to write");
        let input = shared_input();
        let screen = Arc::new(Mutex::new(Screen::new(20, 3)));
        let mut out = Tap::new(
            Vec::new(),
            screen,
            input,
            Painter::off(),
            Log(Some(Arc::new(Mutex::new(file)))),
        );
        out.write_all(boxed("hi", 2).as_bytes()).expect("draw");
        out.write_all(boxed("hi", 2).as_bytes()).expect("redraw");
        out.write_all(boxed("hi", 1).as_bytes())
            .expect("move the cursor");
        let logged = std::fs::read_to_string(&path).expect("the log back");
        let _ = std::fs::remove_file(&path);
        assert_eq!(logged.matches("resync").count(), 2);
    }

    /// The underline is written to the terminal after the program's own frame,
    /// and past the parser: the model of the screen the program believes it is
    /// drawing on counts the program's two frames and not the paint's own.
    #[test]
    fn a_misspelling_in_the_box_is_painted_after_the_frame() {
        let input = shared_input();
        let screen = Arc::new(Mutex::new(Screen::new(20, 3)));
        let checker = Box::new(crate::checker::FakeChecker::spelling(&["teh"]));
        let mut out = Tap::new(
            Vec::new(),
            screen.clone(),
            input,
            Painter::checking(checker),
            Log(None),
        );
        // The first frame is the one that changed the line; the paint is on the
        // frame after, once there is a line that has held still.
        out.write_all(boxed("teh", 3).as_bytes()).expect("draw");
        out.write_all(boxed("teh", 3).as_bytes()).expect("redraw");
        assert!(out.out.ends_with(UNDERLINED), "{:?}", painted(&out));
        assert_eq!(screen.lock().expect("screen").frames(), 2);
    }

    /// The paint goes on at the end of a frame, not at the end of a read: a
    /// read carrying no frame of its own leaves the terminal as the last one
    /// left it.
    #[test]
    fn a_read_that_ends_no_frame_paints_nothing() {
        let mut out = fake(&["teh"]);
        out.write_all(boxed("teh", 3).as_bytes()).expect("draw");
        out.write_all(boxed("teh", 3).as_bytes()).expect("redraw");
        let painted = out.out.len();
        let home = b"\x1b[1;1H";
        out.write_all(home).expect("a read with no frame in it");
        assert_eq!(out.out.len(), painted + home.len());
    }

    /// A thread died holding the input model. The box is still on the screen,
    /// so the screen's own reading of the line stands in: what the painter must
    /// never be told is that the box has gone, which is what makes it throw
    /// away everything it knows about the line.
    #[test]
    fn a_model_that_cannot_be_read_is_not_a_box_that_left() {
        let input = shared_input();
        let held = input.clone();
        let died = std::thread::spawn(move || {
            let _holding = held.lock();
            panic!("a thread died holding the input model");
        });
        assert!(died.join().is_err(), "the thread was supposed to die");

        let checker = Box::new(crate::checker::FakeChecker::spelling(&["teh"]));
        let mut out = painting(checker, 3, &input);
        out.write_all(boxed("teh", 3).as_bytes()).expect("draw");
        out.write_all(boxed("teh", 3).as_bytes()).expect("redraw");

        assert!(out.out.ends_with(UNDERLINED), "{:?}", painted(&out));
    }

    /// Whatever the paint did to the terminal, the program's cursor is where
    /// the program left it. The tests around this one hold the paint's exact
    /// bytes, a cursor move among them, and they held it once when it was
    /// wrong — so this one asserts nothing it was told: it replays what the
    /// terminal saw and what the program alone would have said, and the two
    /// cursors have to agree.
    #[test]
    fn the_paint_leaves_the_cursor_where_the_program_put_it() {
        let mut out = fake(&["teh"]);
        // Held at the start of the line, not at the end of the flagged word:
        // there the underline ends where the cursor already is, and a paint
        // that never put it back would look like one that did.
        out.write_all(boxed("teh", 0).as_bytes()).expect("draw");
        out.write_all(boxed("teh", 0).as_bytes()).expect("redraw");
        assert!(painted(&out).contains("\x1b[4:3m"), "nothing was painted");

        let replay = |bytes: &[u8]| {
            let mut terminal = Screen::new(20, 3);
            Parser::new().advance(&mut terminal, bytes);
            terminal.look().cursor
        };
        let unwrapped = boxed("teh", 0).repeat(2);

        assert_eq!(replay(&out.out), replay(unwrapped.as_bytes()));
    }

    /// The user stops typing and the program stops drawing: the frame that
    /// carried the last keystroke is the one that changed the line, so nothing
    /// is coming to paint on. The wait is the tap's own, and running it out is
    /// what puts the underline up.
    #[test]
    fn a_line_nothing_redraws_is_painted_when_the_wait_runs_out() {
        let mut out = fake(&["teh"]);
        out.write_all(boxed("teh", 3).as_bytes()).expect("draw");
        assert!(!out.out.ends_with(UNDERLINED), "painted a changed line");
        assert!(out.wake_in().is_some(), "nothing left to wait for");
        out.tick();
        assert!(out.out.ends_with(UNDERLINED), "{:?}", out.out);
        assert!(out.wake_in().is_none(), "still waiting for something");
    }

    /// A word the box broke to wrap it is one word. The checker is handed the
    /// line as the user typed it, so a real dictionary is asked about
    /// `misspelled` rather than about the two halves of it the box is showing.
    #[test]
    fn a_word_the_box_broke_is_checked_whole() {
        let input = shared_input();
        typed(&input, b"misspelled");
        let mut out = harper(&input);
        out.write_all(wrapped("misspe", "lled", (2, 6)).as_bytes())
            .expect("draw");
        out.write_all(wrapped("misspe", "lled", (2, 6)).as_bytes())
            .expect("redraw");
        assert!(!painted(&out).contains("\x1b[4:3m"), "{:?}", painted(&out));
    }

    /// The box wraps at a space and does not draw it. The line the checker sees
    /// has the space back, so the word after it is a word, and it is underlined
    /// once — where the box is showing it, on the row it wrapped onto.
    #[test]
    fn a_word_after_a_wrap_is_underlined_where_the_box_wrapped_it() {
        let input = shared_input();
        typed(&input, b"one teh");
        let mut out = harper(&input);
        out.write_all(wrapped("one", "teh", (2, 5)).as_bytes())
            .expect("draw");
        out.write_all(wrapped("one", "teh", (2, 5)).as_bytes())
            .expect("redraw");
        let painted = painted(&out);
        assert!(
            painted.contains("\x1b[3;3H\x1b[4:3m\x1b[58:2::255:0:0mteh"),
            "{painted:?}"
        );
        assert_eq!(painted.matches("\x1b[4:3m").count(), 1, "{painted:?}");
    }

    /// A break the user typed is not a wrap: it stays in the line the checker
    /// reads, so the words either side of it stay two words rather than being
    /// run together into one the dictionary has never heard of.
    #[test]
    fn a_break_the_user_typed_keeps_the_words_apart() {
        let input = shared_input();
        typed(&input, b"hte\x1b[13;2ucat");
        let mut out = harper(&input);
        out.write_all(wrapped("hte", "cat", (2, 5)).as_bytes())
            .expect("draw");
        out.write_all(wrapped("hte", "cat", (2, 5)).as_bytes())
            .expect("redraw");
        let painted = painted(&out);
        assert!(
            painted.contains("\x1b[2;3H\x1b[4:3m\x1b[58:2::255:0:0mhte"),
            "{painted:?}"
        );
        assert_eq!(painted.matches("\x1b[4:3m").count(), 1, "{painted:?}");
    }

    /// History recall replaces the line from somewhere the input model cannot
    /// see, so it is recorded and then corrected: the key marks the line as owed
    /// an answer, and the frame that follows carries it.
    #[test]
    fn a_frame_corrects_a_line_the_input_model_could_only_record() {
        let input = shared_input();
        let mut keys = InputTap::new(Vec::new(), input.clone(), Log(None));
        keys.write_all(b"hi\x1b[A").expect("type");
        let screen = Arc::new(Mutex::new(Screen::new(20, 3)));
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Painter::off(), Log(None));
        out.write_all(boxed("recalled", 8).as_bytes())
            .expect("draw");
        let input = input.lock().expect("input");
        assert_eq!(input.line(), "recalled");
        assert_eq!(input.cursor(), 8);
    }

    /// The screen only wins where the two disagree — a frame that says what the
    /// model already holds must not move the cursor the user just moved. The
    /// second frame is the quiet one: the first carries the keystrokes.
    #[test]
    fn a_frame_that_agrees_leaves_the_line_alone() {
        let input = shared_input();
        let mut keys = InputTap::new(Vec::new(), input.clone(), Log(None));
        keys.write_all(b"hi\x1b[D").expect("type");
        let screen = Arc::new(Mutex::new(Screen::new(20, 3)));
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Painter::off(), Log(None));
        out.write_all(boxed("hi", 1).as_bytes()).expect("draw");
        out.write_all(boxed("hi", 1).as_bytes()).expect("redraw");
        let input = input.lock().expect("input");
        assert_eq!(input.line(), "hi");
        assert_eq!(input.cursor(), 1);
    }

    /// A frame is drawn from what the program had when it began drawing, so one
    /// that lands while the user is still typing is older than this model
    /// rather than righter than it, and takes nothing back.
    #[test]
    fn a_frame_older_than_the_keystrokes_does_not_take_the_line_back() {
        let input = shared_input();
        let mut keys = InputTap::new(Vec::new(), input.clone(), Log(None));
        keys.write_all(b"ab").expect("type");
        let screen = Arc::new(Mutex::new(Screen::new(20, 3)));
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Painter::off(), Log(None));
        out.write_all(boxed("a", 1).as_bytes()).expect("draw");
        let input = input.lock().expect("input");
        assert_eq!(input.line(), "ab");
        assert_eq!(input.cursor(), 2);
    }

    /// A recalled line can read exactly like the one it replaced, so agreement
    /// is not enough on its own: a key this model records but cannot carry out
    /// leaves the line owed an answer whatever the frame turns out to say.
    #[test]
    fn a_key_that_could_not_be_applied_is_answered_by_an_agreeing_frame_too() {
        let input = shared_input();
        let mut keys = InputTap::new(Vec::new(), input.clone(), Log(None));
        keys.write_all(b"hi\x1b[D\x1b[A").expect("type");
        let screen = Arc::new(Mutex::new(Screen::new(20, 3)));
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Painter::off(), Log(None));
        out.write_all(boxed("hi", 2).as_bytes()).expect("draw");
        assert_eq!(input.lock().expect("input").cursor(), 2);
    }

    #[test]
    fn the_output_tap_tracks_a_character_split_across_two_reads() {
        let screen = Arc::new(Mutex::new(Screen::new(10, 1)));
        let input = shared_input();
        let mut tap = Tap::new(Vec::new(), screen.clone(), input, Painter::off(), Log(None));
        for read in split_reads() {
            tap.write_all(read).expect("write");
        }
        assert_eq!(screen.lock().expect("screen").line(0).trim_end(), "é/中");
    }

    #[test]
    fn the_input_tap_tracks_a_character_split_across_two_reads() {
        let input = shared_input();
        let mut tap = InputTap::new(Vec::new(), input.clone(), Log(None));
        for read in split_reads() {
            tap.write_all(read).expect("write");
        }
        assert_eq!(input.lock().expect("input").line(), "é/中");
    }
}
