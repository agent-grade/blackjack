use std::fmt::Arguments;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use vte::Parser;

use crate::input::{Event, Input};
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
    log: Log,
}

impl<W: Write> Tap<W> {
    pub fn new(out: W, screen: SharedScreen, log: Log) -> Self {
        Self {
            out,
            parser: Parser::new(),
            screen,
            log,
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
    input: Input,
    log: Log,
}

impl<W: Write> InputTap<W> {
    pub fn new(to: W, log: Log, from_a_pipe: bool) -> Self {
        Self {
            to,
            input: Input::new(from_a_pipe),
            log,
        }
    }
}

impl<W: Write> Write for InputTap<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.to.write(buf)?;
        self.input.feed(&buf[..written]);
        // Drained whether or not anything is listening, or a session with the
        // log off would hold every line it ever submitted.
        let events = self.input.take_events();
        // Escaping allocates, and this runs per keystroke.
        if !self.log.is_on() {
            return Ok(written);
        }
        self.log.line(format_args!(
            "in {written}B \"{}\" -> \"{}\" @{}",
            escape(&buf[..written]),
            escape(self.input.line().as_bytes()),
            self.input.cursor()
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
