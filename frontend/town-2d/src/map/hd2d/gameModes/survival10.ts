import type * as THREE from 'three'
import type { GameModeView, Survival10View } from 'town-shared/protocol'
import type { GameModeLook } from '.'
import './survival10.css'

const WORK_REQUIRED = 480

// 只显示服务器同步来的模式事实；不在前端重新模拟生产、偷米或投票。
const LABEL: Record<Survival10View['phase'], string> = {
  waiting: '等待开局',
  day: '白天行动',
  speech: '篝火发言',
  vote: '记名投票',
  night: '夜间行动',
  jury: '陪审团',
  over: '终局',
}

export class Survival10Look implements GameModeLook {
  readonly waterAnywhere = false
  private root: HTMLElement | null = null
  private state: Survival10View | null = null

  shadeGround(_material: THREE.Material, _surface: 'land' | 'sea') {}
  shadeStanding(_material: THREE.Material, _who: 'thing' | 'villager') {}

  mount(layer: HTMLElement) {
    this.root = document.createElement('section')
    this.root.className = 's10-hud'
    this.root.hidden = true
    layer.append(this.root)
  }

  apply(view: GameModeView | null) {
    this.state = view?.kind === 'survival10' ? view : null
    this.render()
  }

  private render() {
    const root = this.root
    if (!root) return
    const v = this.state
    root.hidden = !v
    root.replaceChildren()
    if (!v) return

    const banner = document.createElement('div')
    banner.className = 's10-banner'
    const title = document.createElement('strong')
    title.textContent = `聪明人小镇 · 第 ${v.day}/10 天 · ${LABEL[v.phase]}`
    const notice = document.createElement('span')
    notice.textContent = v.notice
    banner.append(title, notice)

    const board = document.createElement('div')
    board.className = 's10-board'
    const heading = document.createElement('header')
    const alive = v.seats.filter((s) => !s.out).length
    heading.textContent = `公仓 ${v.public_rice} 袋  ·  今日报名 ${v.registered_today ?? 0}/4  ·  今日产粮 ${v.produced_today}/4  ·  在场 ${alive}/7`
    board.append(heading)
    for (const s of v.seats) {
      const item = document.createElement('div')
      item.className = 's10-person' + (s.out ? ' s10-out' : '')
      const name = document.createElement('b')
      name.textContent = `${s.seat}. ${s.name}`
      const count = document.createElement('small')
      count.textContent = s.out
        ? `出局：${s.out}`
        : `私粮 ${s.rice} 袋 · 连饿 ${s.hungry_streak} 晚`
      item.append(name, count)
      if (v.phase === 'day' && s.farm && !s.out) {
        const progress = document.createElement('small')
        progress.className = 's10-work' + (s.working_now ? ' s10-working' : '')
        const minutes = Math.min(Math.round(s.labor_progress ?? s.worked_minutes), WORK_REQUIRED)
        const percent = Math.round((minutes / WORK_REQUIRED) * 100)
        const state = s.working_now
          ? '农田劳作中'
          : minutes >= WORK_REQUIRED
            ? '已满八小时'
            : minutes > 0
              ? '已暂停'
              : '已报名，尚未下地'
        progress.textContent = `劳动 ${minutes}/${WORK_REQUIRED} 分钟 · ${state}`
        progress.title = `真实有效劳动 ${minutes} 分钟（${percent}%）`
        item.append(progress)
      }
      board.append(item)
    }
    const details = document.createElement('div')
    details.className = 's10-details'
    if (v.turn) {
      const current = document.createElement('div')
      current.textContent = `正在发言：${v.seats.find((s) => s.seat === v.turn)?.name ?? v.turn}`
      details.append(current)
    }
    if (v.phase === 'vote' || v.phase === 'jury') {
      const votes = document.createElement('div')
      votes.textContent = `已交选票 ${v.ballots.length} 张（统一揭票）`
      details.append(votes)
    }
    const latest = v.lines.at(-1)
    if (latest) {
      const say = document.createElement('blockquote')
      say.textContent = `${v.seats.find((s) => s.seat === latest.seat)?.name ?? latest.seat}：${latest.text}`
      details.append(say)
    }
    if (v.winner) {
      const winner = document.createElement('strong')
      winner.textContent = `🏆 冠军：${v.seats.find((s) => s.seat === v.winner)?.name ?? v.winner}`
      details.append(winner)
    }
    root.append(banner, board, details)
  }
}
