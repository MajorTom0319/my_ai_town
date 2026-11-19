// 管理服务（crates/manager）带着真的世界进程：小镇的保存、打开、互相隔离、重启后接着来、删除，和只开一个小镇的规矩。
// 建镇要求、预览改名、建镇说明的校验在 crates/manager/tests/http.rs，不在这里重复。
import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import http from 'node:http'
import { seal } from '../mapgen/document.mjs'
import {
  createTown,
  draftTown,
  managerBinaryAvailable,
  root as source,
  startManager,
  worldQuery,
} from './support.mjs'
const BUILTIN_TOWNS = ['default-town', 'spring-valley', 'broken-towers']
const spec = {
  name: '隔离地图',
  seed: 51,
  style_id: 'svg-town-v1',
  places: [
    { kind: 'home', count: 2, size: 'medium' },
    { kind: 'library', count: 1, size: 'medium' },
    { kind: 'plaza', count: 1, size: 'medium' },
  ],
  constraints: [],
  resources: 'normal-life',
}
const alive = (pid) => {
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}
test('saved towns open in isolated worlds, come back after a restart, and are deleted only with their world closed', async (t) => {
  if (!managerBinaryAvailable()) return t.skip('需要先构建 target/debug/town-manager 与 host')
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'town-manager-test-'))
  let m
  try {
    const retired = path.join(root, 'towns/old-3d-town')
    await fs.mkdir(retired, { recursive: true })
    const oldMap = JSON.stringify(
      seal({
        ...JSON.parse(await fs.readFile(path.join(source, 'data/maps/2d-town/map.json'))),
        renderer: 'authored-island-v1',
        views: { available: ['3d'], default: '3d' },
      })
    )
    const oldSave = '{"legacy":"preserve resident memories"}'
    await fs.writeFile(path.join(retired, 'map.json'), oldMap)
    await fs.writeFile(path.join(retired, 'world.json'), oldSave)
    // 访达复制文件夹的默认名：编号不合规的目录不进列表，也不挡住别的小镇
    await fs.mkdir(path.join(root, 'towns/default-town 2'))
    // 这一段同时开两个小镇，验证两个世界互相隔离；产品里同时只运行一个
    m = await startManager(root, { maxRunning: 2 })
    const { api } = m
    // 随应用发的内置小镇第一次运行就都在，排在最前；旧 3D 存档不进列表
    assert.deepEqual(
      (await api('')).towns.map((t) => [t.id, t.deletable]),
      BUILTIN_TOWNS.map((id) => [id, false])
    )
    assert.deepEqual((await api('')).towns[0].views, ['2d'])
    for (const [route, body] of [
      ['/old-3d-town/map', undefined],
      ['/old-3d-town/open', {}],
      ['/old-3d-town/delete', {}],
    ])
      await assert.rejects(api(route, body), /3D 版本/)
    assert((await api('')).towns.every((town) => town.running === false))

    // 保存的就是页面生成、交上来的那一版；同一次保存同时到两遍只建一个小镇
    const { document, draft } = await draftTown(m, spec)
    const body = {
      draft_id: draft.id,
      expected_version: draft.version,
      request_id: 'save-exact-preview',
      creation: { description: '海边的小渔村', worldview: '这里的人想靠海吃饭。', residents: [] },
    }
    const [saved, repeated] = await Promise.all([api('/save', body), api('/save', body)])
    assert.equal(saved.id, repeated.id)
    assert.equal(saved.mapHash, document.content_hash)
    const [a, aa] = await Promise.all([api(`/${saved.id}/open`, {}), api(`/${saved.id}/open`, {})])
    assert.equal(a.endpoint, aa.endpoint)
    assert.equal(new URL(a.endpoint).host, new URL(base).host)
    assert.equal(new URL(a.endpoint).pathname, `/api/towns/${saved.id}/socket`)
    // 一句话建镇的说明和小镇存在一起：列表简介是玩家那句话，进镇时带着世界观建议和适合住进来的人
    assert.equal(a.creation.worldview, '这里的人想靠海吃饭。')
    assert.equal((await api('')).towns.find((town) => town.id === a.id).running, true)
    const b = await api('/default-town/open', {})
    assert.notEqual(a.worldId, b.worldId)
    assert.notEqual(a.endpoint, b.endpoint)
    const query = (town, command, type) =>
      worldQuery(town.endpoint, command, (msg) => msg.type === type)
    const wa = (await query(a, { type: 'get_world_settings' }, 'world_settings')).state
    assert.equal(wa.ready, false)
    const ack = await query(
      a,
      {
        type: 'set_worldview',
        request_id: 'setup',
        world_id: wa.world_id,
        expected_revision: wa.revision,
        text: '隔离生成的小镇，村民自由生活。',
      },
      'world_settings_saved'
    )
    assert.equal(ack.ok, true)
    assert.equal(
      (await query(b, { type: 'get_world_settings' }, 'world_settings')).state.ready,
      false
    )
    const cross = await query(
      b,
      {
        type: 'set_worldview',
        request_id: 'wrong',
        world_id: wa.world_id,
        expected_revision: 0,
        text: '跨世界写入',
      },
      'world_settings_saved'
    )
    assert.equal(cross.ok, false)
    const preview = (
      await query(a, { type: 'preview_resident_join', template_id: 'builtin_ashu' }, 'join_preview')
    ).preview
    const joined = await query(
      a,
      {
        type: 'join_resident',
        op_id: 'join-a',
        world_id: a.worldId,
        template_id: 'builtin_ashu',
        template_version: preview.template_version,
        name: preview.name,
        home: preview.home,
      },
      'resident_joined'
    )
    assert.equal(joined.outcome.ok, true)
    const id = joined.outcome.resident_id
    const population = (await query(a, { type: 'get_population' }, 'population')).population
    assert.equal(population.residents.length, 1)
    assert.equal(
      (await query(b, { type: 'get_population' }, 'population')).population.residents.length,
      0
    )
    // Dynamic books are real persisted world content, with independent durable display slots.
    const created = await query(
      a,
      {
        type: 'content',
        request_id: 'new-book',
        world_id: a.worldId,
        command: {
          operation: 'create',
          id: 'user:map-book',
          locale: 'library_1',
          title: '地图里的新书',
          author: '测试作者',
          text: '这本书只属于这个运行世界。',
        },
      },
      'prop_result'
    )
    assert.equal(created.ok, true)
    const instances = (await query(a, { type: 'get_props' }, 'props')).catalog.instances.map(
      (p) => ({ id: p.id, locale: p.locale })
    )
    const positions = await api(`/${a.id}/positions`, {
      map_hash: a.mapHash,
      instances,
    })
    const reordered = await api(`/${a.id}/positions`, {
      map_hash: a.mapHash,
      instances: [...instances].reverse(),
    })
    assert.deepEqual(
      positions.positions.find((p) => p.id === 'user:map-book'),
      reordered.positions.find((p) => p.id === 'user:map-book')
    )
    const filesBefore = await fs.readFile(path.join(root, 'towns', a.id, 'map.json'), 'utf8')
    // 管理服务退出前让两个世界都保存退出，没有一个超时被强制结束
    assert.equal(await m.stop(), 0, m.output())
    m = await startManager(root)
    assert.equal((await m.api('')).towns.length, BUILTIN_TOWNS.length + 1)
    let restored = await m.api(`/${a.id}/open`, {})
    assert.equal(restored.worldId, a.worldId)
    assert.equal(
      (await query(restored, { type: 'get_population' }, 'population')).population.residents[0].id,
      id
    )
    assert(
      (await query(restored, { type: 'get_props' }, 'props')).catalog.instances.some(
        (p) => p.id === 'user:map-book'
      )
    )
    await m.api(`/${a.id}/close`, {})
    const closed = (await m.api('')).towns.find((town) => town.id === a.id)
    assert.equal(closed.running, false)
    // 列表带着存档停在哪：选镇页据此画时钟牌、头像和那一刻的天色
    assert.equal(closed.progress.residents, 1)
    assert.equal(closed.progress.cast[0].template_id, 'builtin_ashu')
    assert.equal(closed.progress.minutes, Math.floor(closed.progress.minutes))
    assert.equal(typeof closed.progress.weather, 'string')
    const reopened = await m.api(`/${a.id}/open`, {})
    assert.equal(reopened.worldId, a.worldId)
    assert.notEqual(reopened.endpoint, restored.endpoint)
    restored = reopened
    await query(restored, { type: 'reset_world', request_id: 'reset-world' }, 'world_reset')
    const afterReset = (await query(restored, { type: 'get_world_settings' }, 'world_settings'))
      .state
    assert.notEqual(afterReset.world_id, a.worldId)
    assert.equal(afterReset.ready, false)
    assert.equal(
      (await query(restored, { type: 'get_population' }, 'population')).population.residents.length,
      0
    )
    assert.equal(await fs.readFile(path.join(root, 'towns', a.id, 'map.json'), 'utf8'), filesBefore)
    await assert.rejects(m.api(`/${a.id}/map?hash=wrong`), /地图已变化/)
    await assert.rejects(
      worldQuery(
        restored.endpoint.replace(/session=.*/, 'session=incorrect'),
        null,
        (msg) => msg.type === 'world_identity',
        2000
      )
    )
    // 删除运行中的生成地图：先关掉它的世界再删目录；内置的删不掉，别的存档不变，重启后也不再出现
    const protectedFile = path.join(root, 'towns/default-town/map.json')
    const protectedBefore = await fs.readFile(protectedFile, 'utf8')
    await assert.rejects(m.api('/default-town/delete', {}), /内置地图/)
    const { pid } = JSON.parse(
      await fs.readFile(path.join(root, 'towns', a.id, 'host-ready.json'), 'utf8')
    )
    assert(alive(pid))
    await m.api(`/${a.id}/delete`, {})
    assert(!alive(pid), '删除前要先关掉这个小镇的世界')
    assert.equal(
      (await m.api('')).towns.some((town) => town.id === a.id),
      false
    )
    await assert.rejects(fs.access(path.join(root, 'towns', a.id)), { code: 'ENOENT' })
    await assert.rejects(m.api(`/${a.id}/open`, {}), /不存在/)
    assert.equal(await fs.readFile(protectedFile, 'utf8'), protectedBefore)
    assert.equal(await m.stop(), 0, m.output())
    m = await startManager(root)
    assert.equal((await m.api('')).towns.length, BUILTIN_TOWNS.length)
    assert.equal(await fs.readFile(path.join(retired, 'map.json'), 'utf8'), oldMap)
    assert.equal(await fs.readFile(path.join(retired, 'world.json'), 'utf8'), oldSave)
  } finally {
    await m?.stop()
    await fs.rm(root, { recursive: true, force: true })
  }
})

test('LAN sharing is opt-in and checks the authority and origin for both API and world upgrades', async (t) => {
  if (!managerBinaryAvailable()) return t.skip('需要先构建 target/debug/town-manager 与 host')
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'town-lan-test-'))
  const hostname =
    Object.values(os.networkInterfaces())
      .flat()
      .find((address) => address.family === 'IPv4' && !address.internal)?.address ?? 'localhost'
  let manager
  try {
    manager = await startManager(root)
    const local = manager.origin
    const forbidden = await new Promise((resolve, reject) => {
      http
        .get(local + '/api/towns/health', { headers: { host: '192.0.2.10' } }, (response) => {
          response.resume()
          resolve(response.statusCode)
        })
        .once('error', reject)
    })
    assert.equal(forbidden, 403)
    assert.equal(await manager.stop(), 0)
    manager = await startManager(root, { lanHosts: [hostname] })
    const base = `http://${hostname}:${new URL(manager.origin).port}`
    assert.equal((await fetch(base + '/')).status, 200)
    assert.equal(
      (
        await fetch(base + '/api/towns/health', {
          headers: { origin: 'http://untrusted.example' },
        })
      ).status,
      403
    )
    const opened = await fetch(base + '/api/towns/default-town/open', {
      method: 'POST',
      headers: { origin: base, 'content-type': 'application/json', 'x-town-client': '1' },
      body: '{}',
    })
    assert.equal(opened.status, 200)
    const town = await opened.json()
    assert.equal(new URL(town.endpoint).host, new URL(base).host)
    const identity = await worldQuery(
      town.endpoint,
      null,
      (msg) => msg.type === 'world_identity'
    )
    assert.equal(identity.town_id, 'default-town')
  } finally {
    await manager?.stop()
    await fs.rm(root, { recursive: true, force: true })
  }
})
