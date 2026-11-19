// 把一个小镇的全部运行记录导出成一份独立目录：目前所有有效录像按卷的时间顺序拼成一条时间线，
// 另附游戏内剧本、完成的动作回执、逐条模型调用、居民档案、导出时的世界快照和进程日志。只读用户目录，不改存档。
//
//   node scripts/replay/export.mjs <小镇> [--out 目录] [--compact]
// compact 只保留有效录像原卷、剧本、模型轨迹、居民档案和快照；不生成整条逐帧消息。
import fs from 'node:fs'
import path from 'node:path'
import zlib from 'node:zlib'
import { listReels, loadReel, scriptOf, townDir, townName } from './reels.mjs'
import { createRosterState, messages, parseSegment } from './reel.mjs'

const args = process.argv.slice(2)
const option = (name) => {
  const i = args.indexOf(name)
  return i >= 0 ? args[i + 1] : undefined
}
const town = args[0]
if (!town || town.startsWith('--')) {
  console.error('用法：node scripts/replay/export.mjs <小镇> [--out 目录]')
  process.exit(2)
}

const source = townDir(town)
const stamp = new Date()
  .toISOString()
  .replace(/[-:]/g, '')
  .replace(/\.\d+Z$/, 'Z')
const out = path.resolve(option('--out') ?? path.join('.local', 'exports', `${town}-${stamp}`))
const compact = args.includes('--compact')
if (fs.existsSync(out) && fs.readdirSync(out).length) throw Error(`导出目录不是空的：${out}`)
fs.mkdirSync(out, { recursive: true })

const copy = (from, to) => {
  fs.mkdirSync(path.dirname(to), { recursive: true })
  fs.copyFileSync(from, to)
}
const clockMinutes = (clock) => {
  const match = /^D(\d+)\s+(\d{1,2}):(\d{2})/.exec(clock ?? '')
  return match ? (Number(match[1]) - 1) * 1440 + Number(match[2]) * 60 + Number(match[3]) : null
}
const clockOf = (minutes) => {
  const whole = Math.max(0, Math.floor(minutes + 1e-6))
  const day = Math.floor(whole / 1440) + 1
  const t = whole % 1440
  return `D${day} ${String(Math.floor(t / 60)).padStart(2, '0')}:${String(t % 60).padStart(2, '0')}`
}
// 世界事件正文里已经带主语，日志前缀只保留第一段，不再把「名字 · 事件」再冠一遍。
const notes = (text) =>
  text
    .split('\n')[0]
    .replace(/^#\s*/, '')
    .replace(/^[^ ]+ · /, '')
    .trim()
const eventText = (text, who) =>
  who && text.startsWith(`${who} `) ? text.slice(who.length + 1) : text
const currentWorld = JSON.parse(fs.readFileSync(path.join(source, 'world.json'), 'utf8'))
const worldId =
  currentWorld.world.world_id ??
  currentWorld.world.view?.world_id ??
  currentWorld.population?.world_id
const reels = listReels(town).reverse()

fs.mkdirSync(path.join(out, 'recordings'), { recursive: true })
fs.mkdirSync(path.join(out, 'resident-logs'), { recursive: true })
fs.mkdirSync(path.join(out, 'residents'), { recursive: true })
const messageOut = compact ? null : fs.createWriteStream(path.join(out, 'recordings.jsonl'))
const beatOut = fs.createWriteStream(path.join(out, 'script.jsonl'))
// 下一条写入会重复等待同一个流的错误；监听一次，之后只等可写缓冲腾出空间。
for (const stream of [messageOut, beatOut])
  stream?.on('error', (error) => {
    throw error
  })
const write = (stream, item) =>
  new Promise((resolve, reject) => {
    try {
      if (stream.write(`${JSON.stringify(item)}\n`)) resolve()
      else stream.once('drain', resolve)
    } catch (error) {
      reject(error)
    }
  })
const manifest = []
let lastSim = ''
let lastClock = ''
let lastResidents = ''
for (const { id, header } of reels) {
  const reel = loadReel(town, id)
  const valid = !worldId || header.world_id === worldId
  if (compact && !valid) continue
  copy(path.join(reel.dir, 'header.json'), path.join(out, 'recordings', id, 'header.json'))
  for (const segment of reel.segments) {
    const packed = path.join(reel.dir, `${segment.name}`)
    copy(packed, path.join(out, 'recordings', id, segment.name))
  }
  const { beats, names } = scriptOf(reel)
  if (valid) {
    // 逐卷还原名册并边读边写。每条旧广播里都挂着完整名册，整条时间线留在内存会迅速超出 Node 的堆。
    const state = createRosterState()
    let written = 0
    for (const segment of reel.segments) {
      for (const { t, msg } of messages(parseSegment(segment.text), state)) {
        if (messageOut) await write(messageOut, { reel: id, t, msg })
        written++
        if (msg.clock && clockMinutes(msg.clock) !== null) lastClock = msg.clock
        if (msg.sim_minutes != null) lastSim = msg.sim_minutes
        if (msg.type === 'roster') {
          const key = msg.locales
            .flatMap((l) => l.members.map((m) => `${m.id}:${m.name}`))
            .join('|')
          if (key !== lastResidents) {
            lastResidents = key
            await write(beatOut, {
              reel: id,
              t,
              clock: msg.clock,
              kind: 'roster',
              who: '',
              text: key,
            })
          }
        }
      }
    }
    for (const beat of beats)
      await write(beatOut, {
        reel: id,
        ...beat,
        text: beat.kind === 'event' ? eventText(notes(beat.text), beat.who) : beat.text,
      })
    const acts = new Map()
    const states = createRosterState()
    for (const segment of reel.segments) {
      const text = zlib
        .gunzipSync(fs.readFileSync(path.join(reel.dir, segment.name)))
        .toString('utf8')
      for (const { msg } of messages(parseSegment(text), states)) {
        if (msg.type !== 'roster') continue
        for (const locale of msg.locales)
          for (const member of locale.members)
            for (const result of member.resident_agent?.results ?? [])
              acts.set(`${member.id}|${result.action_id}`, { name: member.name, ...result })
      }
    }
    for (const act of acts.values())
      if (Number.isFinite(act.ended_at))
        await write(beatOut, {
          reel: id,
          t: 0,
          clock: clockOf(act.ended_at),
          kind: 'act',
          who: act.name,
          verb: act.verb,
          status: act.status,
          changes: act.changes,
          reason: act.reason ?? null,
        })
    manifest.push({
      id,
      header,
      valid,
      beats: beats.length,
      names: [...names.values()],
      messages: written,
    })
  } else manifest.push({ id, header, valid, reason: '旧实验世界的录像，保留但不并入当前记录' })
}
await Promise.all([
  messageOut ? new Promise((resolve) => messageOut.end(resolve)) : undefined,
  new Promise((resolve) => beatOut.end(resolve)),
])

const logDir = path.join(source, 'logs')
const logFiles = fs.existsSync(logDir)
  ? fs.readdirSync(logDir).filter((name) => name.endsWith('.jsonl'))
  : []
const traceOut = fs.createWriteStream(path.join(out, 'model-traces.jsonl'))
traceOut.on('error', (error) => {
  throw error
})
let traced = 0
for (const name of logFiles) {
  copy(path.join(logDir, name), path.join(out, 'resident-logs', name))
  for (const line of fs.readFileSync(path.join(logDir, name), 'utf8').split('\n')) {
    if (!line) continue
    try {
      const row = JSON.parse(line)
      const trace = row.trace ?? {}
      await write(traceOut, {
        log: name,
        actor: trace.actor_id,
        at: trace.sim_minutes,
        event: trace.event,
        request: trace.request_id,
        model: row.model_snapshot?.model ?? null,
        data: trace.data,
      })
      traced++
    } catch {} // 进程写到一半的末行不当作记录
  }
}
await new Promise((resolve) => traceOut.end(resolve))

const residentDir = path.join(source, 'residents')
const residents = []
if (fs.existsSync(residentDir))
  for (const name of fs.readdirSync(residentDir)) {
    const dir = path.join(residentDir, name)
    if (!fs.statSync(dir).isDirectory() || name === '.objects') continue
    const files = {}
    for (const file of fs.readdirSync(dir)) {
      if (!file.endsWith('.md')) continue
      files[file] = fs.readFileSync(path.join(dir, file), 'utf8')
      copy(path.join(dir, file), path.join(out, 'residents', name, file))
    }
    residents.push({ name, ...files })
  }
fs.writeFileSync(path.join(out, 'residents.json'), JSON.stringify(residents, null, 2) + '\n')
for (const name of ['world.json', 'map.json', 'host.log', 'creation.json', 'receipt.json']) {
  if (fs.existsSync(path.join(source, name))) copy(path.join(source, name), path.join(out, name))
}
const mapHash = reels.at(-1)?.header.map_hash
const mapRecording = path.join(source, 'recordings', 'maps', `${mapHash}.json.gz`)
if (mapHash && fs.existsSync(mapRecording))
  copy(mapRecording, path.join(out, 'recordings', 'maps', `${mapHash}.json.gz`))

const readme = `# ${townName(town)}（${town}）运行记录

本目录是从小镇数据目录只读导出的一份快照，导出时光是世界时钟 **${lastClock || '未知'}**。导出不会停止或修改正在运行的世界。

## 文件
- \`recordings/\`：原始录像，一卷一段，保留名册变化、事件和场景消息，可重演。
- \`recordings.jsonl\`：当前世界的全部有效录像，按卷的开始时间拼接；每行 \`{ reel, t, msg }\`。${compact ? '（轻量导出未生成。）' : ''}
- \`script.jsonl\`：游戏内剧本，按时间出现角色名单、说过的话、内心想法、世界事件和完成的动作回执。
- \`model-traces.jsonl\`：逐条模型请求、回答、工具调用和结果；按源日志分别保留在 \`resident-logs/\`。
- \`residents/\`、\`residents.json\`：每位居民的当前事务、身份和索引。
- \`world.json\`：导出时的世界存档快照；\`map.json\`：地图。
- \`host.log\`：世界进程日志。${compact ? '旧实验世界的录像未收入轻量包。' : '旧实验世界的录像在 `recordings/` 中也原样保留，不并入上面拼接的记录。'}

世界标识：\`${worldId ?? '未知'}\`；有效录像 ${manifest.filter((r) => r.valid).length} 卷；模型轨迹 ${traced} 条（完整请求与回答见 JSONL）。
`
fs.writeFileSync(path.join(out, 'README.md'), readme)
fs.writeFileSync(
  path.join(out, 'manifest.json'),
  JSON.stringify(
    {
      town,
      name: townName(town),
      world_id: worldId,
      exported_at: new Date().toISOString(),
      last_clock: lastClock,
      last_sim_minutes: lastSim,
      reels: manifest,
      model_traces: traced,
    },
    null,
    2
  ) + '\n'
)
console.log(`已导出：${out}`)
console.log(
  `当前世界 ${worldId}：${manifest.filter((r) => r.valid).length} 卷有效录像，${manifest.filter((r) => !r.valid).length} 卷旧实验录像，模型轨迹 ${traced} 条。`
)
