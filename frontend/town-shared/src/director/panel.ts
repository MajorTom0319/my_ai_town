// 世界导演：玩家在地图上下指令，导演经内核执行——投放道具、发通知、预约事件、贴公告——村民再按
// 自己看到的世界做决定。面板只展示玩家的话、导演的回应和每件事的执行结果，以及待触发的时间线；
// 村民的目标、记忆、心声、私聊不在这里出现。样式与道具页的浮窗一致（props/page.ts）。
import './style.css'
import type { Receipt, SaveCommand } from '../connection'
import type {
  ClientMsg,
  DirectorEntry,
  DirectorPending,
  DirectorState,
  ServerMsg,
} from '../protocol'
import { $ } from '../dom'

const SAY_MAX = 600

/** 几个示例指令：点一下就填进输入框，玩家改一改再发 */
const EXAMPLES = [
  '在海边放一张长椅，让路过的人能歇脚',
  '半小时后发布一条通知：今晚码头来船，想去的人来集合',
  '在公告栏贴一张告示：本周六镇上办集市',
  '一小时后下一场雨，只让在室外的人知道',
]

/** 等回执的上限：超时就把这句话交还输入框，别让面板一直停在「发送中」。 */
const RECEIPT_TIMEOUT_MS = 8000

export function createDirectorPanel(deps: {
  send: (msg: ClientMsg) => void
  /** 发出指令并收回执；断线时以失败结束 */
  save: (command: SaveCommand, receipt: Receipt) => void
  /** 当前世界标识，取自最近一份道具目录 */
  worldId: () => string | undefined
}) {
  /** 发出后还没收到回执的那条；回执的 ok 决定这条发没发出，失败时把原话放回输入框 */
  let sending: { text: string } | null = null
  let receiptTimer = 0
  /** 连着小镇没有；断开时发送不丢原话，只提示稍后再发 */
  let online = false

  const root = document.createElement('section')
  root.id = 'director-panel'
  root.className = 'director-panel'
  root.hidden = true
  root.setAttribute('aria-label', '世界导演')

  const top = document.createElement('header')
  top.className = 'director-top'
  const title = document.createElement('h2')
  title.textContent = '世界导演'
  const clock = document.createElement('span')
  clock.className = 'director-clock'
  const close = document.createElement('button')
  close.type = 'button'
  close.className = 'director-close'
  close.textContent = '收起'
  close.onclick = () => setOpen(false)
  top.append(title, clock, close)

  const log = document.createElement('div')
  log.className = 'director-log'
  log.setAttribute('aria-live', 'polite')

  const side = document.createElement('aside')
  side.className = 'director-side'
  const sideTitle = document.createElement('h3')
  sideTitle.textContent = '待触发'
  const pending = document.createElement('ol')
  pending.className = 'director-pending'
  const tips = document.createElement('ul')
  tips.className = 'director-examples'
  for (const text of EXAMPLES) {
    const item = document.createElement('li')
    const pick = document.createElement('button')
    pick.type = 'button'
    pick.textContent = text
    pick.onclick = () => {
      input.value = text
      input.focus()
    }
    item.append(pick)
    tips.append(item)
  }
  const note = document.createElement('p')
  note.className = 'director-note'
  note.textContent =
    '导演操作经内核校验后执行。预约按模拟时间计时，暂停时不推进；到点执行并显示结果，失败不会当作成功。没有可用工具模型时请先在设定里选一个。'
  side.append(sideTitle, pending, note, tips)

  const form = document.createElement('form')
  form.className = 'director-form'
  const input = document.createElement('textarea')
  input.rows = 1
  input.maxLength = SAY_MAX
  input.placeholder = '对世界说点什么…（Ctrl+Enter 发送）'
  input.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) {
      event.preventDefault()
      form.requestSubmit()
    }
  })
  input.addEventListener('input', () => {
    input.style.height = 'auto'
    input.style.height = `${Math.min(140, input.scrollHeight)}px`
  })
  const send = document.createElement('button')
  send.type = 'submit'
  send.className = 'director-send'
  send.textContent = '发送'
  form.append(input, send)
  form.onsubmit = (event) => {
    event.preventDefault()
    submit()
  }

  const body = document.createElement('div')
  body.className = 'director-body'
  body.append(log, side)
  root.append(top, body, form)
  $('map').append(root)
  // 面板在地图容器里：按下、双击和滚轮不能漏给地图。地图在 pointerdown 上捕获指针，
  // 漏下去的话点击会被当成拖拽起点，面板里的按钮和输入框都点不到。
  for (const type of ['pointerdown', 'dblclick', 'wheel'] as const)
    root.addEventListener(type, (event) => event.stopPropagation())
  // Esc 关面板（先关的先用掉这次按键，菜单就不会跟着开）
  root.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape' || event.repeat || event.isComposing) return
    event.preventDefault()
    event.stopPropagation()
    setOpen(false)
  })

  const button = $('open-director')
  button.onclick = () => setOpen(root.hidden)

  let state: DirectorState | null = null
  let world: string | undefined
  let shown: unknown = null

  function setOpen(open: boolean) {
    root.hidden = !open
    button.setAttribute('aria-pressed', String(open))
    if (open) {
      deps.send({ type: 'get_director_state' })
      input.focus()
      scrollToEnd()
    }
  }

  function submit() {
    const text = input.value.trim()
    if (!text || sending) return
    const worldId = deps.worldId()
    if (!worldId || !online) {
      say('还没连上小镇；等右上角显示已连上再发，这句还留在输入框里', true)
      return
    }
    input.value = ''
    input.style.height = 'auto'
    sending = { text }
    send.disabled = true
    deps.save({ type: 'director_say', world_id: worldId, text }, (ok, message) => {
      clearTimeout(receiptTimer)
      if (sending?.text !== text) return
      sending = null
      send.disabled = !!state?.thinking
      if (ok) return
      restore()
      say(message, true)
    })
    clearTimeout(receiptTimer)
    receiptTimer = window.setTimeout(() => {
      if (sending?.text !== text) return
      sending = null
      send.disabled = !!state?.thinking
      restore()
      say('小镇迟迟没有回话，这句还留在输入框里；稍后再发一次', true)
    }, RECEIPT_TIMEOUT_MS)
  }

  /** 没发出去的那句放回输入框，等玩家改一改再发 */
  function restore() {
    if (!sending || input.value.trim()) return
    input.value = sending.text
    input.style.height = 'auto'
    input.style.height = `${Math.min(140, input.scrollHeight)}px`
  }

  function say(text: string, bad = false) {
    const line = document.createElement('p')
    line.className = bad ? 'director-status error' : 'director-status'
    line.textContent = text
    log.append(line)
    scrollToEnd()
  }

  function scrollToEnd() {
    log.scrollTop = log.scrollHeight
  }

  function render(next: DirectorState) {
    state = next
    clock.textContent = next.clock
    send.disabled = next.thinking || !!sending
    send.textContent = next.thinking ? '思考中…' : '发送'
    input.disabled = next.thinking
    renderLog(next.entries, next.thinking)
    renderPending(next.pending)
  }

  /** 对话：只重建发生变化的整条记录；条目不会改写，比较长度与最后一条即可 */
  function renderLog(entries: DirectorEntry[], thinking: boolean) {
    const key = JSON.stringify([entries, thinking])
    if (shown === key) return
    shown = key
    log.replaceChildren()
    if (!entries.length) {
      const empty = document.createElement('p')
      empty.className = 'director-empty'
      empty.textContent = '还没有指令。写一句，让小镇因它而动。'
      log.append(empty)
    }
    for (const entry of entries) log.append(entryNode(entry))
    if (thinking) {
      const wait = document.createElement('p')
      wait.className = 'director-thinking'
      wait.textContent = '导演正在读世界、准备执行…'
      log.append(wait)
    }
    scrollToEnd()
  }

  function entryNode(entry: DirectorEntry): HTMLElement {
    const box = document.createElement('article')
    box.className = 'director-entry'
    const ask = document.createElement('p')
    ask.className = 'director-ask'
    ask.textContent = entry.player
    const reply = document.createElement('p')
    reply.className = 'director-reply'
    reply.textContent = entry.reply || entry.error || '（这一轮没有回应）'
    if (entry.error && entry.reply) {
      reply.textContent = `${entry.reply}\n${entry.error}`
    }
    if (entry.error && !entry.reply) reply.classList.add('error')
    box.append(ask, reply)
    const effects = entry.effects.filter(
      (effect) =>
        !effect.ok ||
        ![
          'look',
          'survey',
          'watch',
          'inspect',
          'read_content',
          'trace',
          'history',
          'world',
        ].includes(effect.tool)
    )
    if (effects.length) {
      const list = document.createElement('ul')
      list.className = 'director-effects'
      for (const effect of effects) {
        const item = document.createElement('li')
        item.className = effect.ok ? 'ok' : 'bad'
        const tool = document.createElement('code')
        tool.textContent = effect.tool
        item.append(tool, document.createTextNode(` ${effect.text}`))
        list.append(item)
      }
      if (effects.every((effect) => effect.ok)) {
        const details = document.createElement('details')
        const summary = document.createElement('summary')
        summary.textContent = `执行结果（${effects.length} 项）`
        details.append(summary, list)
        box.append(details)
      } else {
        box.append(list)
      }
    }
    return box
  }

  function renderPending(list: DirectorPending[]) {
    pending.replaceChildren()
    if (!list.length) {
      const empty = document.createElement('li')
      empty.className = 'director-pending-empty'
      empty.textContent = '没有预约中的事'
      pending.append(empty)
      return
    }
    for (const item of list) {
      const row = document.createElement('li')
      const when = document.createElement('b')
      when.textContent = item.clock
      const where = document.createElement('span')
      where.className = 'director-pending-place'
      where.textContent = item.locale
      row.append(when, where, document.createTextNode(item.text))
      pending.append(row)
    }
  }

  return {
    open: () => setOpen(true),
    /** 道具目录换了世界：旧世界的对话不再算数，面板清空 */
    onWorld(id: string) {
      if (world === id) return
      world = id
      state = null
      shown = null
      log.replaceChildren()
      pending.replaceChildren()
      clock.textContent = ''
      if (!root.hidden) deps.send({ type: 'get_director_state' })
    },
    close: () => setOpen(false),
    isOpen: () => !root.hidden,
    handleMsg(message: ServerMsg): boolean {
      if (message.type === 'roster' && state) {
        clock.textContent = message.clock
        return false
      }
      if (message.type === 'director_state') {
        render(message.state)
        return false
      }
      if (message.type === 'director_reply') {
        deps.send({ type: 'get_director_state' })
        return false
      }
      return false
    },
    connected() {
      online = true
    },
    disconnected() {
      // 断线时连接层会把没回执的那条以失败交回来；这里只管不再收新的指令
      online = false
    },
    onReset() {
      state = null
      shown = null
      log.replaceChildren()
      pending.replaceChildren()
      clock.textContent = ''
    },
    dispose() {
      clearTimeout(receiptTimer)
      root.remove()
    },
  }
}

export type DirectorPanel = ReturnType<typeof createDirectorPanel>
