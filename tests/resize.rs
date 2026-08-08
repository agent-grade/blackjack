use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const START: (u16, u16) = (24, 80);
const RESIZED: (u16, u16) = (40, 100);

/// A resize the wrapped program never hears about leaves a TUI drawing to the
/// geometry it started with, so this drives the real thing: blackjack inside a
/// pty, resized from the outside, asked what size it now believes it has.
#[test]
fn hands_a_resize_to_the_wrapped_program() {
    let pty = native_pty_system().openpty(size(START)).expect("outer pty");

    let mut wrapped = CommandBuilder::new(env!("CARGO_BIN_EXE_blackjack"));
    wrapped.args(["--", "sh", "-c", "stty size; while read _; do stty size; done"]);
    let mut child = pty.slave.spawn_command(wrapped).expect("spawn blackjack");
    drop(pty.slave);

    let mut from_wrapped = BufReader::new(pty.master.try_clone_reader().expect("reader"));
    let mut to_wrapped = pty.master.take_writer().expect("writer");

    assert_eq!(
        read_size(&mut from_wrapped),
        Some(START),
        "the starting size never reached the wrapped program"
    );

    pty.master.resize(size(RESIZED)).expect("resize");

    // The signal crosses a process and a thread before the child can observe it,
    // so the new size is polled for rather than assumed to have landed already.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = None;
    while Instant::now() < deadline && seen != Some(RESIZED) {
        writeln!(to_wrapped).expect("poke the wrapped shell");
        to_wrapped.flush().ok();
        seen = read_size(&mut from_wrapped);
    }

    child.kill().ok();
    assert_eq!(seen, Some(RESIZED), "the resize never reached the wrapped program");
}

fn size((rows, cols): (u16, u16)) -> PtySize {
    PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }
}

/// Skips the pty's echo of our own pokes: only a bare `<rows> <cols>` counts.
fn read_size(from: &mut impl BufRead) -> Option<(u16, u16)> {
    let mut line = String::new();
    loop {
        line.clear();
        if from.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if let [rows, cols] = fields[..]
            && let (Ok(rows), Ok(cols)) = (rows.parse(), cols.parse())
        {
            return Some((rows, cols));
        }
    }
}
