use std::fmt::Arguments;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use crate::input::{Event, SharedInput};
use crate::parser::Parser;
use crate::screen::SharedScreen;

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
    /// The last frame this tap has corrected the input model against. Claude
    /// Code redraws the whole input box, so between frames the model is a guess
    /// and at the end of one the screen is the answer.
    frames: u64,
    log: Log,
}

impl<W: Write> Tap<W> {
    pub fn new(out: W, screen: SharedScreen, input: SharedInput, log: Log) -> Self {
        Self {
            out,
            parser: Parser::new(),
            screen,
            input,
            frames: 0,
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
    /// to say — it reads the screen's account of the line before comparing it —
    /// and it covers the cursor, which nothing else ever reopens.
    fn resync(&mut self) {
        let Ok(screen) = self.screen.lock() else {
            return;
        };
        if screen.frames() == self.frames {
            return;
        }
        self.frames = screen.frames();
        let frame_line = screen.frame_line();
        drop(screen);
        let Ok(mut input) = self.input.lock() else {
            return;
        };
        // Taken on every frame, box or none, or a keystroke from an older frame
        // eats the first correction the box comes back for.
        let raced = input.raced_the_frame();
        let Some((text, cursor)) = frame_line else {
            return;
        };
        if !input.stale() && raced {
            return;
        }
        if input.resync(text, cursor) {
            self.log.line(format_args!(
                "resync \"{}\" @{}",
                escape(input.line().as_bytes()),
                input.cursor()
            ));
        }
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
        if let Ok(mut screen) = self.screen.lock() {
            self.parser.advance(&mut *screen, &buf[..written]);
            let (row, col) = screen.cursor();
            self.log
                .line(format_args!("out {written}B -> cursor {row},{col}"));
        }
        self.resync();
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
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Log(None));
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
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Log(None));
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
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Log(None));
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
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Log(None));
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

    /// History recall replaces the line from somewhere the input model cannot
    /// see, so it is recorded and then corrected: the key marks the line as owed
    /// an answer, and the frame that follows carries it.
    #[test]
    fn a_frame_corrects_a_line_the_input_model_could_only_record() {
        let input = shared_input();
        let mut keys = InputTap::new(Vec::new(), input.clone(), Log(None));
        keys.write_all(b"hi\x1b[A").expect("type");
        let screen = Arc::new(Mutex::new(Screen::new(20, 3)));
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Log(None));
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
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Log(None));
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
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Log(None));
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
        let mut out = Tap::new(Vec::new(), screen, input.clone(), Log(None));
        out.write_all(boxed("hi", 2).as_bytes()).expect("draw");
        assert_eq!(input.lock().expect("input").cursor(), 2);
    }

    #[test]
    fn the_output_tap_tracks_a_character_split_across_two_reads() {
        let screen = Arc::new(Mutex::new(Screen::new(10, 1)));
        let input = shared_input();
        let mut tap = Tap::new(Vec::new(), screen.clone(), input, Log(None));
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
