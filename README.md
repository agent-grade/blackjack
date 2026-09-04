# blackjack

A terminal spell-check overlay. `blackjack` wraps a command in a PTY and paints
red curly underlines under misspellings and a curated set of grammar slips as
you type — an editor's spell-check, in the terminal.

```sh
blackjack -- claude
```

Everything runs locally: the checker is linked in-process, and nothing typed
ever leaves the machine.

Status: early. Blackjack changes what you typed only when you ask it to, with
the fix key below.

## Configuration

Optional, at `$XDG_CONFIG_HOME/blackjack/config.toml` or
`~/.config/blackjack/config.toml`. Every key has a default; a missing file
means the defaults, a malformed one costs a line on stderr and the defaults.

```toml
# Extra words the spell-check accepts, one per line. `~` expands.
dict_path = "~/.config/blackjack/words.txt"

# The grammar slips to flag; spelling is always on. Remove a class to
# silence it.
grammar = ["articles", "its-its", "lets-lets"]

# How long typing has to pause before the line is checked and painted.
pause_ms = 400

# The key that replaces the flagged word under the cursor with the first
# suggestion. Press it again for the next one, and once more past the last to
# get your own word back. Modifiers joined by `+`, then the character.
fix_key = "ctrl+."

# The key that adds the flagged word under the cursor to the word list above,
# so it stops being flagged — now, and in every session after this one.
learn_key = "ctrl+,"
```

## Acknowledgements

Approach and crate choices informed by [tuipo](https://github.com/ARahim3/tuipo)
(MIT/Apache-2.0). Spelling and grammar come from
[Harper](https://github.com/Automattic/harper) (`harper-core`, Apache-2.0),
linked in-process.
