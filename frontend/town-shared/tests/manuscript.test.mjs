// 实时剧本的阅读规则：仅公开会藏起心声和私聊；心声写成本人自述；导出保留时钟、未公开标记和来源编号。
import test from 'node:test'
import assert from 'node:assert/strict'
import { matches, scenes, summarySections, toMarkdown } from '../src/manuscript/format.ts'

const entry = (patch) => ({
  seq: 1,
  world_id: 'w1',
  sim_minutes: 600,
  clock: 'D1 10:00',
  kind: 'speech',
  visibility: 'public',
  tone: 'statement',
  text: '原话',
  source: 'chat:c1:1',
  ...patch,
})

const entries = [
  entry({ seq: 1, actor: 'a', name: '阿树', text: '明天会下雨。' }),
  entry({
    seq: 2,
    kind: 'thought',
    visibility: 'private',
    tone: 'thought',
    actor: 'b',
    name: '小禾',
    text: '先不答应。',
    source: 'thought:b:601',
  }),
  entry({
    seq: 3,
    kind: 'action',
    visibility: 'private',
    tone: 'result',
    actor: 'b',
    name: '小禾',
    text: '偷粮：被守家者拦住，粮食没有转移',
    clock: 'D1 13:05',
    source: 'action:g1/0:4',
  }),
  entry({
    seq: 4,
    kind: 'action',
    visibility: 'private',
    tone: 'result',
    actor: 'b',
    name: '小禾',
    text: '前往：门锁着，进不去',
    clock: 'D1 13:20',
    source: 'action:g2/0:5',
  }),
]

test('实时剧本只把公开条目给公开视图，心声标成自述，失败原样保留', () => {
  const hidden = entries.filter((e) => !matches(e, { view: 'public' }))
  assert.deepEqual(
    hidden.map((e) => e.seq),
    [2, 3, 4]
  )
  assert.equal(matches(entries[1], { view: 'all', noThought: true }), false)
  assert.equal(matches(entries[0], { view: 'all', day: 'D1', actor: 'a' }), true)
  const acts = scenes(entries).map((scene) => scene.title)
  assert.deepEqual(acts, ['D1 上午', 'D1 午后'])

  const md = toMarkdown(entries, { view: 'all' })
  assert.match(md, /小禾〔心声·自述〕〔未公开〕：先不答应。/)
  assert.match(md, /偷粮：被守家者拦住，粮食没有转移/)
  assert.match(md, /`action:g1\/0:4`/)
  assert.doesNotMatch(md, /已做到/)
  const open = toMarkdown(entries, { view: 'public' })
  assert.match(open, /阿树〔发言〕：“明天会下雨。”/)
  assert.doesNotMatch(open, /小禾/)
})

test('标成公开或误写前缀的秘密仍被「仅公开」挡住，全知视图保留', () => {
  const secret = entry({
    seq: 9,
    kind: 'announcement',
    tone: 'announcement',
    visibility: 'public',
    text: '裁判全知：住宅受灾 2/7 户，这些私人损失未向其他参赛者公开。',
  })
  assert.equal(matches(secret, { view: 'public' }), false)
  assert.equal(matches(secret, { view: 'all' }), true)
})

test('概览里引用私密条目的判断不会出现在公开视图', () => {
  const summary = {
    through_seq: 4,
    through_clock: 'D1 13:20',
    situation: '小禾没有说出自己的打算。',
    conflicts: [
      ['阿树与众人讨论明天的天气', '1'],
      ['小禾心里另有打算', '2'],
    ],
    promises: [['小禾答应过什么', '7']],
    questions: [],
  }
  const open = summarySections(summary, entries, { view: 'public' })
  assert.deepEqual(open, [
    { title: '主要冲突', lines: [{ text: '阿树与众人讨论明天的天气', sources: [1] }] },
  ])
  assert.deepEqual(
    summarySections(summary, entries, { view: 'all' })[0].lines.map((line) => line.sources),
    [[1], [2]]
  )
})
