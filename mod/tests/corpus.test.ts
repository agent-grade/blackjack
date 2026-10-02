import { test } from 'node:test'
import assert from 'node:assert/strict'

import { LocalLinter } from '../engine/vendor/harper/index.js'
import { binary } from '../engine/vendor/harper/binary.js'
import { enabledConfig, lint } from '../engine/flags.mts'

const SLIPS = [
  'create file',
  'add function to module',
  'send report',
  'we need quick fix',
  'make screenshot of page',
  'do you any idea',
  'check diff and commit',
  'can you add comment',
  'please provide summary',
  'i need example',
  'give me short summary',
  'create test for this',
  'add note to the file',
  'send me screenshot',
  'we need better solution',
  'share log',
  'include example in the doc',
  'file ticket for this',
  'reproduce issue',
  'i want quick answer',
  'add fix to the branch',
  'provide explanation',
  'see problem in the logs',
  'attach screenshot',
  'submit change',
  'report bug',
  'need repro',
  'get response from the api',
  'we should create clear summary',
  'could you share detailed response',
  'please send clear update',
  'they need clear solution',
  'the and other options were ignored',
  'we interviewed the some candidates',
  'the crate is famous its speed',
  'i am interested learning rust',
  'take a note',
  'lets try to rewrite',
]

const CLEAN = [
  'add a --json option to the CLI',
  'rename the src/prose.rs helper and keep the tests green',
  'the tests/chord.rs case is a good place to start',
  'run `cargo test` and tell me what fails',
  'i want an --offline flag for the checker',
  'move the config to ~/.config/app/config.toml',
  'check that std::mem::take is the right call here',
  'the painter should not redraw a `pending wrap` cell',
  'give me a summary of what changed in src/screen.rs',
  'is example.com the host it should use',
  'fix the camelCase name in the parser',
  'we need a --dry-run mode before this lands',
  'the harper-core crate is the one to pin',
  'read docs/roadmap.md and tell me the next step',
  'a $HOME path should be expanded before it is used',
  'the v2 branch has a better version of this',
  'explain what `LintGroup::new_curated` does',
  'the 40x100 fixture is the one to replay',
  'add a #tag to the issue and close it',
  'the user@example.com address is in the test data',
  'what does the ~/docs folder hold',
  'put the release_bar case back in the suite',
  'compare the src/a.rs and src/b.rs versions',
  'an --all flag would help here',
  'write a --check mode that prints a summary',
  'the a.out binary is not in .gitignore',
  'is this a std::io::Error or an anyhow::Error',
  'a PascalCase name is fine in the docs',
  'the --release build is the one to measure',
  'open src/main.rs and add an --exit-code flag',
]

async function engine() {
  const linter = new LocalLinter({ binary })
  await linter.setup()
  await linter.setLintConfig(enabledConfig(await linter.getLintConfig()))
  return async (line: string) => (await lint(linter, line)).filter(flag => flag.kind === 'grammar')
}

test('grammar catches the slips and leaves the clean lines alone', async () => {
  const check = await engine()
  const caught = []
  for (const line of SLIPS) if ((await check(line)).length > 0) caught.push(line)
  const falsely = []
  for (const line of CLEAN) if ((await check(line)).length > 0) falsely.push(line)
  assert.ok(caught.length >= 25, `caught ${caught.length} of ${SLIPS.length}; the Rust checker caught 25`)
  assert.deepEqual(falsely, [])
})

test('a missing determiner is flagged on the noun that lacks it', async () => {
  const check = await engine()
  assert.deepEqual(await check('send report'), [{ start: 5, end: 11, kind: 'grammar' }])
})
