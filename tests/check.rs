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
/// dictionary and none of them reaches it, and a name that opens with a dot is
/// a name and not a sentence that started badly.
#[test]
fn says_nothing_about_a_line_that_is_not_prose() {
    assert_eq!(check("src/pty.rs").1, "");
    assert_eq!(check("the a.out binary is not in .gitignore").1, "");
    let (_, dotted, _) = check("teh. and .env");
    assert!(dotted.starts_with("0..3"), "{dotted:?}");
    assert_eq!(
        dotted.lines().count(),
        1,
        "a dot ending a word leaves the word, and a dot starting one takes it"
    );
}

/// And nothing about the hole the filter leaves either — the words either side
/// of a path are never each other's neighbours, and a doubled word the user did
/// type is still theirs.
#[test]
fn says_nothing_about_words_a_removed_path_left_next_to_each_other() {
    assert_eq!(check("compare the src/a.rs the src/b.rs").1, "");
    assert!(
        check("the the cat sat").1.starts_with("0..7"),
        "a doubled word the user did type is still flagged"
    );
}

/// Nor about the article in front of one: the word after `a` is the word the
/// rule is about, and where that word was a flag the filter took out, the hole
/// is what it reads. One line of the corpus is enough here — the shapes are the
/// unit tests' to own; this is the wiring.
#[test]
fn says_nothing_about_an_article_in_front_of_something_removed() {
    assert_eq!(check("add a --json option to the CLI").1, "");
    assert!(
        check("This is an mule.").1.contains("→ a"),
        "an article the user did get wrong is still flagged"
    );
}

/// The determiner the request never got. The rule's own subject is the gap, so
/// what a user sees is the word after it, offered with the determiner in front —
/// one press of the fix key puts the line right.
#[test]
fn says_which_word_a_request_is_missing_its_determiner_before() {
    for (line, span, offer) in [
        ("send report", "5..11  report", "a report"),
        ("we need quick fix", "8..13  quick", "a quick"),
        ("add brief comment", "4..9  brief", "a brief"),
    ] {
        let out = check(line).1;
        assert!(out.starts_with(span), "for {line:?}: {out:?}");
        assert!(out.contains(offer), "for {line:?}: {out:?}");
    }
    assert_eq!(check("Please provide the report").1, "");
    assert_eq!(check("send ").1, "", "the noun is still being typed");
    assert_eq!(
        check("send report.").1.trim_end(),
        "5..11  report  → the report, a report",
        "the stop is not part of the word, and `an report` is not an offer"
    );
}

/// A class name the user got wrong is a line on stderr rather than a setting
/// that quietly does nothing, which a silenced class looks exactly like.
#[test]
fn a_grammar_class_that_does_not_exist_says_so() {
    let home = configured("unknown-class", "grammar = [\"artikles\"]\n");

    let (code, out, error) = checking("a teh word", &home);

    assert_eq!(code, Some(0));
    assert!(out.starts_with("2..5  teh"), "{out:?}");
    assert!(error.contains("artikles"), "{error:?}");
    assert!(error.contains("articles"), "{error:?}");
    assert_eq!(check("a teh word").2, "", "a config with nothing wrong");
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
