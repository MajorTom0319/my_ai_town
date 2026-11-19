// 实时剧本：地图右侧的阅读面板，底部「剧本」格和菜单里的「剧本」都能开。上面是截至某一编号的
// 剧情概览，下面按天、幕和场景排世界确认过的台词、心声、结果与公告；可筛公开／全知、日期、
// 人物和心声，可导出 Markdown。与导演面板互斥，不另开连接。自动整理由世界设定控制，
// 它广播来的新概览直接采用。
import './style.css'
import { $, el } from '../dom'
import type {
  ClientMsg,
  ManuscriptEntry,
  ManuscriptPage,
  ManuscriptSummary,
  ServerMsg,
} from '../protocol'
import {
  matches,
  scenes,
  summarySections,
  toMarkdown,
  type EntryFilter,
  type Scene,
  type ViewMode,
} from './format'

/** 一次查询要多少条；打开面板先读最近这些，往上翻再按编号往前补 */
const PAGE = 400

export function createManuscriptPanel(deps: {
  send: (msg: ClientMsg) => void
  /** 当前世界标识；读取时核对，重连不把旧局条目混进新局 */
  worldId: () => string | undefined
  /** 打开时收起其他大面板 */
  onOpen?: () => void
}) {
  let entries: ManuscriptEntry[] = []
  let cursor = 0
  let world = ''
  let filter: EntryFilter = { view: 'all' }
  let online = false
  let loading = false
  let exporting = false
  let overview: ManuscriptSummary | null = null
  /** 等回执的手动整理；自动整理的结果不会清掉它 */
  let summaryRequest = ''
  let summaryError = ''
  let noteText = ''
  /** 导出逐页续读时，等下一次 manuscript_page */
  let waiting:
    | {
        world: string
        resolve: (page: ManuscriptPage) => void
        reject: (error: Error) => void
        timer: ReturnType<typeof setTimeout>
      }
    | undefined
  /** 正在往旧的一端读；读到头以后不再重复请求 */
  let older = true
  /** 用户上翻后不再自动跟随；新条目计数显示在底部 */
  let following = true
  let unread = 0
  /** 已收过的编号：断线补齐与重发时按编号去重 */
  const seen = new Set<number>()
  let state: 'connected' | 'disconnected' | 'reset' = 'disconnected'

  const root = el('section')
  root.id = 'manuscript-panel'
  root.className = 'manuscript-panel'
  root.hidden = true
  root.setAttribute('aria-label', '实时剧本')
  root.dataset.volume = 'live'

  const top = el('header', '', 'manuscript-top')
  const back = el('button', '返回', 'manuscript-back')
  back.type = 'button'
  const title = el('h2', '实时剧本')
  const clock = el('span', '', 'manuscript-clock')
  const close = el('button', '收起', 'manuscript-close')
  close.type = 'button'
  top.append(back, title, clock, close)

  const summaryBox = el('section', '', 'manuscript-summary')
  const summaryHead = el('div', '', 'manuscript-summary-head')
  const summaryTitle = el('h3', '剧情概览')
  const asOf = el('span', '', 'manuscript-asof')
  const generate = el('button', '整理概览', 'manuscript-generate')
  generate.type = 'button'
  generate.disabled = true
  generate.title = '由模型归纳局势、冲突和悬念，每次判断都能指回实录；模型不可用时不影响世界'
  summaryHead.append(summaryTitle, asOf, generate)
  const summaryText = el(
    'p',
    '概览暂未更新。下面的实录按世界确认的事实追加；心声是当事人的自述，不等于事实。',
    'manuscript-summary-text'
  )
  const summaryBody = el('div', '', 'manuscript-summary-body')
  summaryBody.append(summaryText)
  summaryBox.append(summaryHead, summaryBody)

  const filters = el('div', '', 'manuscript-filters')
  const view = el('select', '', 'manuscript-view')
  view.setAttribute('aria-label', '看的范围')
  for (const [value, text] of [
    ['all', '全知（含心声与私密）'],
    ['public', '仅公开信息'],
  ] as [ViewMode, string][]) {
    const option = el('option', text)
    option.value = value
    view.append(option)
  }
  const day = el('select', '', 'manuscript-day')
  day.setAttribute('aria-label', '日期')
  const actor = el('select', '', 'manuscript-actor')
  actor.setAttribute('aria-label', '人物')
  const thoughts = el('label', '', 'manuscript-check')
  const thoughtInput = document.createElement('input')
  thoughtInput.type = 'checkbox'
  thoughts.append(thoughtInput, document.createTextNode('显示心声'))
  thoughtInput.checked = true
  const follow = el('button', '跟随最新', 'manuscript-follow')
  follow.type = 'button'
  const exportButton = el('button', '导出', 'manuscript-export')
  exportButton.type = 'button'
  filters.append(view, day, actor, thoughts, follow, exportButton)

  const list = el('div', '', 'manuscript-list')
  list.setAttribute('aria-live', 'polite')
  const olderHint = el('p', '向上滚动读取更早的剧情', 'manuscript-older')
  olderHint.hidden = true
  const jump = el('button', '', 'manuscript-jump')
  jump.type = 'button'
  jump.hidden = true
  const status = el('p', '', 'manuscript-status')
  status.hidden = true
  const body = el('div', '', 'manuscript-body')
  body.append(summaryBox, filters, list)
  list.append(olderHint)
  root.append(top, body, jump, status)
  $('map').append(root)

  for (const type of ['pointerdown', 'dblclick', 'wheel'] as const)
    root.addEventListener(type, (event) => event.stopPropagation())
  root.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape' || event.repeat || event.isComposing) return
    event.preventDefault()
    event.stopPropagation()
    setOpen(false)
  })
  list.addEventListener('scroll', () => {
    if (list.scrollTop < 32 && older && !loading) loadPage({ before: entries[0]?.seq })
    const atEnd = list.scrollHeight - list.scrollTop - list.clientHeight < 24
    if (atEnd && !following) {
      following = true
      unread = 0
      jump.textContent = ''
      paint()
    } else if (!atEnd && following) {
      following = false
    }
  })

  view.onchange = () => {
    filter = { ...filter, view: view.value as ViewMode }
    paint()
  }
  day.onchange = () => {
    filter = { ...filter, day: day.value || undefined }
    paint()
  }
  actor.onchange = () => {
    filter = { ...filter, actor: actor.value || undefined }
    paint()
  }
  thoughtInput.onchange = () => {
    filter = { ...filter, noThought: !thoughtInput.checked }
    paint()
  }
  follow.onclick = () => {
    following = true
    unread = 0
    jump.textContent = ''
    loading = false
    list.scrollTop = list.scrollHeight
    paint()
  }
  jump.onclick = follow.onclick
  generate.onclick = () => {
    const world_id = deps.worldId()
    if (!world_id || !online) return note('还没连上小镇，稍后再整理。', true)
    if (summaryRequest) return
    summaryRequest = `summary-${cursor}-${Date.now()}`
    summaryError = ''
    paintSummary()
    deps.send({
      type: 'summarize_manuscript',
      world_id,
      request_id: summaryRequest,
    })
  }
  exportButton.onclick = () => {
    if (!online) return note('还没连上小镇，先恢复连接再导出。', true)
    void exportAll()
  }

  /** 导出从编号 1 起逐页补齐整个世界的剧本，而不是只写浏览器里还没翻到的几页。 */
  async function exportAll() {
    if (exporting) return
    exporting = true
    const previous = noteText
    note('正在读取完整剧本…')
    try {
      const all = [...entries]
      const have = new Set(all.map((entry) => entry.seq))
      let next = 0
      for (;;) {
        const page = await ask(next)
        const incoming = page.entries.filter((entry) => !have.has(entry.seq))
        for (const entry of incoming) have.add(entry.seq)
        all.push(...incoming)
        if (!incoming.length || page.entries.length < PAGE || page.cursor <= next) break
        next = incoming.at(-1)!.seq
      }
      all.sort((a, b) => a.seq - b.seq)
      download(toMarkdown(all, filter))
      note(previous)
    } catch (error) {
      note(`导出失败：${error instanceof Error ? error.message : String(error)}`, true)
    } finally {
      exporting = false
    }
  }

  function download(text: string) {
    const blob = new Blob([text], { type: 'text/markdown;charset=utf-8' })
    const link = document.createElement('a')
    link.href = URL.createObjectURL(blob)
    link.download = `实时剧本-${world.slice(0, 12) || '本局'}.md`
    link.hidden = true
    document.body.append(link)
    link.click()
    link.remove()
    setTimeout(() => URL.revokeObjectURL(link.href), 2000)
  }
  close.onclick = () => setOpen(false)
  back.onclick = () => setOpen(false)

  function setOpen(open: boolean) {
    root.hidden = !open
    button.setAttribute('aria-pressed', String(open))
    if (!open) return
    deps.onOpen?.()
    following = true
    unread = 0
    if (!entries.length) loadPage({})
    else jump.hidden = true
    requestAnimationFrame(() => {
      list.scrollTop = list.scrollHeight
      close.focus({ preventScroll: true })
    })
  }

  function loadPage(query: { after?: number; before?: number }) {
    if (!online) {
      note('还没连上小镇；连上后会自动读取。')
      return
    }
    loading = true
    deps.send({ type: 'get_manuscript', world_id: deps.worldId(), ...query, limit: PAGE })
  }

  function ask(after: number): Promise<ManuscriptPage> {
    return new Promise((resolve, reject) => {
      waiting?.timer && clearTimeout(waiting.timer)
      const timer = setTimeout(() => {
        waiting = undefined
        reject(new Error('等小镇回复超时'))
      }, 8000)
      waiting = { world: deps.worldId() ?? '', resolve, reject, timer }
      deps.send({
        type: 'get_manuscript',
        world_id: deps.worldId(),
        after,
        limit: PAGE,
      })
    })
  }

  function note(text: string, bad = false) {
    noteText = text
    status.textContent = text
    status.classList.toggle('error', bad)
    status.hidden = !text
  }

  function accept(page: { world_id: string; entries: ManuscriptEntry[]; cursor: number }) {
    const before = list.scrollHeight - list.scrollTop
    if (world && page.world_id !== world) {
      entries = []
      seen.clear()
      unread = 0
      following = true
      older = true
    }
    world = page.world_id
    cursor = page.cursor
    merge(page.entries)
    // 一页不满说明那一头已经读到头；从最新一页开始读时还要试一次更旧的记录。
    if (page.entries.length < PAGE) older = false
    loading = false
    if (waiting && waiting.world === page.world_id) {
      clearTimeout(waiting.timer)
      const done = waiting
      waiting = undefined
      done.resolve(page)
    }
    paint()
    if (following) requestAnimationFrame(() => (list.scrollTop = list.scrollHeight))
    else if (page.entries.length)
      requestAnimationFrame(() => (list.scrollTop = list.scrollHeight - before))
  }

  function merge(incoming: ManuscriptEntry[]) {
    let fresh = 0
    for (const entry of incoming) {
      if (seen.has(entry.seq)) continue
      seen.add(entry.seq)
      entries.push(entry)
      fresh++
    }
    entries.sort((a, b) => a.seq - b.seq)
    if (fresh && !root.hidden && !following) unread += fresh
  }

  function paint() {
    paintOptions()
    const shown = entries.filter((entry) => matches(entry, filter))
    list.replaceChildren()
    olderHint.hidden = !older
    if (older) list.append(olderHint)
    if (!shown.length) {
      list.append(
        el(
          'p',
          online
            ? '还没有可读的剧情。世界确认过的台词、心声和结果会在这里自动追加。'
            : '还没连上小镇。',
          'manuscript-empty'
        )
      )
    } else {
      for (const scene of scenes(shown)) list.append(sceneNode(scene))
    }
    const latest = shown.at(-1)
    clock.textContent = latest ? latest.clock : ''
    paintSummary()
    jump.hidden = following || unread === 0
    jump.textContent = `新增 ${unread} 条，点击查看`
    if (!following && unread === 0 && !jump.hidden) jump.textContent = '回到最新'
    if (loading) note('正在读取…')
    else if (!online) note('已断线；恢复后按编号补齐。')
    else note('')
  }

  function paintSummary() {
    const latest = entries.at(-1)
    generate.disabled = !!summaryRequest || !latest || (!!overview && overview.through_seq >= latest.seq)
    generate.title = summaryRequest
      ? '正在归纳最近的实录'
      : !latest
        ? '世界还没有可整理的剧情'
        : overview && overview.through_seq >= latest.seq
          ? '这条概览已经覆盖了目前全部实录'
          : '由模型归纳局势、冲突和悬念，每次判断都能指回实录；模型不可用时不影响世界'
    generate.textContent = summaryRequest ? '整理中…' : '整理概览'
    asOf.textContent = overview
      ? `截至 ${overview.through_clock} · #${overview.through_seq}`
      : entries.length
        ? `截至 ${entries.at(-1)!.clock} · #${entries.at(-1)!.seq}`
        : ''
    summaryBody.replaceChildren()
    // 概览模型非关键路径：失败只在概览里提示，不打断正在读的实录。
    if (summaryError) summaryBody.append(el('p', summaryError, 'manuscript-summary-error'))
    if (!overview) {
      summaryBody.append(summaryText)
      return
    }
    summaryBody.append(el('p', overview.situation, 'manuscript-summary-text'))
    for (const section of summarySections(overview, entries, filter)) {
      const box = el('section', '', 'manuscript-summary-section')
      box.append(el('h4', section.title))
      const lines = el('ul')
      for (const line of section.lines) {
        const item = el('li', line.text)
        if (line.sources.length) {
          const citations = el('span', '', 'manuscript-citations')
          for (const seq of line.sources) {
            const cite = el('button', `#${seq}`, 'manuscript-cite')
            cite.type = 'button'
            cite.title = '回到这条判断对应的实录'
            cite.onclick = () => reveal(seq)
            citations.append(cite)
          }
          item.append(citations)
        }
        lines.append(item)
      }
      box.append(lines)
      summaryBody.append(box)
    }
  }

  function paintOptions() {
    const days = [...new Set(entries.map((entry) => /^D\d+/.exec(entry.clock)?.[0] ?? ''))].filter(
      Boolean
    )
    const people = new Map<string, string>()
    for (const entry of entries) {
      if (entry.actor) people.set(entry.actor, entry.name ?? entry.actor)
    }
    fill(
      day,
      '全部日期',
      days.map((value) => [value, value])
    )
    fill(
      actor,
      '全部人物',
      [...people.entries()].sort((a, b) => a[1].localeCompare(b[1], 'zh-Hans-CN'))
    )
  }

  function fill(select: HTMLSelectElement, any: string, options: [string, string][]) {
    const current = select.value
    select.replaceChildren()
    const all = el('option', any)
    all.value = ''
    select.append(all)
    for (const [value, text] of options) {
      const option = el('option', text)
      option.value = value
      select.append(option)
    }
    select.value = options.some(([value]) => value === current) ? current : ''
  }

  function sceneNode(scene: Scene): HTMLElement {
    const box = el('section', '', 'manuscript-scene')
    box.append(el('h3', scene.title, 'manuscript-scene-title'))
    for (const entry of scene.entries) box.append(entryNode(entry))
    return box
  }

  function entryNode(entry: ManuscriptEntry): HTMLElement {
    const row = el('article', '', `manuscript-entry ${entry.tone}`)
    row.dataset.seq = String(entry.seq)
    if (entry.visibility === 'private') row.classList.add('private')
    const head = el('div', '', 'manuscript-entry-head')
    head.append(el('span', entry.clock.replace(/^D\d+\s+/, ''), 'manuscript-time'))
    if (entry.name || entry.actor) {
      head.append(el('b', entry.name ?? entry.actor!, 'manuscript-who'))
    }
    head.append(el('span', label(entry), 'manuscript-tag'))
    if (entry.visibility === 'private') head.append(el('span', '未公开', 'manuscript-secret'))
    const text = el('p', entry.text, 'manuscript-text')
    const source = el('code', entry.source, 'manuscript-source')
    source.title = '来源编号：回去核对原话和回执'
    row.append(head, text, source)
    return row
  }

  function label(entry: ManuscriptEntry): string {
    if (entry.tone === 'thought') return '心声·自述'
    if (entry.tone === 'result') return '结果'
    if (entry.kind === 'choice') return '选择'
    if (entry.kind === 'announcement') return '公告'
    if (entry.kind === 'speech') return '发言'
    if (entry.kind === 'director') return '导演'
    return '世界'
  }

  /** 从概览的来源编号回到那条实录：放开挡住它的筛选，再滚动过去；没读进浏览器时先从服务器补齐。 */
  async function reveal(seq: number) {
    let target = entries.find((entry) => entry.seq === seq)
    if (!target && online) {
      try {
        for (;;) {
          const page = await ask(seq - 1)
          merge(page.entries)
          target = page.entries.find((entry) => entry.seq === seq)
          if (target || page.entries.length < PAGE || page.cursor < seq) break
        }
      } catch (error) {
        return note(`读取依据失败：${error instanceof Error ? error.message : String(error)}`, true)
      }
    }
    if (!target) return note(`找不到 #${seq} 这条实录。`, true)
    if (!matches(target, filter)) {
      filter = { ...filter, view: 'all', day: undefined, actor: undefined, noThought: false }
      view.value = 'all'
      thoughtInput.checked = true
      paint()
    }
    following = false
    requestAnimationFrame(() => {
      const row = list.querySelector<HTMLElement>(`[data-seq="${seq}"]`)
      if (!row) return
      row.scrollIntoView({ block: 'center' })
      row.classList.add('manuscript-hit')
      setTimeout(() => row.classList.remove('manuscript-hit'), 1600)
      jump.hidden = false
      jump.textContent = `回到最新${unread ? `（新增 ${unread} 条）` : ''}`
    })
  }

  const button = $('open-manuscript')
  button.onclick = () => setOpen(root.hidden)

  return {
    open: () => setOpen(true),
    close: () => setOpen(false),
    isOpen: () => !root.hidden,
    handleMsg(message: ServerMsg): boolean {
      if (message.type === 'manuscript_page') {
        accept(message.page)
        return false
      }
      if (message.type === 'manuscript_append') {
        merge(message.entries)
        if (!root.hidden) paint()
        return false
      }
      if (message.type === 'manuscript_summary') {
        // 手动整理的立即回执先清按钮；结果要等同一个 request_id 回来。自动整理不带编号。
        if (message.request_id && message.request_id === summaryRequest) {
          summaryRequest = ''
          if (message.error) summaryError = message.error
          else if (message.generated) summaryError = ''
        }
        if (message.generated && message.summary) overview = message.summary
        if (!root.hidden) paint()
        return false
      }
      return false
    },
    connected() {
      if (state === 'connected') return
      state = 'connected'
      online = true
      // 断开前已有内容就按最后编号补齐；连上时正好在重置之后，从头读新局。
      loadPage(entries.length ? { after: cursor } : {})
      note('')
    },
    disconnected() {
      state = 'disconnected'
      online = false
      loading = false
      if (waiting) {
        clearTimeout(waiting.timer)
        waiting.reject(new Error('与小镇的连接断开了'))
        waiting = undefined
      }
      status.hidden = true
    },
    onReset() {
      state = 'reset'
      if (waiting) {
        clearTimeout(waiting.timer)
        waiting.reject(new Error('世界已经重置'))
        waiting = undefined
      }
      entries = []
      seen.clear()
      cursor = 0
      world = ''
      unread = 0
      following = true
      older = true
      overview = null
      summaryRequest = ''
      summaryError = ''
      paint()
      if (online) loadPage({})
    },
    dispose() {
      root.remove()
    },
  }
}

export type ManuscriptPanel = ReturnType<typeof createManuscriptPanel>
