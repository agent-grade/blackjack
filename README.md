# blackjack

Spell-check for the Claude Code prompt box. As you type, a word the dictionary
does not know is underlined in red and a curated set of grammar slips in blue,
inside the input line itself, in the terminal and in the desktop app. An
editor's spell-check, where you talk to Claude.

Everything runs locally: the checker is [Harper](https://github.com/Automattic/harper),
vendored as its WebAssembly build and run by Node on this machine. Nothing typed
leaves it.

Blackjack changes what you typed only when you ask it to.

## Install

Needs Node 22.18 or newer on `PATH`. The repository is its own plugin
marketplace:

```sh
claude plugin marketplace add git@gitlab.com:yuriikhi/blackjack.git
```

```sh
claude plugin install blackjack@blackjack
```

Updates come with Claude Code's own plugin updates, or by hand:

```sh
claude plugin marketplace update blackjack
```

To try a checkout without installing it, start a session with the plugin
directory:

```sh
claude --plugin-dir ~/path/to/blackjack/mod
```

## Using it

Nothing is underlined while the cursor is still inside the word. What is not
prose is never checked, because at a prompt those are what you meant: a path, a
flag, an identifier, a version, `camelCase`, a dotfile, anything between
backticks.

Two chords, typed as ordinary characters, do the two things the underlines ask
for:

- `;;` replaces the nearest flagged word before the cursor with the first
  suggestion. Press it again for the next one, and once more past the last to
  get your own word back. With nothing flagged, or nothing to suggest, the two
  characters stay as typed.
- `::` after a space keeps the nearest red word instead: the underline goes,
  and the word is written to your list so it is not flagged again. Typed
  directly after a word, as in `std::mem`, it is code and stays.

Grammar flags are not words and cannot be learned. The classes: the wrong
article, a determiner left out of a request, `its` for `it's` and back, `lets`
for `let's`, a word typed twice, two spaces for one, a space before punctuation,
`too` for `to`, `then` for `than`.

## The word list

`~/.config/blackjack/words`, one word per line, read when a session starts.
Blank lines and lines starting with `#` are skipped, so the file can be
annotated by hand. The learn chord appends to it.

## Development

```sh
npm --prefix mod install
```

```sh
npm --prefix mod test
```

```sh
npm --prefix mod run typecheck
```

The typecheck reads the type declarations Claude Code writes next to the plugin
when it loads it once (`mod/.claude-plugin/types/`), so load the plugin before
the first run. The manifest and the hooks module are checked the way Claude Code
reads them with:

```sh
claude plugin validate mod
```

The checker is a Node child process the plugin starts with the session and
talks to over a unix socket; the hooks module itself runs in Claude Code's
plugin sandbox, which has no WebAssembly. Harper's files under
`mod/engine/vendor/harper/` are copied unmodified from the `harper.js` npm
package, version noted in `VERSION` there.

## Acknowledgements

Spelling and grammar come from [Harper](https://github.com/Automattic/harper)
(Apache-2.0). The original approach was informed by
[tuipo](https://github.com/ARahim3/tuipo) (MIT/Apache-2.0).
