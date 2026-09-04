use std::io::{IsTerminal, Read, Write};
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM, SIGWINCH};
use signal_hook::iterator::Signals;

use crate::cli::Command;
use crate::screen::{Screen, SharedScreen};
use crate::tap::{InputTap, Log, Tap};

const FALLBACK_SIZE: (u16, u16) = (80, 24);
const BUF_LEN: usize = 8192;
const EOT: u8 = 0x04;
// libc exposes no NSIG on macOS. The lookup is a round trip through one
// strsignal table, so a range wide enough for Linux's realtime signals costs
// nothing here: this platform's signals above 31 have no name to match.
const HIGHEST_SIGNAL: i32 = 64;

static RAW_ACTIVE: AtomicBool = AtomicBool::new(false);

type Master = Arc<Mutex<Box<dyn MasterPty + Send>>>;

pub fn run(command: &Command) -> Result<u32> {
    let size = inherited_size();
    let pty = native_pty_system().openpty(size).context("opening a pty")?;
    let (cols, rows) = (size.cols as usize, size.rows as usize);
    let screen: SharedScreen = Arc::new(Mutex::new(Screen::new(cols, rows)));

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
    let master: Master = Arc::new(Mutex::new(pty.master));

    // portable_pty::UnixMasterWriter writes "\n" + EOT when dropped. That newline
    // is an Enter the wrapped program never received, so this writer is never
    // dropped and end-of-input is signalled deliberately below.
    let mut to_child = ManuallyDrop::new(to_child);
    let piped_stdin = !std::io::stdin().is_terminal();
    let log = Log::open();

    // Registered before raw mode is entered: a signal arriving in between would
    // otherwise hit the default disposition and leave the terminal raw.
    watch_signals(Arc::clone(&master), Arc::clone(&screen)).context("watching for signals")?;
    let _raw = RawMode::enable();

    // Detached: this read blocks, and the child exiting must not wait on a keystroke.
    let typed = log.clone();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut to_child = InputTap::new(&mut *to_child, typed, piped_stdin);
        pump(&mut stdin, &mut to_child);
        if piped_stdin {
            // The first EOT flushes a partial line, the second ends the input.
            // Only for piped stdin: on a terminal, stdin ending means the
            // terminal itself is gone, and 0x04 into a raw-mode child would
            // arrive as the user pressing ctrl-D.
            let _ = to_child.write_all(&[EOT, EOT]);
            let _ = to_child.flush();
        }
    });

    let mut tap = Tap::new(std::io::stdout(), screen, log);
    pump(&mut from_child, &mut tap);
    tap.log_final_grid();

    let status = child.wait().context("waiting for the wrapped command");
    // The wrapped program's terminal dies with this handle, so it is held until
    // the session is genuinely over rather than left to the signal thread.
    drop(master);
    Ok(exit_code(&status?))
}

/// portable-pty reports a signal death as code 1 plus a signal *name*, so
/// propagating its code verbatim would have blackjack claim an ordinary failure
/// where the shell reports 128 + the signal number. The name is turned back into
/// its number through the same `strsignal` table that produced it, which is what
/// keeps this from depending on the locale that table is rendered in.
fn exit_code(status: &ExitStatus) -> u32 {
    let Some(died_from) = status.signal() else {
        return status.exit_code();
    };
    (1..=HIGHEST_SIGNAL)
        .find(|&signal| signal_name(signal).as_deref() == Some(died_from))
        .map_or_else(|| status.exit_code(), |signal| 128 + signal as u32)
}

fn signal_name(signal: i32) -> Option<String> {
    // SAFETY: the returned pointer stays valid until this thread's next
    // strsignal call — it names a reusable per-thread buffer, not a static —
    // and it is copied into an owned String before this function returns.
    let name = unsafe { libc::strsignal(signal) };
    (!name.is_null())
        .then(|| unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy().into_owned())
}

/// A resize the wrapped program never hears about leaves it drawing to the old
/// geometry, and a signal death skips every destructor — including the one that
/// takes the terminal back out of raw mode, which otherwise outlives us.
fn watch_signals(master: Master, screen: SharedScreen) -> Result<()> {
    let mut signals = Signals::new([SIGWINCH, SIGHUP, SIGINT, SIGTERM])?;
    std::thread::spawn(move || {
        for signal in signals.forever() {
            if signal == SIGWINCH {
                let size = inherited_size();
                // Before the child hears about it: the master resize is what
                // triggers its repaint, and a repaint parsed against the old
                // geometry wraps in the wrong places for good.
                if let Ok(mut screen) = screen.lock() {
                    screen.resize(size.cols as usize, size.rows as usize);
                }
                if let Ok(master) = master.lock() {
                    let _ = master.resize(size);
                }
            } else {
                restore_terminal();
                std::process::exit(128 + signal);
            }
        }
    });
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
struct RawMode;

impl RawMode {
    fn enable() -> Self {
        let on_a_terminal =
            std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        if on_a_terminal && crossterm::terminal::enable_raw_mode().is_ok() {
            RAW_ACTIVE.store(true, Ordering::SeqCst);
        }
        Self
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Shared with the signal thread, so it runs at most once whichever path exits.
fn restore_terminal() {
    if RAW_ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = crossterm::terminal::disable_raw_mode();
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
