import type { Register } from 'claude-code'

import { FIX, LEARN, chordAt, cycleOver, isStanding, nearest, turn, type Box, type Cycle } from './chords.ts'
import { decorationsOf, type Flag } from './spans.ts'

export const register: Register = on => {
  let socketPath: string | undefined
  let cycle: Cycle | undefined

  on('session.start', async ($, e, next) => {
    const started = await next(e)
    const tmp = (await $.env.get('TMPDIR')) ?? '/tmp'
    const id = (await $.session.id()).slice(0, 8)
    const path = `${tmp.replace(/\/$/, '')}/blackjack-${id}.sock`
    void (async () => {
      const engine = $.process.spawn({
        argv: ['node', 'engine/server.mjs', path],
        cwd: $.plugin.root,
      })
      for await (const { stream, text } of engine) {
        if (stream === 'stdout' && text.startsWith('ready')) socketPath = path
        else $.ui.log(text.trimEnd(), { to: 'debug' })
      }
      socketPath = undefined
    })()
    return started
  })

  on('prompt.edit', async ($, e, pass) => {
    if (!socketPath) return pass(e)
    const socket = socketPath

    const lint = async (text: string): Promise<Flag[]> => {
      const response = await $.http.fetch('http://blackjack/lint', { method: 'POST', body: text, socketPath: socket })
      return response.ok ? (JSON.parse(response.text) as { flags: Flag[] }).flags : []
    }
    const painted = async (box: Box) => ({
      ...box,
      decorations: decorationsOf(box.text, await lint(box.text), box.cursor),
    })

    const fix = chordAt(e, FIX)
    if (fix) {
      if (!cycle || !isStanding(cycle, fix.text)) {
        const flag = nearest(await lint(fix.text), fix.cursor)
        cycle = flag && cycleOver(fix.text, flag)
      }
      if (cycle) {
        const turned = turn(cycle, fix)
        cycle = turned.cycle
        return painted(turned.box)
      }
    }

    const learn = chordAt(e, LEARN)
    if (learn) {
      const flag = nearest((await lint(learn.text)).filter(f => f.kind === 'spelling'), learn.cursor)
      if (flag) {
        await $.http.fetch('http://blackjack/learn', { method: 'POST', body: learn.text.slice(flag.start, flag.end), socketPath: socket })
        return painted(learn)
      }
    }

    if (e.inputText !== FIX[0]) cycle = undefined
    const r = await pass(e)
    return { ...r, decorations: [...(r.decorations ?? []), ...decorationsOf(r.text, await lint(r.text), r.cursor)] }
  })
}
