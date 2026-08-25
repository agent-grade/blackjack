use std::io::{Read, Write};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// Deliberately not the 80x24 the wrapper falls back to when it cannot read a
/// terminal size: a grid of the fallback shape would otherwise satisfy every
/// assertion below without the real size ever being read.
const SIZE: (u16, u16) = (40, 100);
const STARTED: (u16, u16) = (24, 80);
const WIDE_LINE: &str = "printf 'a%.0s' $(seq 1 100)";
const GRID_HEADER: &str = "--- final grid, cursor at ";

/// The unit tests feed the screen model bytes by hand. This is the wiring they
/// cannot cover — the model fed by a real wrapped program through the pty — and
/// the guarantee that outranks it: what reaches the terminal is untouched.
#[test]
fn tracks_what_the_wrapped_program_drew_without_altering_it() {
    let log = log_path("tracking");
    let pty = native_pty_system().openpty(size(SIZE)).expect("outer pty");

    // A full-width line, then a jump home to overwrite its start: a grid that
    // matches only at 100 columns, and only if the escape sequence was read
    // rather than copied through.
    let mut child = spawn(
        &pty,
        &format!("{WIDE_LINE}; printf '\\033[1;1Hjolly'"),
        &log,
    );

    let mut from_wrapped = pty.master.try_clone_reader().expect("reader");
    let drained = std::thread::spawn(move || {
        let mut seen = Vec::new();
        from_wrapped.read_to_end(&mut seen).ok();
        seen
    });

    let (cursor, rows) = final_grid(&wait_for_grid(&log, &mut child));
    assert_eq!(cursor, "0,5", "the cursor did not follow the jump home");
    assert_eq!(rows[0], format!("jolly{}", "a".repeat(95)));
    assert_eq!(
        rows.len(),
        SIZE.0 as usize,
        "the grid is not the pty's size"
    );

    assert_eq!(
        String::from_utf8_lossy(&drained.join().expect("drain")),
        format!("{}\x1b[1;1Hjolly", "a".repeat(100)),
        "the wrapped program's output did not reach the terminal verbatim"
    );
}

/// The model is resized off the signal thread, and it must happen before the
/// wrapped program hears about the new size — the program repaints the moment
/// it does, and a repaint parsed at the old width wraps in the wrong places.
/// So the program reports its own size on both sides of the resize: seeing the
/// new one proves the model was already there.
#[test]
fn follows_the_terminal_through_a_resize() {
    let log = log_path("resize");
    let pty = native_pty_system()
        .openpty(size(STARTED))
        .expect("outer pty");
    let script = format!("stty size; read _; printf '\\033[2J\\033[H'; stty size; {WIDE_LINE}");
    let mut child = spawn(&pty, &script, &log);

    // Not before the wrapper has parsed output: until then it has not read the
    // starting size either, and a resize would land as its startup size.
    wait_for(&log, "out ");
    pty.master.resize(size(SIZE)).expect("resize");

    let mut to_wrapped = pty.master.take_writer().expect("writer");
    writeln!(to_wrapped).expect("release the wrapped shell");
    to_wrapped.flush().ok();

    let (_, rows) = final_grid(&wait_for_grid(&log, &mut child));
    assert_eq!(rows.len(), SIZE.0 as usize, "the model kept the old height");
    assert_eq!(
        rows[0], "40 100",
        "the new size never reached the wrapped program"
    );
    assert_eq!(
        rows[1],
        "a".repeat(100),
        "the wide line did not fit the resized grid"
    );
    assert_eq!(
        rows[2], "",
        "the line wrapped, so the model kept the old width"
    );
}

fn spawn(
    pty: &portable_pty::PtyPair,
    script: &str,
    log: &std::path::Path,
) -> Box<dyn portable_pty::Child + Send + Sync> {
    let mut wrapped = CommandBuilder::new(env!("CARGO_BIN_EXE_blackjack"));
    wrapped.args(["--", "sh", "-c", script]);
    wrapped.env("BLACKJACK_DEBUG_LOG", log);
    pty.slave.spawn_command(wrapped).expect("spawn blackjack")
}

fn log_path(name: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.log"));
    let _ = std::fs::remove_file(&path);
    path
}

/// Polls the debug log rather than the wrapped program's output: a read on the
/// pty has no deadline, so a wrapper that goes quiet would hang the run instead
/// of failing it.
fn wait_for(log: &std::path::Path, marker: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let dump = std::fs::read_to_string(log).unwrap_or_default();
        if dump.contains(marker) {
            return dump;
        }
        assert!(
            Instant::now() < deadline,
            "never logged {marker:?}: {dump:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The dump lands when the wrapped program's output ends, which is the event
/// worth waiting for — reaping the wrapper itself is neither needed nor prompt.
fn wait_for_grid(
    log: &std::path::Path,
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
) -> String {
    let dump = wait_for(log, GRID_HEADER);
    child.kill().ok();
    dump
}

/// The logged cursor and the grid rows that follow it.
fn final_grid(dump: &str) -> (String, Vec<String>) {
    let grid = dump
        .split(GRID_HEADER)
        .nth(1)
        .expect("no final grid logged");
    let (cursor, rows) = grid.split_once(" ---\n").expect("malformed grid header");
    (
        cursor.to_string(),
        rows.lines().map(str::to_string).collect(),
    )
}

fn size((rows, cols): (u16, u16)) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}
