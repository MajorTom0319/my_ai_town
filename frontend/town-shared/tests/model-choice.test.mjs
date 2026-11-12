import test from 'node:test'
import assert from 'node:assert/strict'
import { PRESETS, modelOptions } from '../src/providers.ts'
import { providerSaveCommand } from '../src/settings/providerCommands.ts'

test('已连接的 MaaS 服务列出所有模型，选择同服务的其他模型时保存完整 ID 且不改密钥', () => {
  const preset = PRESETS.find((provider) => provider.id === 'maas-deepseek')
  const providers = [
    { id: preset.id, name: preset.name, base_url: preset.baseUrl, key_mask: '已保存' },
    { id: 'deepseek', name: 'DeepSeek', base_url: 'https://api.deepseek.com', key_mask: '' },
  ]
  const active = { provider: preset.id, model: 'maas-deepseek/private-model' }
  const options = modelOptions({ active, providers })
  assert.deepEqual(options[0], { ...active, name: preset.name })
  assert.deepEqual(
    new Set(options.map((option) => option.model)),
    new Set([active.model, ...preset.models])
  )
  assert.equal(options.length, preset.models.length + 1)
  assert.ok(options.every((option) => option.provider === preset.id))

  const chosen = options.find((option) => option.model === 'maas-deepseek/g-6-a')
  assert.ok(chosen)
  const choice = { provider: chosen.provider, model: chosen.model }
  const command = providerSaveCommand(choice, providers, [])
  assert.deepEqual(command.active, { provider: 'maas-deepseek', model: 'maas-deepseek/g-6-a' })
  assert.deepEqual(command.keys, [])
  assert.deepEqual(
    command.providers,
    providers.map(({ id, name, base_url }) => ({ id, name, base_url }))
  )
  assert.deepEqual(modelOptions({ active: command.active, providers })[0], chosen)
})
