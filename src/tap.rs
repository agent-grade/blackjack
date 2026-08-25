use std::fs::{File, OpenOptions};
use std::io::{self, Write};

use vte::Parser;

use crate::screen::SharedScreen;

const LOG_VAR: &str = "BLACKJACK_DEBUG_LOG";

/// The wrapped program's output on its way to the terminal, with a copy fed to
/// the screen model. The terminal is written first and nothing is held back or
/// altered, so tracking the grid cannot change what the user sees.
pub struct Tap<W: Write> {
    out: W,
    parser: Parser,
    screen: SharedScreen,
    log: Option<File>,
}

impl<W: Write> Tap<W> {
    pub fn new(out: W, screen: SharedScreen) -> Self {
        Self {
            out,
            parser: Parser::new(),
            screen,
            log: open_log(),
        }
    }

    /// The grid as the wrapped program left it — what a real session can be
    /// checked against while there is still nothing painted to look at.
    pub fn log_final_grid(&mut self) {
        let Some(log) = self.log.as_mut() else {
            return;
        };
        let Ok(screen) = self.screen.lock() else {
            return;
        };
        let (row, col) = screen.cursor();
        let _ = writeln!(
            log,
            "--- final grid, cursor at {row},{col} ---\n{}",
            screen.render()
        );
    }
}

impl<W: Write> Write for Tap<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.out.write(buf)?;
        if let Ok(mut screen) = self.screen.lock() {
            self.parser.advance(&mut *screen, &buf[..written]);
            if let Some(log) = self.log.as_mut() {
                let (row, col) = screen.cursor();
                let _ = writeln!(log, "out {written}B -> cursor {row},{col}");
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

fn open_log() -> Option<File> {
    let path = std::env::var_os(LOG_VAR)?;
    OpenOptions::new().create(true).append(true).open(path).ok()
}
