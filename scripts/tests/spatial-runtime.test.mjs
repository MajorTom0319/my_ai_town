import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { registerHooks } from 'node:module'
import { createManager } from '../town-manager.mjs'
import { worldQuery } from '../towns/hosts.mjs'
import { APPEARANCE_SET } from '../mapgen/budget.mjs'
import { position } from '../../frontend/town-shared/src/spatial.ts'

test('survival10 personas, worldview and furnishings survive opening, reload and reset', async () => {
  const hooks = registerHooks({
    resolve(specifier, context, nextResolve) {
      if (context.parentURL?.includes('/src/prefabs/') && specifier.startsWith('.'))
        return nextResolve(
          new URL(specifier === '.' ? './index.ts' : `${specifier}.ts`, context.parentURL).href,
          context
        )
      return nextResolve(specifier, context)
    },
  })
  let prefab
  try {
    prefab = (await import('../../frontend/town-shared/src/prefabs/index.ts')).prefabOf(
      'survival10'
    )
  } finally {
    hooks.deregister()
  }
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'town-survival10-setup-'))
  let manager
  try {
    const started = await startTown(root, prefab.spec.places, prefab.spec)
    manager = started.m
    let town = started.town
    const query = (command, type) => worldQuery(town.endpoint, command, (msg) => msg.type === type)
    for (const reset of [false, true]) {
      if (reset) await query({ type: 'reset_world', request_id: 'reset' }, 'world_reset')
      const world = (await query({ type: 'get_world_settings' }, 'world_settings')).state
      if (world.worldview !== prefab.worldview) {
        const reply = await query(
          {
            type: 'set_worldview',
            request_id: 'setup',
            world_id: world.world_id,
            expected_revision: world.revision,
            text: prefab.worldview,
          },
          'world_settings_saved'
        )
        assert.ok(reply.ok, reply.message)
      }
      for (const id of prefab.furnish.remove) {
        const reply = await query(
          { type: 'remove_prop', request_id: id, world_id: world.world_id, id },
          'prop_result'
        )
        assert.ok(reply.ok, reply.message)
      }
      for (const instance of prefab.furnish.place) {
        const reply = await query(
          { type: 'place_prop', request_id: instance.id, world_id: world.world_id, instance },
          'prop_result'
        )
        assert.ok(reply.ok, reply.message)
      }
      for (const supply of prefab.furnish.giveItem) {
        const reply = await query(
          {
            type: 'set_prop_supply',
            request_id: 'supply',
            world_id: world.world_id,
            prop: supply.id,
            action: supply.action,
            remaining: supply.remaining,
          },
          'prop_result'
        )
        assert.ok(reply.ok, reply.message)
      }
      for (const [index, persona] of prefab.cast.entries()) {
        const template = prefab.roster[index]
        const preview = (
          await query(
            { type: 'preview_resident_join', template_id: template, name: persona.name },
            'join_preview'
          )
        ).preview
        assert.ok(preview.ok, preview.problems?.join('；'))
        const joined = await query(
          {
            type: 'join_resident',
            op_id: `cast-${reset}-${index}`,
            world_id: world.world_id,
            template_id: template,
            template_version: preview.template_version,
            name: preview.name,
            home: preview.home,
            persona,
          },
          'resident_joined'
        )
        assert.ok(joined.outcome.ok, joined.outcome.message)
      }
      const mode = await query(
        {
          type: 'set_game_mode',
          request_id: 'mode',
          world_id: world.world_id,
          game_mode: prefab.gameMode,
        },
        'world_settings_saved'
      )
      assert.ok(mode.ok, mode.message)
      await manager.hosts.close(town.id)
      const saved = JSON.parse(
        await fs.readFile(path.join(root, 'towns', town.id, 'world.json'), 'utf8')
      )
      assert.equal(saved.world_settings.worldview, prefab.worldview)
      assert.equal(Object.keys(saved.population.residents).length, 7)
      for (const resident of Object.values(saved.population.residents)) {
        const persona = prefab.cast.find((candidate) => candidate.name === resident.name)
        assert.ok(persona, resident.name)
        assert.ok(resident.persona.includes(persona.personality))
        assert.ok(resident.persona.includes(persona.drives))
        assert.ok(persona.drives.startsWith('最高目标是自己保持参赛资格，活到第十天终局'))
        const session = saved.resident_agent.runtime.sessions[resident.id]
        assert.equal(session.instructions, `${resident.persona}\n${prefab.worldview}`)
        assert.equal(session.daily.calls, 0)
      }
      town = await manager.hosts.open(town.id)
      const population = (await query({ type: 'get_population' }, 'population')).population
      for (const resident of population.residents) {
        const config = await query(
          { type: 'get_agent_config', resident: resident.id },
          'agent_config'
        )
        assert.equal(config.persona, saved.population.residents[resident.id].persona)
      }
      const catalog = (await query({ type: 'get_props' }, 'props')).catalog
      assert.equal(catalog.instances.filter((prop) => prop.name === '单人床').length, 7)
      assert.ok(!catalog.instances.some((prop) => ['干粮袋', '饮用水桶'].includes(prop.name)))
      const pantries = catalog.instances.filter((prop) => prop.name === '私粮袋')
      assert.equal(pantries.length, 7)
      assert.ok(
        pantries.every(
          (prop) =>
            prop.storage_slots === 0 && prop.actions.every((action) => action.remaining === 0)
        )
      )
      for (const instance of prefab.furnish.place)
        assert.ok(
          catalog.instances.some((placed) => placed.id === instance.id),
          instance.id
        )
    }
  } finally {
    await manager?.stop()
    await fs.rm(root, { recursive: true, force: true })
  }
})

test('residents at runtime prop stations remain visible and possessable after removal and reload', async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'town-runtime-station-'))
  let m
  try {
    const started = await startTown(root, [{ kind: 'home', count: 1, size: 'large' }])
    m = started.m
    let town = started.town
    const query = (command, type) => worldQuery(town.endpoint, command, (v) => v.type === type)
    const prop = 'user:station-can'
    assert.ok(
      (
        await query(
          {
            type: 'place_prop',
            request_id: 'can',
            world_id: town.worldId,
            instance: { id: prop, definition: 'core:island-can@1', locale: 'home_1' },
          },
          'prop_result'
        )
      ).ok
    )
    const preview = (
      await query({ type: 'preview_resident_join', template_id: 'builtin_ashu' }, 'join_preview')
    ).preview
    const joined = await query(
      {
        type: 'join_resident',
        op_id: 'join',
        world_id: town.worldId,
        template_id: 'builtin_ashu',
        template_version: preview.template_version,
        name: preview.name,
        home: preview.home,
      },
      'resident_joined'
    )
    assert.ok(joined.outcome.ok)
    const resident = joined.outcome.resident_id
    const file = path.join(root, 'towns', town.id, 'world.json')
    const map = JSON.parse(await fs.readFile(started.map, 'utf8'))
    await m.hosts.close(town.id)
    // 复现旧存档里 NPC 已走到后来摆放的道具旁的合法站位；测试全程暂停，模型不安排动作。
    const saved = JSON.parse(await fs.readFile(file, 'utf8'))
    saved.world.actors[resident].locus = {
      locale: 'home_1',
      at: { kind: 'station', station: `use_${prop}` },
    }
    await fs.writeFile(file, JSON.stringify(saved))
    for (const removed of [false, true]) {
      town = await m.hosts.open(town.id)
      if (removed) {
        assert.ok(
          (
            await query(
              { type: 'remove_prop', request_id: 'remove', world_id: town.worldId, id: prop },
              'prop_result'
            )
          ).ok
        )
        // 墓碑和历史站位经保存、重启后仍必须可用。
        await m.hosts.close(town.id)
        town = await m.hosts.open(town.id)
      }
      const catalog = (await query({ type: 'get_props' }, 'props')).catalog
      const override = catalog.spatial.overrides.find((v) => v.prop.id === prop)
      assert.equal(
        catalog.instances.some((p) => p.id === prop),
        !removed
      )
      const locus = saved.world.actors[resident].locus
      assert.deepEqual(
        position(map.spatial, locus.locale, locus.at, catalog.spatial),
        override.prop.anchor
      )
      assert.deepEqual(
        position(
          map.spatial,
          override.route.id,
          { kind: 'transit', elapsed: override.route.minutes, total: override.route.minutes },
          catalog.spatial
        ),
        override.prop.anchor
      )
      const result = await query(
        {
          type: 'avatar',
          request_id: 'possess',
          world_id: town.worldId,
          epoch: '',
          sequence: 1,
          command: { kind: 'possess', resident },
        },
        'avatar_result'
      )
      assert.ok(result.ok, result.reason)
      assert.equal(result.state.actor.id, resident)
      assert.deepEqual(result.state.actor.locus.at.position, [
        override.prop.anchor[0],
        override.prop.anchor[2],
      ])
      await m.hosts.close(town.id)
      const after = JSON.parse(await fs.readFile(file, 'utf8'))
      assert.equal(after.world.minutes, saved.world.minutes, '附身与读档不能推进暂停中的世界')
      // 下一轮仍从历史站位出发，不能用第一次附身转换出的自由坐标掩盖问题。
      after.world.actors[resident].locus = locus
      await fs.writeFile(file, JSON.stringify(after))
    }
  } finally {
    await m?.stop()
    await fs.rm(root, { recursive: true, force: true })
  }
})

test('one world commits dynamic book geometry and removals together across restart', async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'town-space-runtime-'))
  let m
  try {
    const started = await startTown(root, [
      { kind: 'library', count: 1, size: 'large' },
      { kind: 'home', count: 2, size: 'small' },
    ])
    m = started.m
    let town = started.town
    const query = (command, type) => worldQuery(town.endpoint, command, (m) => m.type === type)
    const content = (id, locale) =>
      query(
        {
          type: 'content',
          request_id: id,
          world_id: town.worldId,
          command: {
            operation: 'create',
            id,
            locale,
            title: '空间中的书',
            author: '验收',
            text: '这段文字由读者自行决定记住什么。',
          },
        },
        'prop_result'
      )
    const created = await content('user:spatial-book', 'library_1')
    assert.ok(created.ok, JSON.stringify(created))
    let catalog = (await query({ type: 'get_props' }, 'props')).catalog
    const spatial = catalog.spatial,
      placed = spatial.overrides.find((v) => v.prop.id === 'user:spatial-book')
    assert.ok(placed)
    assert.ok(catalog.instances.some((p) => p.id === 'user:spatial-book'))
    assert.equal(placed.route.to, 'node_user:spatial-book')
    assert.ok(placed.route.length > 0)
    assert.equal((await content('user:invalid', 'missing-place')).ok, false)
    catalog = (await query({ type: 'get_props' }, 'props')).catalog
    assert.equal(catalog.spatial.overrides.length, 1)
    assert.ok(!catalog.instances.some((p) => p.id === 'user:invalid'))
    await m.hosts.close(town.id)
    town = await m.hosts.open(town.id)
    assert.deepEqual((await query({ type: 'get_props' }, 'props')).catalog.spatial, spatial)
    const removed = await query(
      {
        type: 'remove_prop',
        request_id: 'remove',
        world_id: town.worldId,
        id: 'user:spatial-book',
      },
      'prop_result'
    )
    assert.ok(removed.ok, JSON.stringify(removed))
    const after = (await query({ type: 'get_props' }, 'props')).catalog
    assert.ok(!after.instances.some((p) => p.id === 'user:spatial-book'))
    assert.ok(after.spatial.removed.includes('user:spatial-book'))
    assert.ok(after.spatial.revision > spatial.revision)
    await m.hosts.close(town.id)
    town = await m.hosts.open(town.id)
    assert.deepEqual((await query({ type: 'get_props' }, 'props')).catalog.spatial, after.spatial)
    assert.ok(
      !(await query({ type: 'get_props' }, 'props')).catalog.instances.some(
        (p) => p.id === 'user:spatial-book'
      )
    )
    await query({ type: 'reset_world', request_id: 'reset-world' }, 'world_reset')
    const reset = (await query({ type: 'get_props' }, 'props')).catalog
    assert.deepEqual(reset.spatial.overrides, [])
    assert.deepEqual(reset.spatial.removed, [])
  } finally {
    await m?.stop()
    await fs.rm(root, { recursive: true, force: true })
  }
})

/** 起管理服务，用生成器建一座海边小镇并打开，设好世界观 */
async function startTown(root, places, extra = {}) {
  const m = await createManager({ root, port: 0, mockLlm: true })
  const api = async (route, body) => {
    const r = await fetch(`http://127.0.0.1:${m.server.address().port}/api/towns${route}`, {
      method: body === undefined ? 'GET' : 'POST',
      headers: { 'content-type': 'application/json', 'x-town-client': '1' },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
    const v = await r.json()
    assert.ok(r.ok, JSON.stringify(v))
    return v
  }
  const spec = {
    schema_version: 2,
    name: '空间持久化',
    seed: 42,
    appearance_set: APPEARANCE_SET,
    environment: { preset: 'coastal-village', vegetation: 'medium' },
    places,
    resources: 'normal-life',
    constraints: [],
    ...extra,
  }
  const draft = await api('/preview', { spec }),
    saved = await api('/save', {
      draft_id: draft.id,
      expected_version: draft.version,
      request_id: 'space-save',
    })
  const town = await api(`/${saved.id}/open`, {})
  const query = (command, type) => worldQuery(town.endpoint, command, (m) => m.type === type)
  const state = (await query({ type: 'get_world_settings' }, 'world_settings')).state
  assert.ok(
    (
      await query(
        {
          type: 'set_worldview',
          request_id: 'setup',
          world_id: state.world_id,
          expected_revision: state.revision,
          text: '一处供居民生活和阅读的海岸村落。',
        },
        'world_settings_saved'
      )
    ).ok
  )
  return { m, town, map: path.join(root, 'towns', saved.id, 'map.json') }
}

// 环岛南边的地点，入口开在朝岛心的北边：操作锚点一律放南边时，从入口走过去都要穿过道具本身，
// 放一两件就满了。锚点可以换到北边，同一处地点能接连放下多件。
test('a place entered from the north still takes several props', async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'town-space-north-'))
  let m
  try {
    const started = await startTown(root, [{ kind: 'area', count: 6, size: 'large' }], {
      settlement: 'ring',
    })
    m = started.m
    const { town } = started
    const query = (command, type) => worldQuery(town.endpoint, command, (m) => m.type === type)
    const map = JSON.parse(await fs.readFile(started.map, 'utf8'))
    const north = map.spatial.floors.find((f) => {
      const [, , z] = map.spatial.stations.find((s) => s.id === `at_${f.id}`).position
      return !f.indoor && z < f.bounds.y + f.bounds.h / 3
    })
    assert.ok(north, '环岛上总有入口在北边的地点')
    for (let i = 0; i < 6; i++) {
      const placed = await query(
        {
          type: 'place_prop',
          request_id: `north-${i}`,
          world_id: town.worldId,
          instance: { id: `user:north-${i}`, definition: 'core:island-can@1', locale: north.id },
        },
        'prop_result'
      )
      assert.ok(placed.ok, JSON.stringify(placed))
    }
    const overrides = Object.values(
      (await query({ type: 'get_props' }, 'props')).catalog.spatial.overrides
    )
    assert.ok(
      overrides.some((o) => o.prop.anchor[2] < o.prop.position[2]),
      '有的锚点在北边'
    )
  } finally {
    await m?.stop()
    await fs.rm(root, { recursive: true, force: true })
  }
})

test('placing a prop where the player clicked puts it beside that spot', async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'town-space-near-'))
  let m
  try {
    const started = await startTown(root, [{ kind: 'plaza', count: 1, size: 'large' }])
    m = started.m
    const { town } = started
    const query = (command, type) => worldQuery(town.endpoint, command, (m) => m.type === type)
    // 先点右头再点左头，两件各自更靠近自己点的那头。
    // 不看点的位置时放置器从区域一角按行扫，先放的在左、后放的在右，正好相反
    const map = JSON.parse(await fs.readFile(started.map, 'utf8'))
    const { bounds } = map.spatial.floors.find((f) => f.id === 'plaza_1')
    const ends = [
      [bounds.x + bounds.w - 1, bounds.y + bounds.h / 2],
      [bounds.x + 1, bounds.y + bounds.h / 2],
    ]
    for (const [i, near] of ends.entries()) {
      const placed = await query(
        {
          type: 'place_prop',
          request_id: `near-${i}`,
          world_id: town.worldId,
          instance: { id: `user:near-${i}`, definition: 'core:workbench@1', locale: 'plaza_1' },
          near: { space: 'town_outdoor', position: near },
        },
        'prop_result'
      )
      assert.ok(placed.ok, JSON.stringify(placed))
    }
    // 道具列表带上放好的位置，地图才画得出来
    const { instances } = (await query({ type: 'get_props' }, 'props')).catalog
    const away = (i, end) => {
      const [x, , z] = instances.find((p) => p.id === `user:near-${i}`).position
      return Math.hypot(x - ends[end][0], z - ends[end][1])
    }
    assert.ok(away(0, 0) < away(1, 0) && away(1, 1) < away(0, 1))
  } finally {
    await m?.stop()
    await fs.rm(root, { recursive: true, force: true })
  }
})
