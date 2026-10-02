import { createServer } from 'node:http'
import { unlink } from 'node:fs/promises'
import { LocalLinter } from './vendor/harper/index.js'
import { binary } from './vendor/harper/binary.js'

const socketPath = process.argv[2]

const linter = new LocalLinter({ binary })
await linter.setup()
const config = await linter.getLintConfig()
await linter.setLintConfig(Object.fromEntries(Object.keys(config).map(rule => [rule, rule === 'SpellCheck'])))

async function flagsOf(text) {
  try {
    const lints = await linter.lint(text, { language: 'plaintext' })
    return lints.map(lint => {
      const span = lint.span()
      return { start: span.start, end: span.end }
    })
  } catch (error) {
    console.error(`lint failed: ${error}`)
    return []
  }
}

const server = createServer((request, response) => {
  if (request.method !== 'POST' || request.url !== '/lint') {
    response.writeHead(404).end()
    return
  }
  let body = ''
  request.setEncoding('utf8')
  request.on('data', chunk => (body += chunk))
  request.on('end', async () => {
    response.setHeader('content-type', 'application/json')
    response.end(JSON.stringify({ flags: await flagsOf(body) }))
  })
})

await unlink(socketPath).catch(() => {})
server.listen(socketPath, () => console.log(`ready ${socketPath}`))
process.on('SIGTERM', () => server.close(() => process.exit(0)))
setInterval(() => {
  if (process.ppid === 1) server.close(() => process.exit(0))
}, 10_000).unref()
