use std::io::{IsTerminal, Read, Write};
use std::mem::ManuallyDrop;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM, SIGWINCH};
use signal_hook::iterator::Signals;

use crate::checker::Checker;
use crate::cli::Command;
use crate::config::Config;
use crate::input::{Input, SharedInput};
use crate::painter::Painter;
use crate::picker::{Chord, Picker, SharedChecked, SharedLearned};
use crate::resolve;
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
    if let Err(failure) = resolve::check(&command.program) {
        eprintln!("blackjack: {}: {failure}", command.program);
        return Ok(failure.exit_code());
    }
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
    let painting = Painter::wanted();
    let (pause, keys, arriving) = configured(painting);
    let checked = SharedChecked::default();
    let learned = SharedLearned::default();
    let picker = Picker::new(
        keys.fix,
        keys.learn,
        Arc::clone(&checked),
        Arc::clone(&learned),
        keys.words,
    );

    let piped_stdin = !std::io::stdin().is_terminal();
    let input: SharedInput = Arc::new(Mutex::new(Input::new(piped_stdin)));
    let log = Log::open();

    // Registered before raw mode is entered: a signal arriving in between would
    // otherwise hit the default disposition and leave the terminal raw.
    watch_signals(Arc::clone(&master), Arc::clone(&screen)).context("watching for signals")?;
    let _raw = RawMode::enable();
    restore_terminal_on_panic();

    // Detached: this read blocks, and the child exiting must not wait on a keystroke.
    let typed = log.clone();
    let typed_input = input.clone();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut to_child = InputTap::new(&mut *to_child, typed_input, picker, typed);
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

    let mut tap = Tap::new(
        std::io::stdout(),
        screen,
        input,
        Painter::open(painting, pause, arriving, Arc::clone(&checked), learned),
        log,
    );
    // The read moves to its own thread so that this one can wait on a timer as
    // well as on the program: the underline is due a pause after the user
    // stops typing, and a program that has finished drawing sends nothing to
    // wake anyone with. The handover is bounded, which is what a single loop
    // gave for free — a program that writes faster than this side can parse
    // has to wait for it, rather than filling memory with output the terminal
    // has not been shown yet.
    let (chunks, arriving_output) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut buf = [0u8; BUF_LEN];
        while let Ok(read) = from_child.read(&mut buf) {
            if read == 0 || chunks.send(buf[..read].to_vec()).is_err() {
                return;
            }
        }
    });
    if let Ended::TerminalGone = forward(arriving_output, &mut tap) {
        // Nothing can be shown to anyone any more, so the wrapped program has
        // nobody left to talk to. Waiting on it instead would wait forever:
        // it fills the pty nothing is draining and blocks there.
        let _ = child.kill();
    }
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

/// Everything the user's config decides, and the checker it asks for on its
/// way. A third of a second of dictionary loading is kept off the path between
/// the user and their prompt: the wrapper was started to run a program, so the
/// checker is built on a thread of its own and joins whenever it is ready.
/// Switched off, none of it happens — no config is read, no dictionary is
/// built, and the channel it would have arrived on is closed from the start;
/// the chord is the default one, which nothing will ask for.
fn configured(wanted: bool) -> (Duration, Keys, Receiver<Box<dyn Checker + Send>>) {
    let (xdg, home) = (
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    );
    if !wanted {
        let quiet = Config::default();
        let keys = Keys::of(&quiet, xdg, home);
        return (Duration::ZERO, keys, std::sync::mpsc::channel().1);
    }
    let config = Config::load();
    let keys = Keys::of(&config, xdg, home);
    let (pause, words) = (config.pause(), keys.words.clone());
    let (built, arriving) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = built.send(config.checker(words.as_deref()));
    });
    (pause, keys, arriving)
}

/// The two keys the wrapper answers to itself, and where a learned word goes.
struct Keys {
    fix: Chord,
    learn: Chord,
    words: Option<PathBuf>,
}

impl Keys {
    fn of(
        config: &Config,
        xdg: Option<std::ffi::OsString>,
        home: Option<std::ffi::OsString>,
    ) -> Self {
        Self {
            fix: config.chord(),
            learn: config.learn_chord(),
            words: config.words_path(xdg, home),
        }
    }
}

/// How the output side stopped.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The wrapped program's output ran out, which is how a session ends.
    ProgramDone,
    /// The terminal being written to is gone.
    TerminalGone,
}

/// The wrapped program's output on one side and the painter's own clock on the
/// other. A chunk is written straight through; a timeout is the painter saying
/// its line has held still long enough to be worth checking, which is the only
/// thing that ever happens on a screen nobody is drawing to.
///
/// Takes the receiving end rather than borrowing it, so that giving up here
/// closes the channel: the reader thread is holding a chunk this side will
/// never take, and a bounded channel would leave it blocked there forever.
fn forward<W: Write>(chunks: Receiver<Vec<u8>>, tap: &mut Tap<W>) -> Ended {
    loop {
        let arriving = match tap.wake_in() {
            Some(waiting) => chunks.recv_timeout(waiting),
            None => chunks.recv().map_err(RecvTimeoutError::from),
        };
        match arriving {
            Ok(chunk) => {
                if tap.write_all(&chunk).is_err() || tap.flush().is_err() {
                    return Ended::TerminalGone;
                }
                panic_if_asked();
            }
            Err(RecvTimeoutError::Timeout) => tap.tick(),
            Err(RecvTimeoutError::Disconnected) => return Ended::ProgramDone,
        }
    }
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

/// A panic prints its message before unwinding reaches any destructor, so on
/// the way out the message would land on a terminal still in raw mode: no
/// newline translation, every line starting where the last one ended. The
/// restore is idempotent, so the destructor still runs and still does nothing
/// twice. Deliberately for any thread's panic, not only the one that ends the
/// session: a panic anywhere in here means the wrapper is no longer keeping
/// its side of the bargain, and a cooked terminal is the safe way to be wrong.
fn restore_terminal_on_panic() {
    let printing = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panicking| {
        restore_terminal();
        printing(panicking);
    }));
}

/// Nothing outside this process can make the output side panic, so a debug
/// build takes one on request and the pty test can watch what it leaves
/// behind — and only from outside is there anything to watch, since what is
/// under test is a real terminal coming back cooked before the message lands.
/// The cost is what settles it: one branch on a value read once for the
/// process, and nothing at all in a release build.
#[cfg(debug_assertions)]
fn panic_if_asked() {
    // Read once for the process: this runs on every chunk the program writes.
    static ASKED: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var_os("BLACKJACK_PANIC").is_some());
    if *ASKED {
        panic!("asked for a panic on the output thread");
    }
}

#[cfg(not(debug_assertions))]
fn panic_if_asked() {}

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
    use crate::screen::Screen;

    /// Off is off: nothing is spawned, so nothing can arrive — a channel with
    /// no sender left is the only shape that says the dictionary was never
    /// built rather than merely not ready yet.
    /// The terminal this was writing to is gone. Whoever is reading the
    /// wrapped program has a chunk in hand that will never be taken, and the
    /// handover holds one at a time — so giving up here has to take the
    /// channel down too, or that reader waits forever and the wrapped program
    /// waits behind it.
    #[test]
    fn a_terminal_that_is_gone_releases_whoever_is_reading() {
        struct Gone;
        impl Write for Gone {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (chunks, arriving) = std::sync::mpsc::sync_channel(1);
        chunks.send(b"anything".to_vec()).expect("one chunk queued");
        let mut tap = Tap::new(
            Gone,
            Arc::new(Mutex::new(Screen::new(20, 3))),
            Arc::new(Mutex::new(Input::new(false))),
            Painter::off(),
            crate::tap::Log::open(),
        );

        assert_eq!(forward(arriving, &mut tap), Ended::TerminalGone);
        assert!(
            chunks.send(b"more".to_vec()).is_err(),
            "the reader is still waiting to hand over a chunk"
        );
    }

    #[test]
    fn the_switch_builds_no_checker_at_all() {
        let (pause, _, arriving) = configured(false);

        assert_eq!(pause, Duration::ZERO);
        assert!(matches!(
            arriving.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
    }

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
