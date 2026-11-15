import { createServer } from 'node:http'
import { appendFile, mkdir, readFile, unlink } from 'node:fs/promises'
import { homedir } from 'node:os'
import { dirname, join } from 'node:path'
import { LocalLinter } from './vendor/harper/index.js'
import { binary } from './vendor/harper/binary.js'
import { enabledConfig, lint } from './flags.mts'

const socketPath = process.argv[2]
const wordsPath = join(homedir(), '.config', 'blackjack', 'words')

const linter = new LocalLinter({ binary })
await linter.setup()
await linter.setLintConfig(enabledConfig(await linter.getLintConfig()))
await linter.importWords(await wordsOf(wordsPath))

async function wordsOf(path) {
  const list = await readFile(path, 'utf8').catch(() => '')
  return list
    .split('\n')
    .map(line => line.trim())
    .filter(line => line !== '' && !line.startsWith('#'))
}

async function learn(word) {
  try {
    await linter.importWords([word])
    await mkdir(dirname(wordsPath), { recursive: true })
    await appendFile(wordsPath, `${word}\n`)
  } catch (error) {
    console.error(`${wordsPath}: ${error}`)
  }
}

async function flagsOf(text) {
  try {
    return await lint(linter, text)
  } catch (error) {
    console.error(`lint failed: ${error}`)
    return []
  }
}

const server = createServer((request, response) => {
  let body = ''
  request.setEncoding('utf8')
  request.on('data', chunk => (body += chunk))
  request.on('end', async () => {
    if (request.method === 'POST' && request.url === '/lint') {
      response.setHeader('content-type', 'application/json')
      response.end(JSON.stringify({ flags: await flagsOf(body) }))
    } else if (request.method === 'POST' && request.url === '/learn') {
      await learn(body.trim())
      response.writeHead(204).end()
    } else {
      response.writeHead(404).end()
    }
  })
})

await unlink(socketPath).catch(() => {})
server.listen(socketPath, () => console.log(`ready ${socketPath}`))
process.on('SIGTERM', () => server.close(() => process.exit(0)))
setInterval(() => {
  if (process.ppid === 1) server.close(() => process.exit(0))
}, 10_000).unref()
