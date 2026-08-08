use std::io::{IsTerminal, Read, Write};
use std::mem::ManuallyDrop;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::cli::Command;

const FALLBACK_SIZE: (u16, u16) = (80, 24);
const BUF_LEN: usize = 8192;
const EOT: u8 = 0x04;

pub fn run(command: &Command) -> Result<()> {
    let pty = native_pty_system()
        .openpty(inherited_size())
        .context("opening a pty")?;

    let mut builder = CommandBuilder::new(&command.program);
    builder.args(&command.args);
    if let Ok(cwd) = std::env::current_dir() {
        builder.cwd(cwd);
    }

    let mut child = pty
        .slave
        .spawn_command(builder)
        .with_context(|| format!("spawning `{}`", command.program))?;
    // The child holds its own handle; ours would keep the pty from reaching EOF.
    drop(pty.slave);

    let mut from_child = pty.master.try_clone_reader().context("reading the pty")?;
    let to_child = pty.master.take_writer().context("writing to the pty")?;

    // portable_pty::UnixMasterWriter writes "\n" + EOT when dropped. That newline
    // is an Enter the wrapped program never received, so this writer is never
    // dropped and end-of-input is signalled deliberately below.
    let mut to_child = ManuallyDrop::new(to_child);
    let piped_stdin = !std::io::stdin().is_terminal();

    let _raw = RawMode::enable();

    // Detached: this read blocks, and the child exiting must not wait on a keystroke.
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        pump(&mut stdin, &mut *to_child);
        if piped_stdin {
            // The first EOT flushes a partial line, the second ends the input.
            // Only for piped stdin: on a terminal, stdin ending means the
            // terminal itself is gone, and 0x04 into a raw-mode child would
            // arrive as the user pressing ctrl-D.
            let _ = to_child.write_all(&[EOT, EOT]);
            let _ = to_child.flush();
        }
    });

    let mut stdout = std::io::stdout();
    pump(&mut from_child, &mut stdout);

    child.wait().context("waiting for the wrapped command")?;
    Ok(())
}

/// Copies until either side gives out. A broken pipe here is the ordinary way
/// a wrapped session ends, so it reads as completion rather than as an error.
fn pump(from: &mut impl Read, to: &mut impl Write) {
    let mut buf = [0u8; BUF_LEN];
    loop {
        let read = match from.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if to.write_all(&buf[..read]).is_err() || to.flush().is_err() {
            return;
        }
    }
}

fn inherited_size() -> PtySize {
    pty_size(crossterm::terminal::size().ok())
}

/// A zero dimension is reported rather than refused when stdout is not a
/// terminal, and a 0x0 pty gives a TUI nothing to render into — so it is
/// treated as "unknown" alongside an outright failure.
fn pty_size(reported: Option<(u16, u16)>) -> PtySize {
    let (cols, rows) = match reported {
        Some((cols, rows)) if cols > 0 && rows > 0 => (cols, rows),
        _ => FALLBACK_SIZE,
    };
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Raw mode for as long as the wrapped command runs — the child's own terminal
/// handling is the only one that should see the keystrokes. Off outside a tty,
/// which is what makes `blackjack -- echo hi` work in a pipeline.
struct RawMode {
    enabled: bool,
}

impl RawMode {
    fn enable() -> Self {
        let enabled = std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal()
            && crossterm::terminal::enable_raw_mode().is_ok();
        Self { enabled }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if self.enabled {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherits_a_reported_size() {
        let size = pty_size(Some((120, 40)));
        assert_eq!((size.cols, size.rows), (120, 40));
    }

    #[test]
    fn falls_back_when_the_size_is_unknown() {
        let size = pty_size(None);
        assert_eq!((size.cols, size.rows), FALLBACK_SIZE);
    }

    #[test]
    fn refuses_a_zero_dimension() {
        for reported in [(0, 0), (0, 40), (120, 0)] {
            let size = pty_size(Some(reported));
            assert_eq!((size.cols, size.rows), FALLBACK_SIZE, "for {reported:?}");
        }
    }
}
