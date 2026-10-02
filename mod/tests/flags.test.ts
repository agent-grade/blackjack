import { test } from 'node:test'
import assert from 'node:assert/strict'

import { anchored } from '../engine/flags.mts'

const ARTICLES = [' the ', ' a ', ' an ']

test('a gap whose offer inserts a word moves onto the word after it, offers folded in', () => {
  assert.deepEqual(anchored('send report', { start: 4, end: 5 }, ARTICLES), {
    start: 5,
    end: 11,
    offers: ['the report', 'a report', 'an report'],
  })
})

test('the moved span stops before punctuation', () => {
  assert.deepEqual(anchored('send report, now', { start: 4, end: 5 }, ARTICLES).end, 11)
})

test('a gap with no word after it is empty', () => {
  assert.deepEqual(anchored('send ', { start: 4, end: 5 }, ARTICLES), { start: 5, end: 5, offers: [] })
})

test('a gap whose fix is the gap itself stays where it is', () => {
  assert.deepEqual(anchored('a  b', { start: 1, end: 3 }, [' ']), { start: 1, end: 3, offers: [' '] })
})

test('a span over a word is never moved', () => {
  assert.deepEqual(anchored('its fine', { start: 0, end: 3 }, ["it's"]), { start: 0, end: 3, offers: ["it's"] })
})

test('the moved span counts UTF-16 units, so an astral letter is two', () => {
  assert.deepEqual(anchored('send 𝐀report', { start: 4, end: 5 }, [' a ']).end, 13)
})
