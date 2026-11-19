// 从导出包写可读剧本。台词与心想取 script.jsonl 的原文，真正完成的动作取录像里的动作回执，规则公告取
// 每份名册里挂着的玩法历史；三者按世界时间合并。同一份导出另写一份精简版，只留推进局势的对白和落地的
// 结果。剧本不替人物补写，也不把选择了、出发了或中途停下写成已经做到。
//
//   node scripts/replay/manuscript.mjs <导出目录> [--out 文件]
import fs from 'node:fs'
import path from 'node:path'
import readline from 'node:readline'
import zlib from 'node:zlib'
import { createRosterState, messages, parseSegment } from './reel.mjs'

const args = process.argv.slice(2)
const option = (name) => {
  const i = args.indexOf(name)
  return i >= 0 ? args[i + 1] : undefined
}
const exported = args[0]
if (!exported || exported.startsWith('--')) {
  console.error('用法：node scripts/replay/manuscript.mjs <导出目录> [--out 文件]')
  process.exit(2)
}

const manifest = JSON.parse(fs.readFileSync(path.join(exported, 'manifest.json'), 'utf8'))
const reels = manifest.reels.filter((reel) => reel.valid)
if (!reels.length) throw Error('导出包里没有当前世界的有效录像')
const out = option('--out') ?? path.join(exported, '剧情剧本.md')
const briefOut = path.join(path.dirname(out), '剧情剧本-精简版.md')
const clockMinutes = (clock) => {
  const match = /^D(\d+)\s+(\d{1,2}):(\d{2})(?::(\d{2}))?/.exec(clock ?? '')
  return match
    ? (Number(match[1]) - 1) * 1440 +
        Number(match[2]) * 60 +
        Number(match[3]) +
        Number(match[4] ?? 0) / 60
    : null
}
const clockOf = (minutes) => {
  const whole = Math.max(0, Math.floor(minutes + 1e-6))
  const day = Math.floor(whole / 1440) + 1
  const t = whole % 1440
  return `D${day} ${String(Math.floor(t / 60)).padStart(2, '0')}:${String(t % 60).padStart(2, '0')}`
}
const firstMinutes = Math.min(
  ...reels.map((reel) => reel.header.from_minutes ?? clockMinutes(reel.header.from_clock))
)
const lastMinutes = Math.max(
  ...reels.map((reel) => reel.header.to_minutes ?? clockMinutes(reel.header.to_clock))
)
const lastClock = clockOf(lastMinutes)
const startClock = clockOf(firstMinutes)
const dayOf = (minutes) => Math.floor(minutes / 1440) + 1
const clockSuffix = (clock) => clock?.replace(/^D\d+\s+/, '') ?? ''
const oneLine = (text) =>
  String(text ?? '')
    .replace(/\s*\n\s*/g, ' ')
    .trim()
// 规则公告的正文已经说了发生什么事；进去掉日志前缀，显示时再标出属于哪一类。
const clean = (text) =>
  oneLine(text)
    .replace(/^【[^】]+】\s*/, '')
    .replace(/^\[D\d+ \d{1,2}:\d{2}(?::\d{2})?\]\s*/, '')
// 只按游戏时钟切幕，不按台词里的关键词猜章节。
const actAt = (minutes) => {
  const t = minutes % 1440
  if (t < 480) return '凌晨'
  if (t < 720) return '上午'
  if (t < 1080) return '午后'
  if (t < 1200) return '傍晚'
  return '夜间'
}
const actId = (minutes) => `${dayOf(minutes)}|${actAt(minutes)}`

const events = []
const eventKeys = new Set()
const addEvent = (event) => {
  if (!(event.at > 0) || !Number.isFinite(event.at)) return
  const token = /token/.exec(event.text ?? '')
  const key =
    event.kind === 'event' && token
      ? `${event.kind}|${event.who}|${token[0]}|${event.text.replace(/\d+/g, '#')}`
      : `${event.kind}|${event.at}|${event.who}|${event.text}`
  if (eventKeys.has(key)) return
  eventKeys.add(key)
  events.push(event)
}
// 游戏中已经说过的话、想过的事和世界事件。导出时已按时间先后写过，这里去掉重开时重复带回的旧话。
for (const line of fs.readFileSync(path.join(exported, 'script.jsonl'), 'utf8').split('\n')) {
  if (!line) continue
  let row
  try {
    row = JSON.parse(line)
  } catch {
    break
  }
  if (row.kind !== 'speech' && row.kind !== 'thought' && row.kind !== 'event') continue
  const at = clockMinutes(row.clock)
  if (at === null) continue
  const event = {
    kind: row.kind,
    at,
    clock: row.clock,
    who: row.who || '',
    text: clean(row.text),
    reel: row.reel,
    t: row.t,
  }
  if (row.kind === 'event' && /已重置：|^停下的村民接着行动了$/.test(event.text)) continue
  addEvent(event)
}

// 每份名册都挂着完整的玩法历史，但公告本身没有时刻，而且重开存档会把旧公告重新挂上。先全部收下，
// 最后按原文去重；能从同时刻的带钟点事件认回时刻的，就用那个时刻，不能的取它第一次出现的名册时间。
const rules = []
const actions = new Map()
const names = new Map()
const seats = new Map()
const actorPlaces = new Map()
const currentGoals = new Map()
let publicRice = null
const readRoster = (msg) => {
  const at = msg.sim_minutes
  if (!Number.isFinite(at)) return
  if (msg.game_mode?.kind === 'survival10') {
    publicRice = msg.game_mode.public_rice ?? publicRice
    for (const seat of msg.game_mode.seats ?? []) {
      seats.set(seat.id, { ...seat })
      names.set(seat.id, seat.name)
    }
    for (const text of msg.game_mode.history ?? []) {
      if (typeof text !== 'string') continue
      // 先全部收下；每份名册都重复挂着同一段历史，等读完再按原文去重。
      rules.push({ at, clock: msg.clock, text: clean(text) })
    }
  }
  for (const locale of msg.locales ?? [])
    for (const member of locale.members)
      if (member.locus?.locale) actorPlaces.set(member.id, member.locus.locale)
  for (const locale of msg.locales ?? [])
    for (const member of locale.members) {
      if (member.name) names.set(member.id, member.name)
      currentGoals.set(member.id, member.resident_agent?.goals ?? [])
      for (const result of member.resident_agent?.results ?? []) {
        if (!result.action_id) continue
        const key = `${member.id}|${result.action_id}`
        if (!actions.has(key)) actions.set(key, { name: member.name, ...result })
      }
    }
}

const segments = new Map()
for (const reel of reels) {
  const dir = path.join(exported, 'recordings', reel.id)
  if (!fs.existsSync(dir)) continue
  const files = fs
    .readdirSync(dir)
    .filter((name) => /^\d{3,}\.jsonl(?:\.gz)?$/.test(name))
    .sort(
      (a, b) =>
        Number(a.slice(0, 3)) - Number(b.slice(0, 3)) ||
        Number(b.endsWith('.gz')) - Number(a.endsWith('.gz'))
    )
  for (const name of files) {
    const index = Number(name.slice(0, 3))
    const reelSegments = segments.get(reel.id) ?? new Map()
    if (!reelSegments.has(index) || name.endsWith('.gz'))
      reelSegments.set(index, path.join(dir, name))
    segments.set(reel.id, reelSegments)
  }
}
if (!segments.size) {
  const packed = path.join(exported, 'recordings.jsonl')
  if (!fs.existsSync(packed)) throw Error('导出包既没有录像原卷，也没有合并后的 recordings.jsonl')
  segments.set('recordings.jsonl', new Map([[0, packed]]))
}

// 有两种读法：原卷压缩时直接解压，之后接上录像行；一些导出包只留合成的 recordings.jsonl，
// 其每一行已有还原后的 { msg }。两种都只取名册，台词以 script.jsonl 为准。
const packed = segments.size === 1 && segments.has('recordings.jsonl')
for (const group of segments.values()) {
  const state = createRosterState()
  for (const file of [...group.values()].sort((a, b) =>
    a.localeCompare(b, undefined, { numeric: true })
  )) {
    const stream = fs.createReadStream(file)
    const lines = readline.createInterface({
      input: file.endsWith('.gz') ? stream.pipe(zlib.createGunzip()) : stream,
      crlfDelay: Infinity,
    })
    if (packed) {
      for await (const line of lines) {
        if (!line) continue
        let row
        try {
          row = JSON.parse(line)
        } catch {
          continue
        }
        if (row.msg?.type === 'roster') readRoster(row.msg)
      }
    } else {
      // 原卷里只有名册帧需要还原；台词和世界事件已由导出写成 script.jsonl。
      for await (const line of lines) {
        if (!line) continue
        let row
        try {
          row = JSON.parse(line)
        } catch {
          break
        }
        if (row.k || row.d) {
          for (const { msg } of messages([row], state)) if (msg.type === 'roster') readRoster(msg)
        }
      }
    }
  }
}

const meaningfulAct = (verb) =>
  ![
    '前往',
    '停留',
    '穿过',
    '走路',
    '等待',
    '查看四周',
    '查看公告栏',
    '清点货品',
    '坐下',
    '躺下',
    '站起',
    '歇着',
  ].includes(verb)
for (const action of actions.values()) {
  if (action.status !== 'succeeded' || !Number.isFinite(action.ended_at)) continue
  if (!meaningfulAct(action.verb)) continue
  const changes = (action.changes ?? []).map(clean).filter(Boolean)
  if (!changes.length) continue
  if (action.verb === '取粮') {
    // 取粮回执里的公仓余额点出了这一次账本变化；同一句等待/提示不另写一笔。
    const first = changes.find((text) => /取回|公仓|私粮/.test(text))
    if (!first || !/取回/.test(first)) continue
    addEvent({
      kind: 'act',
      at: action.ended_at,
      clock: clockOf(action.ended_at),
      who: action.name,
      verb: action.verb,
      changes: [first],
    })
    continue
  }
  addEvent({
    kind: 'act',
    at: action.ended_at,
    clock: clockOf(action.ended_at),
    who: action.name,
    verb: action.verb,
    changes,
  })
}

events.sort((a, b) => a.at - b.at || (a.kind === 'speech' ? -1 : a.kind === 'thought' ? 0 : 1))
// 历史公告不带时刻，而且重开存档时旧账会被原样重新挂上，不能把“后来才看到”当成“后来才发生”。每句话
// 只认第一次看见的钟点。日和夜的开场公告在游戏时是固定节拍（08:00 开场、18:00 入夜），而旧账可能在
// 归档后重新挂出，所以先认出它属于哪一天，再按固定的节拍落回那一幕。
const timedRules = events.filter((event) => /\[D\d+ \d{1,2}:\d{2}/.test(event.text))
const groupedRules = Map.groupBy(rules, (rule) => rule.text)
for (const [text, list] of groupedRules) {
  const first = list[0]
  const matching = timedRules.find((event) => event.text.endsWith(text))
  if (matching) addEvent({ kind: 'rule', at: matching.at, clock: matching.clock, who: '', text })
  else if (
    /^第\d+\/10天开始/.test(text) ||
    text === '夜间行动：睡觉、偷公仓、偷别人家、守家，每人仅一次。'
  ) {
    const day = dayOf(clockMinutes(first.clock)) ?? dayOf(first.at)
    const at = (day - 1) * 1440 + (/^第\d+\/10天开始/.test(text) ? 480 : 1080)
    addEvent({ kind: 'rule', at, clock: clockOf(at), who: '', text })
  } else addEvent({ kind: 'rule', ...first })
}
events.sort((a, b) => a.at - b.at || (a.kind === 'speech' ? -1 : a.kind === 'thought' ? 0 : 1))
const eventCounts = Object.groupBy(events, (event) => event.kind)
console.log(
  `事件 ${events.length} 条：${Object.entries(eventCounts)
    .map(([kind, list]) => `${kind} ${list.length}`)
    .join('，')}`
)

const town = manifest.name ?? manifest.town
const map = JSON.parse(fs.readFileSync(path.join(exported, 'map.json'), 'utf8'))
const world = JSON.parse(fs.readFileSync(path.join(exported, 'world.json'), 'utf8')).world
for (const [id, actor] of Object.entries(world?.actors ?? {}))
  if (actor.locus?.locale) actorPlaces.set(id, actor.locus.locale)
const titles = new Map((map.topology?.locales ?? []).map((locale) => [locale.id, locale.name]))
const speakers = new Map()
for (const event of events)
  if (event.kind === 'speech' || event.kind === 'thought') speakers.set(event.who, event.clock)
const residentNames = [...new Set([...names.values(), ...speakers.keys()])]
const opening =
  events.find((event) => event.kind === 'rule' && /第1\/10天开始/.test(event.text))?.text ??
  '游戏内规则公告不可用。'
const openingRice = /公仓(\d+)袋/.exec(opening)?.[1] ?? '21'
const outSeats = [
  ...new Set(
    events.flatMap((event) => {
      const match = /^(\d+号[^出]+)出局（([^）]+)）/.exec(event.text ?? '')
      return event.kind === 'rule' && match ? [`${match[1]}：${event.clock}，${match[2]}`] : []
    })
  ),
]
const present = [...seats.values()]
  .filter((seat) => !seat.out)
  .map((seat) => `${seat.seat}号${seat.name}`)
const today = dayOf(lastMinutes)
const nightStarted = events.some(
  (event) => event.kind === 'rule' && event.at > lastMinutes - 1440 && /夜间行动/.test(event.text)
)

const intro = (title, note) => [
  `# ${title}`,
  '',
  `> ${note}`,
  '',
  '## 人物与规则摘要',
  '',
  `- **居民**：${residentNames.join('、')}。`,
  `- 开局公仓 ${openingRice} 袋，每人晚餐吃 1 袋，连着两晚没吃上饭出局；公共农田每天最多四人报名，实际劳动满八小时才产一袋，领米要到公共粮仓完成取粮。`,
  '- 白天工作或领米二选一；第 2、4、6、8、10 天晚餐后开篝火会；第十天由出局者组成陪审团选出冠军。',
  '- 台词、心想和完成动作都取录像原文；只有回执为“已完成”的动作写成结果，“想去”“出发了”“做到一半”都不算完成。',
  '- “心想”只写在括号里，是这个人自己的念头，不是当众说出的话；观众能看见，不表示其他居民知道。',
  '',
  '## 写在开场前',
  '',
  '> 这是只读导出时每位仍在场居民自己那条进行中的事务原文；它是各人的想法与承诺，不是已经完成的事实。',
  '',
  ...[...names].flatMap(([id, name]) => {
    if (seats.get(id)?.out) return []
    const goal = currentGoals.get(id)?.find((item) => item.title)
    return goal ? [`**${name}**：${oneLine(goal.title)}。${oneLine(goal.commitment)}`, ''] : []
  }),
]

const briefKeywords =
  /出局|投票|一票|名额|八小时|袋|吃|晚饭|口粮|守|偷|账|规矩|约|预支|借|换|查|验|睡|干活|名单|满工|缺席|火边|公仓|粮仓|私粮|劳动|悄悄话|陪审/
const full = intro(
  `${town}·当前剧情剧本`,
  `从 ${startClock} 到 ${lastClock}，按世界时间整理 ${residentNames.length} 位居民的公开对话、心中所想、真正完成的动作和规则公告。`
)
const brief = intro(
  `${town}·当前剧情精简剧本`,
  `从 ${startClock} 到 ${lastClock}，同一段运行的阅读版：留下推进局势的对话、真正落地的动作、账本和规则结果；台词不改写。`
)
let fullDay = 0
let fullAct = ''
let briefDay = 0
let briefAct = ''
let lastFullSpeaker = ''
let lastBriefSpeaker = ''

const dayHeading = (doc, day) => {
  doc.push(`## 第 ${day} 天`, '')
}
const actHeading = (doc, minutes) => {
  doc.push(`### ${actAt(minutes)}`, '')
}
const lineOf = (event) => {
  if (event.kind === 'speech')
    return `**${event.who}**（${clockSuffix(event.clock)}）：${event.text}`
  if (event.kind === 'thought') return `*（${event.who} 心想：${event.text}）*`
  if (event.kind === 'event') return `> **${event.clock} · 世界** ${event.text}`
  if (event.kind === 'rule') return `> **${event.clock} · 规则** ${event.text}`
  return `> **${event.clock} · 完成** ${event.who}完成「${event.verb}」：${event.changes.join('；')}`
}
const meaningful = (event) =>
  event.kind === 'result' ||
  event.kind === 'act' ||
  event.kind === 'rule' ||
  event.kind === 'body' ||
  (event.kind === 'event' && !/来到小镇/.test(event.text))

// 完整版按连续对话分段；每段的前后间隔不跨越日或幕。精简版在原时间线里挑关键句，不改变先后。
const blocks = []
for (const event of events) {
  if (
    !blocks.length ||
    actId(event.at) !== actId(blocks.at(-1).at) ||
    event.at - blocks.at(-1).at > 20
  )
    blocks.push({ at: event.at, items: [] })
  blocks.at(-1).items.push(event)
}
for (const block of blocks) {
  const day = dayOf(block.at)
  if (day !== fullDay) {
    fullDay = day
    fullAct = ''
    dayHeading(full, day)
  }
  const act = actAt(block.at)
  if (act !== fullAct) {
    fullAct = act
    actHeading(full, block.at)
    lastFullSpeaker = ''
  }
  for (const event of block.items) {
    if (
      event.kind === 'speech' &&
      event.who === lastFullSpeaker &&
      full.at(-2)?.startsWith(`**${event.who}**`)
    )
      full.push(`（续）${event.text}`, '')
    else full.push(lineOf(event), '')
    if (event.kind === 'speech') lastFullSpeaker = event.who
    else if (event.kind !== 'thought') lastFullSpeaker = ''
  }
}

const briefEvents = events.filter(
  (event) =>
    event.kind === 'act' ||
    event.kind === 'rule' ||
    (event.kind === 'event' && !/来到小镇/.test(event.text)) ||
    (event.kind === 'speech' && briefKeywords.test(event.text))
)
let selectedAt = null
let selectedCount = 0
for (const event of briefEvents) {
  const day = dayOf(event.at)
  if (day !== briefDay) {
    briefDay = day
    briefAct = ''
    dayHeading(brief, day)
    selectedCount = 0
  }
  const act = actAt(event.at)
  if (act !== briefAct) {
    briefAct = act
    actHeading(brief, event.at)
    lastBriefSpeaker = ''
    selectedCount = 0
  }
  if (event.kind === 'speech') {
    if (selectedAt !== null && event.at - selectedAt > 45 && selectedCount >= 8) continue
    if (event.who === lastBriefSpeaker) brief.push(`（续）${event.text}`, '')
    else brief.push(lineOf(event), '')
    lastBriefSpeaker = event.who
    selectedAt = event.at
    selectedCount++
  } else {
    brief.push(lineOf(event), '')
    lastBriefSpeaker = ''
  }
}

const ending = (doc) => {
  doc.push('---', '', '## 当前进度', '')
  doc.push(
    `录像停在 **${lastClock}**。仍在场 ${present.length} 人：${present.join('、') || '无'}。`,
    ''
  )
  if (outSeats.length) doc.push(`已经出局：${outSeats.join('；')}。`, '')
  if (Number.isFinite(publicRice))
    doc.push(`导出时公仓账本为 **${publicRice} 袋**；个人私粮以导出快照为准。`, '')
  const places = [...names]
    .map(([id, name]) => {
      if (seats.get(id)?.out) return `${name}已离场`
      const locale = actorPlaces.get(id)
      const place = titles.get(locale) ?? locale?.replace(/_station$/, '') ?? '未记录的处所'
      return `${name}在${place}`
    })
    .filter(Boolean)
    .join('；')
  if (places) doc.push(`导出时的位置：${places}。`, '')
  doc.push(
    `这一段覆盖第 1 天到第 ${today} 天。第 ${today} 天${nightStarted ? '已进入夜间行动' : '的后续尚未发生'}，冠军、陪审团或以后几天的剧情不能提前写成结局。`,
    ''
  )
  doc.push(
    '',
    '---',
    '',
    `*整理自 ${lastClock} 的只读导出：${reels.length} 卷有效录像。动作只以“已完成”回执为准，规则历史按原文去重。*`
  )
}
ending(full)
ending(brief)

fs.writeFileSync(out, full.join('\n') + '\n')
fs.writeFileSync(briefOut, brief.join('\n') + '\n')
console.log(`剧本：${out}（${full.length} 行）`)
console.log(`精简版：${briefOut}（${brief.length} 行）`)
