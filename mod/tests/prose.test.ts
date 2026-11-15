import { test } from 'node:test'
import assert from 'node:assert/strict'

import { filtered, intact, isCode } from '../engine/prose.mts'

test('what a terminal is full of is not prose', () => {
  for (const code of [
    'src/pty.rs',
    'C:\\Users\\x',
    'std::mem::take',
    '#tag',
    'user@example.com',
    '~/docs',
    'release_bar',
    '--release',
    '$HOME',
    'v2',
    '40x100',
    'example.com',
    'https://example.com/x',
    'camelCase',
    'PascalCase',
    '.gitignore',
    '.env',
  ]) {
    assert.ok(isCode(code), `${code} read as prose`)
  }
})

test('ordinary words are prose', () => {
  for (const word of ['the', 'The', 'HELLO', 'sentence.', 'well-known', 'harper-core', "it's", 'teh', 'recieve', '.', '...']) {
    assert.ok(!isCode(word), `${word} read as code`)
  }
})

test('what is left is where it was', () => {
  const line = 'open src/pty.rs and teh file'
  const prose = filtered(line)
  assert.equal(prose, 'open 0000000000 and teh file')
  assert.equal(prose.indexOf('teh'), line.indexOf('teh'))
})

test('anything inside backticks is code, a lone backticked word included', () => {
  assert.equal(filtered('run `the cat sat` now'), 'run 0000 000 0000 now')
  assert.equal(filtered('run `teh` now'), 'run 00000 now')
})

test('an unclosed backtick runs to the end of the line', () => {
  assert.equal(filtered('say `the rest'), 'say 0000 0000')
})

test("a flag across something removed is not the user's", () => {
  const line = 'compare the src/a.rs the src/b.rs'
  assert.equal(intact(line, { start: 8, end: 24 }), false, 'the doubled the')
  assert.equal(intact(line, { start: 8, end: 11 }), true, 'one word of it')
})

test('a token that fills to itself is still a hole', () => {
  const line = 'the 0 the'
  assert.equal(filtered(line), line)
  assert.equal(intact(line, { start: 0, end: 9 }), false)
  assert.equal(intact(line, { start: 0, end: 3 }), true)
})

test('the hole counts UTF-16 units, so the line keeps its length', () => {
  const line = '𝐀 src/x teh'
  assert.equal(filtered(line).length, line.length)
  assert.equal(intact(line, { start: 9, end: 12 }), true)
  assert.equal(intact(line, { start: 3, end: 8 }), false)
})
