use std::process::{Command, Stdio};

const SIGTERM: i32 = 15;

#[test]
fn reports_the_wrapped_programs_own_exit_code() {
    for expected in [0, 1, 42, 255] {
        assert_eq!(
            wrap(&format!("exit {expected}")),
            Some(expected),
            "for a program exiting {expected}"
        );
    }
}

/// A shell reports a signalled program as 128 + the signal, and portable-pty
/// hands back a plain 1 for every one of them — so this is the case that tells
/// a real propagation from a rubber-stamped success code.
#[test]
fn reports_a_signalled_program_the_way_a_shell_does() {
    assert_eq!(wrap("kill -TERM $$"), Some(128 + SIGTERM));
}

fn wrap(script: &str) -> Option<i32> {
    Command::new(env!("CARGO_BIN_EXE_blackjack"))
        .args(["--", "sh", "-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run blackjack")
        .code()
}
