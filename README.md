# blackjack

A terminal spell-check overlay. `blackjack` wraps a command in a PTY and paints
red curly underlines under misspellings and a curated set of grammar slips as
you type — an editor's spell-check, in the terminal.

```sh
blackjack -- claude
```

Everything runs locally: the checker is linked in-process, and nothing typed
ever leaves the machine.

Status: early. v1 is paint-only — blackjack never rewrites your input.

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
```

## Acknowledgements

Approach and crate choices informed by [tuipo](https://github.com/ARahim3/tuipo)
(MIT/Apache-2.0). Spelling and grammar come from
[Harper](https://github.com/Automattic/harper) (`harper-core`, Apache-2.0),
linked in-process.
