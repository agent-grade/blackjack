import { test } from 'node:test'
import assert from 'node:assert/strict'

import { anchored } from '../engine/flags.mts'

const chars = (text: string) => Array.from(text)

test('a gap whose offer inserts a word moves onto the word after it', () => {
  assert.deepEqual(anchored(chars('send report'), { start: 4, end: 5 }, true), { start: 5, end: 11 })
})

test('the moved span stops before punctuation', () => {
  assert.deepEqual(anchored(chars('send report, now'), { start: 4, end: 5 }, true), { start: 5, end: 11 })
})

test('a gap with no word after it is dropped', () => {
  assert.equal(anchored(chars('send '), { start: 4, end: 5 }, true), undefined)
})

test('a gap whose fix is the gap itself stays where it is', () => {
  assert.deepEqual(anchored(chars('a  b'), { start: 1, end: 3 }, false), { start: 1, end: 3 })
})

test('a span over a word is never moved', () => {
  assert.deepEqual(anchored(chars('its fine'), { start: 0, end: 3 }, true), { start: 0, end: 3 })
})

test('the moved span counts code points', () => {
  assert.deepEqual(anchored(chars('send 𝐀report'), { start: 4, end: 5 }, true), { start: 5, end: 12 })
})
