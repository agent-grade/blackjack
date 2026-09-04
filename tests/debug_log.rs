//! The decision log over a whole session: the lines only a real run produces.

use std::process::{Command, Stdio};

/// How the session ended and what the wrapped program exited with — the last
/// two lines of the account a user sends when something went wrong.
#[test]
fn a_session_accounts_for_how_it_ended() {
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("session.log");
    let _ = std::fs::remove_file(&path);

    let status = Command::new(env!("CARGO_BIN_EXE_blackjack"))
        .args(["--", "sh", "-c", "exit 3"])
        .env("BLACKJACK_DEBUG_LOG", &path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run blackjack");

    assert_eq!(status.code(), Some(3));
    let logged = std::fs::read_to_string(&path).expect("the log back");
    assert!(
        logged.contains("end: the program's output ran out"),
        "{logged}"
    );
    assert!(logged.contains("exit 3"), "{logged}");
}
