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

/// A shell answers a program it cannot run with 127 or 126 and a reason on
/// stderr; portable-pty answers with 1 and a formatted string. Told apart here
/// by the exit code and by the reason reaching stderr, since both are what a
/// caller's `||` or a user's eye gets to see.
#[test]
fn tells_a_missing_program_from_one_it_may_not_run() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("exit-code-resolve");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let plain = dir.join("plain");
    std::fs::write(&plain, "#!/bin/sh\n").unwrap();

    let (code, stderr) = run_program("blackjack-no-such-program-anywhere");
    assert_eq!(code, Some(127));
    assert!(
        stderr.contains("command not found"),
        "stderr was {stderr:?}"
    );

    let (code, stderr) = run_program(plain.to_str().unwrap());
    assert_eq!(code, Some(126));
    assert!(
        stderr.contains("permission denied"),
        "stderr was {stderr:?}"
    );

    let (code, stderr) = run_program(dir.to_str().unwrap());
    assert_eq!(code, Some(126));
    assert!(stderr.contains("is a directory"), "stderr was {stderr:?}");
}

fn run_program(program: &str) -> (Option<i32>, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_blackjack"))
        .args(["--", program])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .expect("run blackjack");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
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
