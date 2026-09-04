use std::io::Read;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const SIZE: (u16, u16) = (40, 100);
/// The whole paint, not its middle: the box puts `teh` at column 5 of row 2
/// and leaves the cursor at column 13, so an underline in the wrong place or
/// one that abandons the program's cursor is a different string.
const UNDERLINE: &str =
    "\x1b[?2026h\x1b[2;5H\x1b[4:3m\x1b[58:2::255:0:0mteh\x1b[59m\x1b[24m\x1b[2;13H\x1b[?2026l";
/// How long the box sits there untouched after being drawn once. The program
/// draws nothing more, so nothing but the wrapper's own clock can produce the
/// underline — and the real engine is still loading its dictionary when the
/// one frame lands, so the wait covers that too.
const SILENCE: &str = "6";
/// A box with the misspelling in it, drawn as Claude Code draws one: the prompt
/// between two borders, inside a synchronized-output frame.
const FAKE_BOX: &str = "\\033[?2026h\\033[1;1H\\342\\224\\200\\342\\224\\200\\342\\224\\200\
     \\033[2;1H\\342\\235\\257\\302\\240a teh word\
     \\033[3;1H\\342\\224\\200\\342\\224\\200\\342\\224\\200\\033[2;13H\\033[?2026l";

/// The underline is drawn on the terminal itself, after the wrapped program's
/// own frame — the wiring the unit tests cannot cover, since it is the terminal
/// and not a buffer that has to receive it. The box is drawn once and never
/// again, which is what a user typing a word and stopping leaves behind: the
/// frame that carried the word is the one that changed the line, so waiting for
/// another frame to paint on is waiting forever.
#[test]
fn a_misspelling_in_a_box_nobody_redraws_is_still_underlined() {
    let (seen, _) = run_wrapped(false);
    assert_eq!(
        seen.matches(UNDERLINE).count(),
        1,
        "the underline did not land exactly once: {seen:?}"
    );
    assert!(
        seen.find(UNDERLINE) > seen.find("a teh word"),
        "the underline landed before the frame it belongs to: {seen:?}"
    );
}

/// And the log says so, in the words a user reporting a mis-paint would read:
/// this is the only place the painter's own log — the one `pty::run` hands it —
/// is asked whether it reaches the file.
#[test]
fn the_kill_switch_leaves_the_terminal_alone_and_says_why() {
    let (seen, logged) = run_wrapped(true);

    assert!(seen.contains("a teh word"), "the box never arrived");
    assert!(!seen.contains("\x1b[4:3m"), "painted anyway: {seen:?}");
    assert!(logged.contains("no paint: off"), "{logged}");
}

/// The other half of the same account: a session that did paint says what it
/// checked, what it left out of the check, and where the underline went.
#[test]
fn the_log_of_a_painting_session_says_what_it_did() {
    let (_, logged) = run_wrapped(false);

    assert!(logged.contains("start: config "), "{logged}");
    assert!(logged.contains("check \"a teh word\""), "{logged}");
    assert!(logged.contains("flags [2..5]"), "{logged}");
    assert!(logged.contains("paint "), "{logged}");
    assert!(
        logged.contains(" at [1,4]"),
        "the underline's own row and column: {logged}"
    );
    assert!(
        logged.contains("no paint: "),
        "a refusal with its reason: {logged}"
    );
}

/// Everything the terminal saw and everything the wrapper wrote down about it,
/// with the wrapper reaped once its output ended.
fn run_wrapped(paint_off: bool) -> (String, String) {
    let log =
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("paint-{paint_off}.log"));
    let _ = std::fs::remove_file(&log);
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: SIZE.0,
            cols: SIZE.1,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("outer pty");
    let mut wrapped = CommandBuilder::new(env!("CARGO_BIN_EXE_blackjack"));
    let script = format!("printf '{FAKE_BOX}'; sleep {SILENCE}");
    wrapped.args(["--", "sh", "-c", &script]);
    wrapped.env("BLACKJACK_DEBUG_LOG", &log);
    // Inherited otherwise, which would hand the wrapper the kill switch and
    // have the painting test pass by never painting.
    match paint_off {
        true => wrapped.env("BLACKJACK_PAINT_OFF", "1"),
        false => wrapped.env_remove("BLACKJACK_PAINT_OFF"),
    };
    let mut child = pty.slave.spawn_command(wrapped).expect("spawn blackjack");

    let mut from_wrapped = pty.master.try_clone_reader().expect("reader");
    let drained = std::thread::spawn(move || {
        let mut seen = Vec::new();
        from_wrapped.read_to_end(&mut seen).ok();
        seen
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while child.try_wait().ok().flatten().is_none() {
        if Instant::now() >= deadline {
            child.kill().ok();
            panic!("the wrapper never exited");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(pty.slave);
    drop(pty.master);
    (
        String::from_utf8_lossy(&drained.join().expect("drain")).into_owned(),
        std::fs::read_to_string(&log).expect("the log back"),
    )
}
