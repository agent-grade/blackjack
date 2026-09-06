//! What the user can tune about the checking, read once at the start.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::checker::Checker;
use crate::harper::HarperChecker;
use crate::picker::Chord;

const DIRECTORY: &str = "blackjack";
const FILE: &str = "config.toml";
const WORDS: &str = "words.txt";

/// Everything the user gets to decide. A key left out keeps its default, so an
/// empty file and no file at all mean the same thing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// A word list of the user's own jargon, one word per line.
    pub dict_path: Option<PathBuf>,
    /// The curated grammar classes to watch for. All of them, unless narrowed.
    pub grammar: Vec<String>,
    /// How long the typing has to stop before the line is checked.
    pub pause_ms: u64,
    /// The key that fixes the flagged word under the cursor: the modifiers
    /// held down, joined by `+`, and then the character.
    pub fix_key: String,
    /// The key that adds the flagged word under the cursor to `dict_path`.
    pub learn_key: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dict_path: None,
            grammar: crate::harper::classes().map(str::to_string).collect(),
            pause_ms: 400,
            fix_key: "ctrl+.".to_string(),
            learn_key: "ctrl+,".to_string(),
        }
    }
}

impl Config {
    /// The user's config, or the defaults. A file that is missing, unreadable or
    /// malformed never stops the overlay: the wrapped program is what the user
    /// came for, and spell-check is the part that can be skipped.
    pub fn load() -> Self {
        match Self::path() {
            Some(path) => Self::read(&path, std::env::var_os("HOME")),
            None => Self::default(),
        }
    }

    /// The file the wrapper reads, whether or not anything is there. A user
    /// asking why a setting did not take is asking which file was read.
    pub fn path() -> Option<PathBuf> {
        config_path(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    }

    /// The config with a file the user got wrong reported rather than shrugged
    /// at. `load` is the wrapper's way, because a session is worth more than a
    /// setting; this is the way of the commands whose whole job is to explain a
    /// config, where a shrug would be the wrong answer to the question asked.
    pub fn strict() -> anyhow::Result<Self> {
        match Self::path() {
            Some(path) => Self::parse(&path, std::env::var_os("HOME")),
            None => Ok(Self::default()),
        }
    }

    pub fn pause(&self) -> Duration {
        Duration::from_millis(self.pause_ms)
    }

    /// The fix key, or the default one when what is written is not a chord.
    /// Same as everything else here: a line the user got wrong costs them that
    /// line's setting and nothing more.
    pub fn chord(&self) -> Chord {
        Self::keyed(&self.fix_key, &Self::default().fix_key)
    }

    pub fn learn_chord(&self) -> Chord {
        Self::keyed(&self.learn_key, &Self::default().learn_key)
    }

    fn keyed(written: &str, fallback: &str) -> Chord {
        Chord::parse(written)
            .or_else(|| Chord::parse(fallback))
            .expect("the default chords parse")
    }

    /// The word list, which is one file for both sides: what the user has
    /// taught the wrapper is written here and read back from here. Their own
    /// list if they named one, and otherwise the file the README says is
    /// theirs — next to the config, whether or not either exists yet.
    pub fn words_path(&self, xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
        self.dict_path
            .clone()
            .or_else(|| config_path(xdg, home).map(|config| config.with_file_name(WORDS)))
    }

    /// What to say about a `grammar` list naming something that is not a class,
    /// which is a mistake the file's own parser cannot make: an unknown key is
    /// an error, and an unknown value is a silence indistinguishable from a
    /// class the user meant to leave out. Nothing is lost by the mistake but
    /// the class they meant, so this is a line to read and not a reason to
    /// stop.
    pub fn grammar_warning(&self) -> Option<String> {
        let unknown: Vec<&str> = self
            .grammar
            .iter()
            .map(String::as_str)
            .filter(|wanted| !crate::harper::classes().any(|class| class == *wanted))
            .collect();

        (!unknown.is_empty()).then(|| {
            format!(
                "not a grammar class: {} (the classes are {})",
                unknown.join(", "),
                crate::harper::classes().collect::<Vec<_>>().join(", ")
            )
        })
    }

    /// The configured engine, given the word list to start from — the same
    /// file a learned word is appended to, so a word taught in one session is
    /// known to the next one. The wrapper asks for a checker and never learns
    /// which one it got, which is the whole point of the trait.
    pub fn checker(&self, words: Option<&Path>) -> Box<dyn Checker + Send> {
        Box::new(HarperChecker::new(&self.grammar, words))
    }

    fn read(path: &Path, home: Option<OsString>) -> Self {
        Self::parse(path, home).unwrap_or_else(|error| {
            eprintln!("blackjack: {error}");
            Self::default()
        })
    }

    /// One reading of the file, for both policies above. A file that is not
    /// there is not a file the user got wrong.
    fn parse(path: &Path, home: Option<OsString>) -> anyhow::Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => anyhow::bail!("{}: {error}", path.display()),
        };

        toml::from_str::<Self>(&text)
            .map(|config| config.with_home(home))
            .map_err(|error| {
                let reason = error.to_string();
                let reason = reason
                    .lines()
                    .next()
                    .unwrap_or("cannot be read")
                    .to_string();
                anyhow::anyhow!("{}: {reason}", path.display())
            })
    }

    /// A config file is hand-written, so a leading `~/` in a path is what the
    /// user will type; no shell is involved to expand it for them.
    fn with_home(mut self, home: Option<OsString>) -> Self {
        self.dict_path = self.dict_path.map(|path| expand_home(&path, home.as_ref()));
        self
    }
}

/// Where the config lives: `$XDG_CONFIG_HOME/blackjack/config.toml` when that is
/// set, else `$HOME/.config/blackjack/config.toml`. With neither, there is no
/// home to look in and the defaults are all there is.
fn config_path(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    if let Some(xdg) = xdg.filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(xdg).join(DIRECTORY).join(FILE));
    }
    home.filter(|value| !value.is_empty()).map(|home| {
        PathBuf::from(home)
            .join(".config")
            .join(DIRECTORY)
            .join(FILE)
    })
}

fn expand_home(path: &Path, home: Option<&OsString>) -> PathBuf {
    let (Some(home), Some(rest)) = (home, path.to_str().and_then(|path| path.strip_prefix("~/")))
    else {
        return path.to_path_buf();
    };
    PathBuf::from(home).join(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config written where the test can reach it, named after the test so
    /// two running at once cannot collide.
    fn config_file(name: &str, contents: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("blackjack-{name}-{}.toml", std::process::id()));
        fs::write(&path, contents).unwrap();
        path
    }

    fn home() -> Option<OsString> {
        Some(OsString::from("/somewhere"))
    }

    /// A value the user got wrong is a line to read: the file parses, the
    /// session runs, and nothing silently watches for a class that is not
    /// there. The defaults, being every class there is, say nothing.
    #[test]
    fn a_grammar_class_that_does_not_exist_says_which_and_what_there_is() {
        let mistaken = Config {
            grammar: vec!["artikles".into(), "repeated".into()],
            ..Config::default()
        };

        let warning = mistaken.grammar_warning().expect("a warning");

        assert!(
            warning.starts_with("not a grammar class: artikles ("),
            "a class the user got right is named as one they got wrong: {warning:?}"
        );
        for class in crate::harper::classes() {
            assert!(
                warning.contains(class),
                "{class} is not offered: {warning:?}"
            );
        }
        assert_eq!(Config::default().grammar_warning(), None);
        assert_eq!(
            Config {
                grammar: Vec::new(),
                ..Config::default()
            }
            .grammar_warning(),
            None
        );
    }

    #[test]
    fn no_file_means_the_defaults() {
        let missing = std::env::temp_dir().join("blackjack-no-such-config.toml");

        assert_eq!(Config::read(&missing, home()), Config::default());
    }

    #[test]
    fn the_defaults_watch_every_class_and_pause_for_400ms() {
        let config = Config::default();

        for class in crate::harper::classes() {
            assert!(
                config.grammar.iter().any(|watched| watched == class),
                "{class} is not watched by default"
            );
        }
        assert_eq!(
            config.grammar.len(),
            crate::harper::classes().count(),
            "the defaults are every class and nothing else"
        );
        assert_eq!(config.pause(), Duration::from_millis(400));
        assert_eq!(config.dict_path, None);
    }

    #[test]
    fn a_config_the_user_wrote_is_read_whole() {
        let path = config_file(
            "whole",
            "dict_path = \"/words.txt\"\ngrammar = [\"articles\"]\npause_ms = 120\nfix_key = \"alt+f\"\nlearn_key = \"alt+l\"\n",
        );

        let config = Config::read(&path, home());

        assert_eq!(config.dict_path, Some(PathBuf::from("/words.txt")));
        assert_eq!(config.grammar, ["articles"]);
        assert_eq!(config.pause(), Duration::from_millis(120));
        assert_eq!(config.chord(), Chord::parse("alt+f").expect("a chord"));
        assert_eq!(
            config.learn_chord(),
            Chord::parse("alt+l").expect("a chord")
        );

        fs::remove_file(&path).unwrap();
    }

    /// A word list the user did not name still has a home: the file the README
    /// says is theirs, beside the config, whether or not it exists yet.
    #[test]
    fn a_learned_word_has_somewhere_to_go_without_a_dict_path() {
        let default = Config::default();

        assert_eq!(
            default.words_path(None, Some(OsString::from("/somewhere"))),
            Some(PathBuf::from("/somewhere/.config/blackjack/words.txt"))
        );
        assert_eq!(
            default.words_path(Some(OsString::from("/config")), home()),
            Some(PathBuf::from("/config/blackjack/words.txt")),
            "the config directory the user named"
        );
    }

    /// And the user's own list is where their words go when they named one.
    #[test]
    fn a_named_word_list_is_where_learned_words_go() {
        let path = config_file("named-words", "dict_path = \"/mine.txt\"\n");

        let config = Config::read(&path, home());

        assert_eq!(
            config.words_path(None, home()),
            Some(PathBuf::from("/mine.txt"))
        );

        fs::remove_file(&path).unwrap();
    }

    /// A chord nobody can press is a line of the config got wrong, and it costs
    /// that line: the default key still works rather than none at all.
    #[test]
    fn a_fix_key_that_is_not_a_chord_keeps_the_default_one() {
        let path = config_file("chord", "fix_key = \"hyper+x\"\n");

        let config = Config::read(&path, home());

        assert_eq!(config.chord(), Config::default().chord());

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_key_left_out_keeps_its_default() {
        let path = config_file("partial", "pause_ms = 50\n");

        let config = Config::read(&path, home());

        assert_eq!(config.pause_ms, 50);
        assert_eq!(config.grammar, Config::default().grammar);

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_malformed_file_falls_back_to_the_defaults() {
        let path = config_file("malformed", "grammar = [\"articles\"\npause_ms = oops\n");

        assert_eq!(Config::read(&path, home()), Config::default());

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_key_of_the_wrong_type_falls_back_to_the_defaults() {
        let path = config_file("wrong-type", "pause_ms = \"soon\"\n");

        assert_eq!(Config::read(&path, home()), Config::default());

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_key_nobody_knows_falls_back_to_the_defaults() {
        let path = config_file("unknown-key", "pause_ms = 50\ndict_paht = \"/words.txt\"\n");

        assert_eq!(Config::read(&path, home()), Config::default());

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_tilde_in_the_word_list_path_means_home() {
        let path = config_file("tilde", "dict_path = \"~/words.txt\"\n");

        let config = Config::read(&path, home());

        assert_eq!(
            config.dict_path,
            Some(PathBuf::from("/somewhere/words.txt"))
        );

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_tilde_with_no_home_is_left_alone() {
        let path = config_file("tilde-no-home", "dict_path = \"~/words.txt\"\n");

        let config = Config::read(&path, None);

        assert_eq!(config.dict_path, Some(PathBuf::from("~/words.txt")));

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn the_config_sits_under_the_xdg_directory_when_there_is_one() {
        let xdg = Some(OsString::from("/config"));

        assert_eq!(
            config_path(xdg, home()),
            Some(PathBuf::from("/config/blackjack/config.toml"))
        );
    }

    #[test]
    fn without_an_xdg_directory_the_config_sits_under_home() {
        for xdg in [None, Some(OsString::new())] {
            assert_eq!(
                config_path(xdg, home()),
                Some(PathBuf::from("/somewhere/.config/blackjack/config.toml"))
            );
        }
    }

    #[test]
    fn with_nowhere_to_look_there_is_no_config_to_read() {
        assert_eq!(config_path(None, None), None);
        assert_eq!(
            config_path(Some(OsString::new()), Some(OsString::new())),
            None
        );
    }

    /// The list a learned word is written to is the list the checker starts
    /// from, so a word taught in one session is a word the next one knows.
    #[test]
    fn a_word_in_the_list_is_known_to_the_checker_built_from_it() {
        let config = Config::default();
        let words = config
            .words_path(Some(OsString::from(&temporary())), None)
            .expect("a path");
        fs::create_dir_all(words.parent().expect("a directory")).unwrap();
        fs::write(&words, "teh\n").unwrap();

        let mut checker = config.checker(Some(&words));

        assert!(checker.check("teh cat").is_empty());
        assert_eq!(checker.check("hte cat").len(), 1, "any other misspelling");

        fs::remove_file(&words).unwrap();
    }

    /// A config directory of this test's own, so a word list left behind by
    /// another test is not the one being read.
    fn temporary() -> OsString {
        std::env::temp_dir()
            .join(format!("blackjack-list-{}", std::process::id()))
            .into_os_string()
    }

    #[test]
    fn the_configured_checker_watches_only_the_classes_it_was_given() {
        let mut every_class = Config::default().checker(None);

        assert_eq!(every_class.check("teh cat").len(), 1);
        assert_eq!(every_class.check("its a test").len(), 1);

        let mut spelling_only = Config {
            grammar: Vec::new(),
            ..Config::default()
        }
        .checker(None);

        assert_eq!(spelling_only.check("teh cat").len(), 1);
        assert!(spelling_only.check("its a test").is_empty());
    }
}
