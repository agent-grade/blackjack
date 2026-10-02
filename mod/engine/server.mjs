import { createServer } from 'node:http'
import { unlink } from 'node:fs/promises'
import { LocalLinter } from './vendor/harper/index.js'
import { binary } from './vendor/harper/binary.js'
import { enabledConfig, lint } from './flags.mts'

const socketPath = process.argv[2]

const linter = new LocalLinter({ binary })
await linter.setup()
await linter.setLintConfig(enabledConfig(await linter.getLintConfig()))

async function flagsOf(text) {
  try {
    return await lint(linter, text)
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
