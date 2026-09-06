# blackjack

A terminal spell-check overlay. `blackjack` wraps a command in a PTY and paints
curly underlines under misspellings and a curated set of grammar slips as you
type — red for a word it does not know, blue for a grammar slip. An editor's
spell-check, in the terminal.

Everything runs locally: the checker is linked in-process, and nothing typed
ever leaves the machine.

Status: early. Blackjack changes what you typed only when you ask it to.

## Running it for a day

Install it — release, which `cargo install` is by default and which the
dictionary build very much notices:

```sh
cargo install --path .
```

Then wrap the program you were going to run anyway:

```sh
blackjack -- claude
```

A slip is underlined a moment after you stop typing; a red one is a word you
can teach it, a blue one is not. What is not prose
is never checked — a path, a flag, an identifier, a version, anything between
backticks — because in a terminal those are what you meant.

Two keys are the wrapper's own and never reach the program underneath:

- `ctrl+.` replaces the flagged word under the cursor with the first
  suggestion. Press it again for the next one, and once more past the last to
  get your own word back.
- `ctrl+,` keeps the word instead: the underline goes, and the word is written
  to your list so it is not flagged again, in this session or any after it.

Anywhere else, the key is yours and arrives exactly as you pressed it.

To try a config change or a word list without wrapping anything:

```sh
blackjack --check "a teh word in src/pty.rs"
```

It prints the flags it would paint, one per line, and nothing at all when there
is nothing to say.

If the painting is ever in the way, `BLACKJACK_PAINT_OFF=1` wraps without it and
the session carries on.

### When a paint goes wrong

```sh
BLACKJACK_DEBUG_LOG=~/blackjack.log blackjack -- claude
```

Every decision is written there, stamped with the milliseconds since the session
started: the config it read, each line it checked and the tokens the filter took
out of it, every paint and every refusal with its reason, every chord, and how
the session ended. It holds the lines you typed, and — once, at the end — the
last screen the wrapped program drew, so read it before you send it anywhere.

To report one, send the log lines from around the moment it happened and the
size of your terminal.

## Configuration

Optional, at `$XDG_CONFIG_HOME/blackjack/config.toml` or
`~/.config/blackjack/config.toml`. Every key has a default; a missing file
means the defaults, a malformed one costs a line on stderr and the defaults —
except under `--check`, which was asked about the config and says so instead.

```toml
# Extra words the spell-check accepts, one per line, and the file `ctrl+,`
# appends to. `~` expands.
dict_path = "~/.config/blackjack/words.txt"

# The grammar slips to flag; spelling is always on. Remove a class to
# silence it; a name that is not one of these is reported at startup.
#   articles          a/an picked before the word was
#   articles-missing  a determiner left out of a request
#   its-its           its for it's, and back
#   lets-lets         lets for let's
#   repeated          a word typed twice
#   spacing           two spaces where one was meant
#   to-too            too where the infinitive marker was meant
#   then-than         then for a comparison
grammar = [
  "articles", "articles-missing", "its-its", "lets-lets", "repeated", "spacing",
  "to-too", "then-than",
]

# How long typing has to pause before the line is checked and painted.
pause_ms = 400

# The two keys above, if these are not the ones you want: modifiers joined by
# `+`, then the character. The modifiers are `ctrl`, `alt`, `shift` and `cmd`.
# No terminal sends `cmd` of its own accord, so a chord holding it works only
# where the terminal has been bound to send it, and a terminal that does not
# speak the kitty keyboard protocol can send any chord only as `alt` and the
# character.
fix_key = "ctrl+."
learn_key = "ctrl+,"
```

## Acknowledgements

Approach and crate choices informed by [tuipo](https://github.com/ARahim3/tuipo)
(MIT/Apache-2.0). Spelling and grammar come from
[Harper](https://github.com/Automattic/harper) (`harper-core`, Apache-2.0),
linked in-process.
