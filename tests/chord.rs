//! The fix chord over a real pty, against the real engine: the unit tests pin
//! what the picker does with a flag it is handed, and this is the one place the
//! flag comes from Harper and the keystrokes arrive at a program that reads
//! them. What the underline looks like on a terminal is `tests/paint.rs`.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// A box with a doubled word in it, drawn as Claude Code draws one, and then a
/// program that reads what is typed. `the the` is one flag over two words —
/// the first span the curated table produces that covers more than one — and
/// the cursor is left at its end, which is where the chord claims it.
const BOX_AND_CAT: &str = "printf '\\033[?2026h\\033[1;1H\\342\\224\\200\\342\\224\\200\\342\\224\\200\
     \\033[2;1H\\342\\235\\257\\302\\240the the cat\
     \\033[3;1H\\342\\224\\200\\342\\224\\200\\342\\224\\200\\033[2;10H\\033[?2026l'; \
     stty raw -echo; cat";

#[test]
fn a_doubled_word_is_fixed_by_the_chord_over_a_real_pty() {
    let log = Path::new(env!("CARGO_TARGET_TMPDIR")).join("chord.log");
    let _ = std::fs::remove_file(&log);
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 40,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("outer pty");
    let mut wrapped = CommandBuilder::new(env!("CARGO_BIN_EXE_blackjack"));
    wrapped.args(["--", "sh", "-c", BOX_AND_CAT]);
    wrapped.env("BLACKJACK_DEBUG_LOG", &log);
    wrapped.env_remove("BLACKJACK_PAINT_OFF");
    // The machine running the suite is not the thing being tested: both the
    // class this needs and the key it presses are the user's to change, so an
    // ordinary config on the machine would fail this or hang it out to its
    // deadline.
    wrapped.env("XDG_CONFIG_HOME", empty_config());
    let mut child = pty.slave.spawn_command(wrapped).expect("spawn blackjack");
    drop(pty.slave);

    // Kept rather than drained: the wrapped program is `cat`, so what it writes
    // back is what the chord actually delivered to it.
    let received = Arc::new(Mutex::new(Vec::new()));
    let mut from_wrapped = pty.master.try_clone_reader().expect("reader");
    let heard = received.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 1024];
        while let Ok(read) = from_wrapped.read(&mut chunk) {
            if read == 0 {
                return;
            }
            heard
                .lock()
                .expect("received")
                .extend_from_slice(&chunk[..read]);
        }
    });
    // The engine is still building its dictionary when the box lands, so the
    // check is what says the wrapper is ready to be asked anything.
    wait_for(&log, "flags [0..7]");

    let mut to_wrapped = pty.master.take_writer().expect("writer");
    to_wrapped.write_all(b"\x1b[46;5u").expect("the fix key");
    to_wrapped.flush().expect("flush");

    // Seven characters taken out and one word put back: the keystrokes a user
    // would have typed, which is all the wrapped program is ever told.
    wait_for(&log, r#"fix: "\x7f\x7f\x7f\x7f\x7f\x7f\x7fthe""#);
    let fix = b"\x7f\x7f\x7f\x7f\x7f\x7f\x7fthe";
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let heard = received.lock().expect("received").clone();
        if heard.windows(fix.len()).any(|window| window == fix) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the program never received the fix: {:?}",
            String::from_utf8_lossy(&heard)
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    child.kill().ok();
    child.wait().ok();
}

/// A config directory with nothing in it, so the defaults are what runs.
fn empty_config() -> std::path::PathBuf {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join("chord-config");
    std::fs::create_dir_all(home.join("blackjack")).unwrap();
    let _ = std::fs::remove_file(home.join("blackjack/config.toml"));
    home
}

/// Polls the log rather than the wrapped program's output: a read on the pty
/// has no deadline, so a wrapper that goes quiet would hang the run instead of
/// failing it.
fn wait_for(log: &Path, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let dump = std::fs::read_to_string(log).unwrap_or_default();
        if dump.contains(marker) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no {marker:?} in the log:\n{dump}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
