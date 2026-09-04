//! `--check`: the line put to the real engine the way the painter puts it, so a
//! user can try a word list or a config change without wrapping anything.

use std::process::Command;

#[test]
fn prints_the_flag_where_it_is_and_what_is_offered_for_it() {
    let (code, out, _) = check("a teh word");

    assert_eq!(code, Some(0));
    assert!(out.starts_with("2..5  teh  → "), "{out:?}");
    assert_eq!(out.lines().count(), 1, "{out:?}");
}

/// The same filter the painter runs: a path is three misspelled words to a
/// dictionary and none of them reaches it.
#[test]
fn says_nothing_about_a_line_that_is_not_prose() {
    assert_eq!(check("src/pty.rs").1, "");
}

/// Nothing wrong and nothing typed both print nothing, and neither is a
/// failure — the exit code answers "did the check run", not "was it clean".
#[test]
fn a_line_with_nothing_wrong_prints_nothing_and_succeeds() {
    for line in ["the cat sat", ""] {
        let (code, out, _) = check(line);
        assert_eq!(code, Some(0), "for {line:?}");
        assert_eq!(out, "", "for {line:?}");
    }
}

/// The wrapper carries on without a config it cannot read, because a session is
/// worth more than a setting. This command does not: the question it was asked
/// is about the config.
#[test]
fn a_config_it_cannot_read_is_the_answer_here() {
    let home = configured("unreadable", "not = [toml\n");

    let (code, _, error) = checking("a teh word", &home);

    assert_eq!(code, Some(1));
    assert!(error.contains("config.toml"), "{error:?}");
}

/// A word list that is not there yet is not a word list the user got wrong: a
/// user who has never pressed the learn key gets no complaint, from this or
/// from a wrapped session.
#[test]
fn a_word_list_that_does_not_exist_yet_is_not_an_error() {
    let home = configured("no-words", "dict_path = \"/nowhere/blackjack/words.txt\"\n");

    let (code, out, error) = checking("a teh word", &home);

    assert_eq!(code, Some(0));
    assert!(out.starts_with("2..5  teh"), "{out:?}");
    assert_eq!(error, "");
}

/// A config directory of this test's own, named after it: the machine running
/// the suite is not the thing being tested, and neither is the test beside it.
fn configured(name: &str, contents: &str) -> std::path::PathBuf {
    let home = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("check-{name}"));
    let directory = home.join("blackjack");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("config.toml"), contents).unwrap();
    home
}

fn check(text: &str) -> (Option<i32>, String, String) {
    checking(text, &configured("default", "pause_ms = 400\n"))
}

fn checking(text: &str, config: &std::path::Path) -> (Option<i32>, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_blackjack"))
        .args(["--check", text])
        .env("XDG_CONFIG_HOME", config)
        .output()
        .expect("run blackjack");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}
