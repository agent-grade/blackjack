use std::io::{Read, Write};
use std::process::Stdio;
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const SIZE: (u16, u16) = (24, 80);
/// Typed in three goes, each waited for, so the log has to show the line being
/// built rather than one read that happens to hold all of it.
const TYPED: [&[u8]; 3] = [b"hello wor", b"\x1b[27u", b"ld\r"];
/// A wrapped `cat` writes nothing until it is typed at, so the script says when
/// it has finished setting the terminal up.
const READY: &[u8] = b"ready";

/// The unit tests feed the input model bytes by hand. This is the wiring they
/// cannot cover — the model fed by real keystrokes through the pty — and the
/// guarantee that outranks it: what reaches the wrapped program is untouched.
#[test]
fn tracks_what_was_typed_without_altering_it() {
    let log = log_path("input");
    let (master, mut child) = wrapped_shell(&log);

    let mut from_wrapped = master.try_clone_reader().expect("reader");
    let echoed = std::thread::spawn(move || {
        let mut seen = Vec::new();
        from_wrapped.read_to_end(&mut seen).ok();
        seen
    });

    let mut to_wrapped = master.take_writer().expect("writer");
    // `stty` has to have run, or the first keystrokes come back doubled — and
    // a wrapped `cat` writes nothing of its own to wait on.
    wait_for(&log, "out ");
    for (chunk, logged) in TYPED.iter().zip([
        "-> \"hello wor\"",
        // The kitty encoding of Esc: recorded raw, and no part of the line.
        r#""\x1b[27u" -> "hello wor""#,
        "submit \"hello world\"",
    ]) {
        to_wrapped.write_all(chunk).expect("type");
        to_wrapped.flush().expect("flush");
        wait_for(&log, logged);
    }

    child.kill().ok();
    let echoed = echoed.join().expect("echo");
    let typed_back = echoed
        .strip_prefix(READY)
        .expect("the wrapped program never started");
    assert_eq!(
        typed_back,
        TYPED.concat(),
        "the keystrokes did not reach the wrapped program verbatim"
    );
}

/// A pipe has no Enter key. Both line endings are here because a CRLF is one
/// ending and not two, which no unit test can prove reaches the piped path.
#[test]
fn a_piped_line_submits_the_way_a_typed_one_does() {
    let log = log_path("piped");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_blackjack"))
        .args(["--", "cat"])
        .env("BLACKJACK_DEBUG_LOG", &log)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn blackjack");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(b"one\ntwo\r\nthree\n").expect("pipe");
    }
    wait_for(&log, "submit \"three\"");
    // Killed first, so the wait that reaps it cannot be the unbounded one.
    child.kill().ok();
    child.wait().ok();

    let dump = std::fs::read_to_string(&log).unwrap_or_default();
    let submits: Vec<&str> = dump
        .lines()
        .map(decision)
        .filter(|line| line.starts_with("submit \"") || line.starts_with("submit-dirty"))
        .collect();
    assert_eq!(
        submits,
        ["submit \"one\"", "submit \"two\"", "submit \"three\""],
        "the whole log: {dump:?}"
    );
}

/// The model's cursor plumbed through the input tap into the log — the one part
/// of this wiring no unit test reaches. Each keystroke is waited for, so the log
/// has to show the cursor walking back into the line and out again.
#[test]
fn an_edited_line_is_submitted_as_the_user_left_it() {
    let log = log_path("edited");
    let (master, mut child) = wrapped_shell(&log);

    // `kill()` below is a SIGHUP, so the wrapped program exits through its own
    // signal path — and it cannot finish exiting while its terminal's output has
    // nowhere to drain, which is what bounds the `wait()` that reaps it.
    let mut from_wrapped = master.try_clone_reader().expect("reader");
    std::thread::spawn(move || std::io::copy(&mut from_wrapped, &mut std::io::sink()));

    let mut to_wrapped = master.take_writer().expect("writer");
    wait_for(&log, "out ");
    for (chunk, logged) in [
        (&b"helo"[..], r#"-> "helo" @4"#),
        (b"\x1b[D", r#"-> "helo" @3"#),
        (b"l", r#"-> "hello" @4"#),
        (b"\r", r#"submit "hello""#),
    ] {
        to_wrapped.write_all(chunk).expect("type");
        to_wrapped.flush().expect("flush");
        wait_for(&log, logged);
    }

    child.kill().ok();
    child.wait().ok();
}

/// Raw and unechoed, so the wrapped program's own output is the bytes it
/// received and nothing the line discipline added on their way in.
fn wrapped_shell(
    log: &std::path::Path,
) -> (Box<dyn MasterPty + Send>, Box<dyn Child + Send + Sync>) {
    let pty = native_pty_system().openpty(size(SIZE)).expect("outer pty");
    let mut wrapped = CommandBuilder::new(env!("CARGO_BIN_EXE_blackjack"));
    wrapped.args(["--", "sh", "-c", "stty raw -echo; printf ready; cat"]);
    wrapped.env("BLACKJACK_DEBUG_LOG", log);
    let child = pty.slave.spawn_command(wrapped).expect("spawn blackjack");
    drop(pty.slave);
    (pty.master, child)
}

/// A log line without the milliseconds it is stamped with, which is the
/// decision on its own.
fn decision(line: &str) -> &str {
    line.trim_start()
        .split_once(' ')
        .map_or(line, |(_, decision)| decision)
}

fn log_path(name: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.log"));
    let _ = std::fs::remove_file(&path);
    path
}

/// Polls the debug log rather than the wrapped program's output: a read on the
/// pty has no deadline, so a wrapper that goes quiet would hang the run instead
/// of failing it.
fn wait_for(log: &std::path::Path, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let dump = std::fs::read_to_string(log).unwrap_or_default();
        if dump.contains(marker) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "never logged {marker:?}: {dump:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn size((rows, cols): (u16, u16)) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}
