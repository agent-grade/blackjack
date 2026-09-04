//! What the user can tune about the checking, read once at the start.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::checker::Checker;
use crate::harper::HarperChecker;

const DIRECTORY: &str = "blackjack";
const FILE: &str = "config.toml";

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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dict_path: None,
            grammar: ["articles", "its-its", "lets-lets"]
                .iter()
                .map(|class| (*class).to_string())
                .collect(),
            pause_ms: 400,
        }
    }
}

impl Config {
    /// The user's config, or the defaults. A file that is missing, unreadable or
    /// malformed never stops the overlay: the wrapped program is what the user
    /// came for, and spell-check is the part that can be skipped.
    pub fn load() -> Self {
        match config_path(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        ) {
            Some(path) => Self::read(&path, std::env::var_os("HOME")),
            None => Self::default(),
        }
    }

    pub fn pause(&self) -> Duration {
        Duration::from_millis(self.pause_ms)
    }

    /// The configured engine. The wrapper asks for a checker and never learns
    /// which one it got, which is the whole point of the trait.
    pub fn checker(&self) -> Box<dyn Checker + Send> {
        Box::new(HarperChecker::new(&self.grammar, self.dict_path.as_deref()))
    }

    fn read(path: &Path, home: Option<OsString>) -> Self {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                if error.kind() != ErrorKind::NotFound {
                    eprintln!("blackjack: {}: {error}", path.display());
                }
                return Self::default();
            }
        };

        match toml::from_str::<Self>(&text) {
            Ok(config) => config.with_home(home),
            Err(error) => {
                let reason = error.to_string();
                let reason = reason.lines().next().unwrap_or("cannot be read");
                eprintln!("blackjack: {}: {reason}", path.display());
                Self::default()
            }
        }
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

    #[test]
    fn no_file_means_the_defaults() {
        let missing = std::env::temp_dir().join("blackjack-no-such-config.toml");

        assert_eq!(Config::read(&missing, home()), Config::default());
    }

    #[test]
    fn the_defaults_watch_every_class_and_pause_for_400ms() {
        let config = Config::default();

        assert_eq!(config.grammar, ["articles", "its-its", "lets-lets"]);
        assert_eq!(config.pause(), Duration::from_millis(400));
        assert_eq!(config.dict_path, None);
    }

    #[test]
    fn a_config_the_user_wrote_is_read_whole() {
        let path = config_file(
            "whole",
            "dict_path = \"/words.txt\"\ngrammar = [\"articles\"]\npause_ms = 120\n",
        );

        let config = Config::read(&path, home());

        assert_eq!(config.dict_path, Some(PathBuf::from("/words.txt")));
        assert_eq!(config.grammar, ["articles"]);
        assert_eq!(config.pause(), Duration::from_millis(120));

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

    #[test]
    fn the_configured_checker_watches_only_the_classes_it_was_given() {
        let mut every_class = Config::default().checker();

        assert_eq!(every_class.check("teh cat").len(), 1);
        assert_eq!(every_class.check("its a test").len(), 1);

        let mut spelling_only = Config {
            grammar: Vec::new(),
            ..Config::default()
        }
        .checker();

        assert_eq!(spelling_only.check("teh cat").len(), 1);
        assert!(spelling_only.check("its a test").is_empty());
    }
}
