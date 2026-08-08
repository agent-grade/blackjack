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

## Acknowledgements

Approach and crate choices informed by [tuipo](https://github.com/ARahim3/tuipo)
(MIT/Apache-2.0).
