import type { Register } from 'claude-code'

import { decorationsOf, type Flag } from './spans.ts'

export const register: Register = on => {
  let socketPath: string | undefined

  on('session.start', async ($, e, next) => {
    const started = await next(e)
    const tmp = (await $.env.get('TMPDIR')) ?? '/tmp'
    const id = (await $.session.id()).slice(0, 8)
    const path = `${tmp.replace(/\/$/, '')}/blackjack-${id}.sock`
    void (async () => {
      const engine = $.process.spawn({ argv: ['node', 'engine/server.mjs', path], cwd: $.plugin.root })
      for await (const { stream, text } of engine) {
        if (stream === 'stdout' && text.startsWith('ready')) socketPath = path
        else $.ui.log(text.trimEnd(), { to: 'debug' })
      }
      socketPath = undefined
    })()
    return started
  })

  on('prompt.edit', async ($, e, next) => {
    const r = await next(e)
    if (!socketPath) return r
    const response = await $.http.fetch('http://blackjack/lint', { method: 'POST', body: r.text, socketPath })
    if (!response.ok) return r
    const { flags } = JSON.parse(response.text) as { flags: Flag[] }
    return { ...r, decorations: [...(r.decorations ?? []), ...decorationsOf(r.text, flags, r.cursor)] }
  })
}
