import { test } from 'node:test'
import assert from 'node:assert/strict'

import { codePointToUnit, decorationsOf, isBeingTyped } from '../hooks/spans.ts'

const flag = (start: number, end: number) => ({ start, end })
const red = (start: number, end: number) => ({ start, end, underline: true, color: 'red' })
const AWAY = 100

test('an ascii span maps one to one', () => {
  assert.deepEqual(decorationsOf('fix teh bug', [flag(4, 7)], AWAY), [red(4, 7)])
})

test('a BMP non-ascii word before the flag keeps code point and unit offsets equal', () => {
  assert.deepEqual(decorationsOf('中文 teh', [flag(3, 6)], AWAY), [red(3, 6)])
})

test('an astral character before the flag shifts the unit offsets by one', () => {
  assert.deepEqual(decorationsOf('🙂 teh', [flag(2, 5)], AWAY), [red(3, 6)])
})

test('a code point count past the text clamps to its end', () => {
  assert.deepEqual(decorationsOf('teh', [flag(0, 9)], AWAY), [red(0, 3)])
})

test('the unit table has one entry per code point boundary', () => {
  assert.deepEqual(codePointToUnit('a🙂b'), [0, 1, 3, 4])
})

test('the word being typed is not flagged until the cursor leaves it', () => {
  assert.deepEqual(decorationsOf('teh', [flag(0, 3)], 3), [], 'cursor at the end of the word')
  assert.deepEqual(decorationsOf('teh ', [flag(0, 3)], 4), [red(0, 3)], 'a space finished it')
  assert.deepEqual(decorationsOf('teh ', [flag(0, 3)], 2), [], 'cursor moved back inside')
})

test('the cursor at each edge of a word says whether it is finished', () => {
  const held = (text: string, cursor: number) => isBeingTyped(text, cursor, 2, 5)
  assert.equal(held('a teh', 2), false, 'in front of the word')
  assert.equal(held('a teh', 3), true, 'inside the word')
  assert.equal(held('a teh', 5), true, 'the word ends where the typing stopped')
  assert.equal(held('a tehx', 5), true, 'more of the word follows the cursor')
  assert.equal(held('a teh word', 5), false, 'finished and spaced')
  assert.equal(held('a teh, x', 5), false, 'finished and punctuated')
  assert.equal(held('a teh word', 10), false, 'the cursor has moved on')
})

test('a word character after the cursor is read by code point, not unit', () => {
  assert.equal(isBeingTyped('a teh𝐀', 5, 2, 5), true)
})
