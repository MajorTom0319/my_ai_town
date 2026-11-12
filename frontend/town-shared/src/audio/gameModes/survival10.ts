import type { GameModeMusic } from '.'
// 沿用已有音乐，不引入外部版权音源。
export const survival10Music: GameModeMusic<'survival10'> = {
  order: ['story/finale', 'tide/final'],
  quietDeaths: true,
  cell(view) {
    if (view.phase === 'over') return 'story/finale'
    if (view.phase === 'vote' || view.phase === 'jury') return 'tide/final'
    return null
  },
  sting(view, before) {
    if (view.phase === 'speech' && before.phase !== 'speech') return 'tide-announce'
    if (view.revealed && !before.revealed) return 'tide-rise'
    return null
  },
}
