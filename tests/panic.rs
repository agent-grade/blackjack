use std::io::Read;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const SIZE: (u16, u16) = (24, 80);
/// The message the debug-only hook panics with, one read into the wrapped
/// program's output.
const ASKED: &str = "asked for a panic on the output thread";

/// A panic on the output side must not leave the user's terminal in raw mode,
/// and the proof is in the message itself: the terminal turns a newline into a
/// carriage return and a newline only once it is out of raw mode, so a message
/// whose lines carry `\r\n` is one printed after the restore. Printed before
/// it, the same lines would staircase down the screen and the terminal would
/// still be raw when the process was gone. What the program had already
/// written is checked on the same run — a wrapper that panicked away the
/// output it was carrying would be no better.
#[test]
fn a_panic_puts_the_terminal_back_before_it_says_anything() {
    // The seam the wrapper panics through is compiled into debug builds only,
    // so under `cargo test --release` there is no panic to watch.
    if !cfg!(debug_assertions) {
        return;
    }
    let (seen, status) = run_panicking();

    // Both positions are taken before either is compared: an `Option` holding
    // nothing sorts first, so output that never arrived would read as having
    // arrived early.
    let wrote = seen
        .find("hello")
        .expect("the wrapped program's own output");
    let panicked = seen.find(ASKED).expect("the panic");
    let message = &seen[panicked..];

    assert!(
        wrote < panicked,
        "the panic landed before the output: {seen:?}"
    );
    assert!(
        message.contains("\r\n"),
        "the panic printed onto a raw terminal: {message:?}"
    );
    assert_ne!(status, 0, "a panic exited as success");
}

fn run_panicking() -> (String, u32) {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: SIZE.0,
            cols: SIZE.1,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("outer pty");
    let mut wrapped = CommandBuilder::new(env!("CARGO_BIN_EXE_blackjack"));
    wrapped.args(["--", "sh", "-c", "printf hello; sleep 2"]);
    wrapped.env("BLACKJACK_PANIC", "1");
    let mut child = pty.slave.spawn_command(wrapped).expect("spawn blackjack");

    let mut from_wrapped = pty.master.try_clone_reader().expect("reader");
    let drained = std::thread::spawn(move || {
        let mut seen = Vec::new();
        from_wrapped.read_to_end(&mut seen).ok();
        seen
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().ok().flatten() {
            break status.exit_code();
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            panic!("the wrapper never exited");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(pty.slave);
    drop(pty.master);
    (
        String::from_utf8_lossy(&drained.join().expect("drain")).into_owned(),
        status,
    )
}
