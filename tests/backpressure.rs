use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const SIZE: (u16, u16) = (40, 100);
/// Big enough that a wrapper holding the whole stream is unmistakable next to
/// one holding a chunk of it, and small enough to push through in a second.
const STREAM: usize = 48 * 1024 * 1024;
/// The wrapper's own resident memory is a few megabytes with the paint off.
/// Anything approaching the stream size is the stream itself, queued.
const ROOF: usize = 16 * 1024 * 1024;
/// And below this it was never measured at all — a wrapper that died early
/// reads as one that held nothing.
const FLOOR: usize = 1024 * 1024;

/// The wrapped program writes down a pipe as fast as the kernel will take it,
/// and this side has to parse every byte. Reading ahead without a bound turns
/// that difference into memory: the terminal is shown the output late and the
/// backlog grows to whatever the program felt like writing — a build log, a
/// large file, a long `git log`. A single loop gave the backpressure for free
/// and the reader thread had to be given it back.
#[test]
fn a_program_that_outruns_the_parser_waits_for_it() {
    let source =
        std::env::temp_dir().join(format!("blackjack-backpressure-{}.bin", std::process::id()));
    let mut chunk = Vec::new();
    while chunk.len() < STREAM {
        chunk.extend_from_slice(include_bytes!("fixtures/claude-tui-40x100.bin"));
    }
    std::fs::write(&source, &chunk).expect("the stream to replay");
    // Read back before the run: a first read off a cold disk is slow enough
    // that the parser keeps up with it, no backlog forms, and an unbounded
    // handover looks exactly like a bounded one.
    std::fs::read(&source).expect("warm the stream");

    let (peak, passed_through) = wrapping(source.to_str().expect("a path"));
    std::fs::remove_file(&source).ok();

    // Everything the program wrote reached the terminal: without this the
    // wrapper could have died on the first chunk and held nothing at all.
    assert_eq!(
        passed_through,
        chunk.len(),
        "the stream did not get through"
    );
    assert!(peak > FLOOR, "held {peak} bytes: nothing was measured");
    assert!(
        peak < ROOF,
        "held {peak} bytes of a {}-byte stream: the reader is not waiting",
        chunk.len()
    );
}

/// The most resident memory the wrapper is seen holding while it swallows the
/// file — sampled from the outside, which is the number a user would see in a
/// process monitor — and how much of the stream came out the other side.
fn wrapping(source: &str) -> (usize, usize) {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: SIZE.0,
            cols: SIZE.1,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("outer pty");
    let mut wrapped = CommandBuilder::new(env!("CARGO_BIN_EXE_blackjack"));
    wrapped.args(["--", "cat", source]);
    // The dictionary is tens of megabytes of its own and would drown the
    // measurement; what is under test is the handover, not the checker.
    wrapped.env("BLACKJACK_PAINT_OFF", "1");
    let mut child = pty.slave.spawn_command(wrapped).expect("spawn blackjack");

    // Drained as fast as it arrives: a full pty would stall the wrapper's
    // writes and hide the very backlog this is looking for.
    let mut from_wrapped = pty.master.try_clone_reader().expect("reader");
    let drained = std::thread::spawn(move || {
        std::io::copy(&mut from_wrapped, &mut std::io::sink()).unwrap_or(0)
    });

    let mut peak = 0;
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().ok().flatten().is_none() {
        peak = peak.max(resident(child.process_id().expect("a pid")));
        if Instant::now() >= deadline {
            child.kill().ok();
            panic!("the wrapper never finished the stream");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(pty.slave);
    drop(pty.master);
    (peak, drained.join().expect("drain") as usize)
}

fn resident(pid: u32) -> usize {
    let reported = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output();
    let Ok(reported) = reported else {
        return 0;
    };
    String::from_utf8_lossy(&reported.stdout)
        .trim()
        .parse::<usize>()
        .unwrap_or(0)
        * 1024
}
