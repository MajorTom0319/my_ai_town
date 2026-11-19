// 实时剧本的纯逻辑：筛选、按场景分组、写导出文本。面板只负责画，规则都在这里，测试能直接载入源码跑。
import type { ManuscriptEntry, ManuscriptSummary } from '../protocol'

export type ViewMode = 'all' | 'public'

export interface EntryFilter {
  view: ViewMode
  /** 空表示不限 */
  day?: string
  actor?: string
  /** 隐藏心声；默认的心声是本人的自述，不是事实 */
  noThought?: boolean
}

/** 日子：D3 07:40 → D3；时钟格式不认识时返回空串，不按正文猜。 */
export function dayOf(entry: Pick<ManuscriptEntry, 'clock'>): string {
  return /^D\d+/.exec(entry.clock)?.[0] ?? ''
}

/** 裁判尚未公开的秘密：即使旧流水或导入素材误标成公开，也不能出现在「仅公开」里。 */
export function isSecret(entry: Pick<ManuscriptEntry, 'visibility' | 'text'>): boolean {
  return entry.visibility === 'private' || entry.text.includes('裁判全知')
}

export function matches(entry: ManuscriptEntry, filter: EntryFilter): boolean {
  if (filter.view === 'public' && isSecret(entry)) return false
  if (filter.day && dayOf(entry) !== filter.day) return false
  if (filter.actor && entry.actor !== filter.actor) return false
  if (filter.noThought && entry.tone === 'thought') return false
  return true
}

/** 按天、再按游戏时钟切幕：只认时钟，不按台词里的关键词猜章节。 */
export function actOf(clock: string): string {
  const match = /^D\d+\s+(\d{1,2}):(\d{2})/.exec(clock)
  if (!match) return '一天里'
  const minutes = Number(match[1]) * 60 + Number(match[2])
  if (minutes < 480) return '凌晨'
  if (minutes < 720) return '上午'
  if (minutes < 1080) return '午后'
  if (minutes < 1200) return '傍晚'
  return '夜间'
}

export interface Scene {
  /** 天 + 幕，例如 `D2 午后` */
  title: string
  entries: ManuscriptEntry[]
}

/** 已按编号排好的条目 → 每幕一段；同一幕里保留原顺序。 */
export function scenes(entries: ManuscriptEntry[]): Scene[] {
  const out: Scene[] = []
  let last = ''
  for (const entry of entries) {
    const title = `${dayOf(entry)} ${actOf(entry.clock)}`.trim()
    if (title !== last) {
      out.push({ title, entries: [] })
      last = title
    }
    out[out.length - 1].entries.push(entry)
  }
  return out
}

const LABEL: Record<ManuscriptEntry['kind'], string> = {
  speech: '发言',
  thought: '心声·自述',
  action: '结果',
  choice: '选择',
  announcement: '公告',
  world: '世界',
  director: '导演',
}

export function labelOf(entry: ManuscriptEntry): string {
  return LABEL[entry.kind] ?? entry.kind
}

/** 一条剧本的文本：谁、怎么说、发生了什么，秘密标明「未公开」。 */
export function lineOf(entry: ManuscriptEntry): string {
  const who = entry.name ?? entry.actor ?? ''
  const secret = entry.visibility === 'private' ? '〔未公开〕' : ''
  switch (entry.tone) {
    case 'thought':
      return `${entry.clock} ${who}〔心声·自述〕${secret}：${entry.text}`
    case 'result':
      return `${entry.clock} ${who}〔结果〕：${entry.text}`
    case 'announcement':
      return `${entry.clock}〔公告〕：${entry.text}`
    default:
      return entry.kind === 'speech'
        ? `${entry.clock} ${who}〔发言〕${secret}：“${entry.text}”`
        : `${entry.clock}${who ? ` ${who}` : ''}：${entry.text}`
  }
}

/** 概览里能被当前筛选看到的判断；来源全被筛掉时也不显示这条判断，避免绕过「仅公开」。 */
export interface SummaryLine {
  text: string
  /** 当前视图下能看到、并已经读进浏览器的来源条目编号 */
  sources: number[]
}

export function summarySections(
  summary: ManuscriptSummary,
  entries: ManuscriptEntry[],
  filter: EntryFilter
): { title: string; lines: SummaryLine[] }[] {
  const bySeq = new Map(entries.map((entry) => [entry.seq, entry]))
  const sections = [
    ['主要冲突', summary.conflicts],
    ['待兑现事项', summary.promises],
    ['未解问题', summary.questions],
  ] as const
  return sections.flatMap(([title, items]) => {
    const lines = items.flatMap(([text, ...cited]) => {
      const visible = cited
        .map(Number)
        .flatMap((source) => bySeq.get(source) ?? [])
        .filter((entry) => matches(entry, filter))
      return visible.length ? [{ text, sources: visible.map((entry) => entry.seq) }] : []
    })
    return lines.length ? [{ title, lines }] : []
  })
}

/** 当前筛选下的一份 Markdown；保留时钟和来源编号，方便回去核对原话。 */
export function toMarkdown(entries: ManuscriptEntry[], filter: EntryFilter): string {
  const shown = entries.filter((entry) => matches(entry, filter))
  const lines = [
    '# 实时剧本',
    '',
    filter.view === 'public' ? '> 只含公开信息。' : '> 含心声、私聊与未公开结果，仅供旁观。',
    '',
  ]
  for (const scene of scenes(shown)) {
    lines.push(`## ${scene.title}`, '')
    for (const entry of scene.entries) {
      lines.push(`- ${lineOf(entry)}  \`${entry.source}\``)
    }
    lines.push('')
  }
  return lines.join('\n')
}
