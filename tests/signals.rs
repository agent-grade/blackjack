use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Command as Shell;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const SIGTERM: u32 = 15;

/// Destructors do not run on a signal death, so a terminal left in raw mode
/// would outlive blackjack and wreck the shell that started it. The exit code
/// is the verdict: reaching it means the handler ran rather than the default
/// disposition, and restoring the terminal is what that handler does first.
/// The restored state itself cannot be asserted here — this harness's terminal
/// is a pty, and the kernel resets its termios once blackjack dies, so the
/// check would pass with the handler removed. A real terminal keeps the raw
/// state, which is why it takes a human at one to close this off.
#[test]
fn intercepts_a_terminating_signal_rather_than_dying_by_default() {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("outer pty");

    let log = Path::new(env!("CARGO_TARGET_TMPDIR")).join("signalled.log");
    let _ = std::fs::remove_file(&log);
    let mut wrapped = CommandBuilder::new(env!("CARGO_BIN_EXE_blackjack"));
    wrapped.args(["--", "sh", "-c", "echo ready; sleep 30"]);
    wrapped.env("BLACKJACK_DEBUG_LOG", &log);
    let mut blackjack = pty.slave.spawn_command(wrapped).expect("spawn blackjack");

    let mut output = BufReader::new(pty.master.try_clone_reader().expect("reader"));
    wait_for(&mut output, "ready");

    let tty = pty.master.tty_name().expect("the pty has a device name");
    assert!(
        !canonical_mode(&tty),
        "blackjack never put the terminal into raw mode, so this proves nothing"
    );

    let pid = blackjack.process_id().expect("blackjack pid");
    Shell::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("signal blackjack");
    let status = blackjack.wait().expect("blackjack exits");

    assert_eq!(status.exit_code(), 128 + SIGTERM);
    // A session that ends this way ends without reaching any of the lines
    // below the forwarding loop, so this is the only thing that can account
    // for it — and it is how most wrapped sessions actually end.
    let logged = std::fs::read_to_string(&log).expect("the log back");
    assert!(logged.contains("end: signal "), "{logged}");
}

fn wait_for(output: &mut impl BufRead, needle: &str) {
    let mut line = String::new();
    for _ in 0..40 {
        line.clear();
        if output.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.contains(needle) {
            return;
        }
    }
    panic!("never saw `{needle}` on the wrapped terminal");
}

/// Asks the terminal device itself rather than blackjack, so what comes back is
/// the state blackjack actually left behind.
fn canonical_mode(tty: &Path) -> bool {
    let stty = Shell::new("stty")
        .args(["-a", "-f"])
        .arg(tty)
        .output()
        .expect("read the terminal settings");
    let flags = String::from_utf8_lossy(&stty.stdout);
    assert!(
        flags.contains("icanon"),
        "stty reported no canonical-mode flag"
    );
    !flags.contains("-icanon")
}
