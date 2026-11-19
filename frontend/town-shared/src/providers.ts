// 服务商预置目录：参照老项目 ModelProviderCatalog（内容权威 ~/dev/my_ai_town）。
// 全部走 OpenAI 兼容传输——base_url 不含 /chat/completions 后缀（host 拼接）。
// 模型清单 = 老目录预置 + 常用官方名；页面上模型可自由输入，清单只是候选。
// 玩家在「模型服务」里给一个服务填的模型名存在配置里（ProviderView.model），选这个服务时先填它。
import type { ActiveModelChoice, ProviderConfig, ProviderView } from './protocol'
import type { ModelChoice } from './protocol'

export interface ProviderPreset {
  id: string
  name: string
  baseUrl: string
  needsKey: boolean
  models: string[]
  defaultModel: string
  /** 聚合平台要在快捷选择里同时列出的模型；普通服务只列当前或默认模型。 */
  quickModels?: string[]
}

export const PRESETS: ProviderPreset[] = [
  {
    // MAAS 适配器：OpenAI 兼容端点，模型要用 /v1/models 返回的完整 ID
    id: 'maas-deepseek',
    name: 'MAAS DeepSeek',
    baseUrl: 'https://maas-codex-adapter.cloud.imgo.tv/v1',
    needsKey: true,
    models: [
      'maas-deepseek/deepseek-v4-1-flash',
      'maas-deepseek/deepseek-v4-flash-0731',
      'maas-deepseek/deepseek-v4-pro-0813',
      'maas-deepseek/fun-asr',
      'maas-deepseek/g-6-1-s',
      'maas-deepseek/g-6-a',
      'maas-deepseek/gemini-2.5-flash-image',
      'maas-deepseek/gemini-3-pro-image-preview',
      'maas-deepseek/gemini-3.1-flash-image-preview',
      'maas-deepseek/glm-5.1',
      'maas-deepseek/glm-5.2',
      'maas-deepseek/glm-5.3',
      'maas-deepseek/gpt-image-2',
      'maas-deepseek/happy-horse-1.0',
      'maas-deepseek/happy-horse-1.1',
      'maas-deepseek/kimi-k3',
      'maas-deepseek/msd-2-0-fast-260128',
      'maas-deepseek/msd-2-0-mini-260615',
      'maas-deepseek/msd-2-0-pro-260128',
      'maas-deepseek/msd-2-5-260628',
      'maas-deepseek/msd-video-2.5',
      'maas-deepseek/msd-video-fast',
      'maas-deepseek/msd-video-pro',
      'maas-deepseek/qwen3.7-max',
      'maas-deepseek/qwen3.7-plus',
      'maas-deepseek/qwen3.8-flash',
      'maas-deepseek/qwen3.8-max',
      'maas-deepseek/seedream-4.5',
      'maas-deepseek/seedream-5.0-lite',
      'maas-deepseek/wan2.6',
      'maas-deepseek/wan2.7',
      'maas-deepseek/wan2.7-pro',
      'maas-deepseek/wan3.0',
    ],
    defaultModel: 'maas-deepseek/deepseek-v4-1-flash',
  },
  {
    id: 'deepseek',
    name: 'DeepSeek',
    baseUrl: 'https://api.deepseek.com',
    needsKey: true,
    models: ['deepseek-chat', 'deepseek-reasoner', 'deepseek-v4-flash', 'deepseek-v4-pro'],
    defaultModel: 'deepseek-chat',
  },
  {
    id: 'kimi',
    name: 'Kimi',
    baseUrl: 'https://api.moonshot.cn/v1',
    needsKey: true,
    models: ['kimi-k3', 'kimi-k2.7-code-highspeed', 'kimi-k2.6', 'kimi-k2.5'],
    defaultModel: 'kimi-k3',
  },
  {
    id: 'zhipu-glm',
    name: '智谱 GLM',
    baseUrl: 'https://open.bigmodel.cn/api/paas/v4',
    needsKey: true,
    models: ['glm-5.2', 'glm-5.1', 'glm-5', 'glm-4.7', 'glm-5v-turbo'],
    defaultModel: 'glm-5.2',
  },
  {
    id: 'minimax',
    name: 'MiniMax',
    baseUrl: 'https://api.minimaxi.com/v1',
    needsKey: true,
    models: [
      'MiniMax-M3',
      'MiniMax-M2.7',
      'MiniMax-M2.7-highspeed',
      'MiniMax-M2.5',
      'MiniMax-M2.5-highspeed',
      'MiniMax-M2.1',
      'MiniMax-M2.1-highspeed',
      'MiniMax-M2',
    ],
    defaultModel: 'MiniMax-M2.7',
  },
  {
    id: 'volcengine-ark',
    name: '火山方舟',
    baseUrl: 'https://ark.cn-beijing.volces.com/api/v3',
    needsKey: true,
    models: ['doubao-seed-2-1-turbo-260628', 'glm-5-3-flash-260828'],
    defaultModel: 'doubao-seed-2-1-turbo-260628',
    quickModels: ['doubao-seed-2-1-turbo-260628', 'glm-5-3-flash-260828'],
  },
  {
    id: 'alibaba-bailian',
    name: '阿里百炼',
    baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
    needsKey: true,
    models: [
      'qwen3.7-plus',
      'qwen3.7-max',
      'qwen3.6-plus',
      'qwen3.6-flash',
      'qwen3.6-max-preview',
      'qwen3.5-plus',
      'qwen3.5-flash',
    ],
    defaultModel: 'qwen3.7-plus',
  },
  {
    id: 'xiaomi-mimo',
    name: '小米 MiMo',
    baseUrl: 'https://api.xiaomimimo.com/v1',
    needsKey: true,
    models: ['mimo-v2.6-pro', 'mimo-v2.6-pro-ultraspeed', 'mimo-v2.6-flash'],
    defaultModel: 'mimo-v2.6-pro',
  },
  {
    id: '302-ai',
    name: '302.AI',
    baseUrl: 'https://api.302.ai/v1',
    needsKey: true,
    models: [],
    defaultModel: '',
  },
  {
    // 老项目走 Ollama 原生 API；本项目统一 OpenAI 兼容端点
    id: 'ollama',
    name: 'Ollama（本地）',
    baseUrl: 'http://127.0.0.1:11434/v1',
    needsKey: false,
    models: [],
    defaultModel: '',
  },
  {
    id: 'ollama-cloud',
    name: 'Ollama Cloud',
    baseUrl: 'https://ollama.com/v1',
    needsKey: true,
    models: [],
    defaultModel: '',
  },
  {
    id: 'lm-studio',
    name: 'LM Studio（本地）',
    baseUrl: 'http://127.0.0.1:1234/v1',
    needsKey: false,
    models: [],
    defaultModel: '',
  },
  {
    id: 'custom',
    name: '自定义（OpenAI 兼容）',
    baseUrl: '',
    needsKey: true,
    models: [],
    defaultModel: '',
  },
  {
    // 随游戏带的小模型，世界进程在本机跑；应用带着它时服务端才把它列进配置
    id: 'builtin',
    name: '内置模型',
    baseUrl: '',
    needsKey: false,
    models: ['小镇村民'],
    defaultModel: '小镇村民',
  },
]

/** 首页和请村民时共用候选：保留玩家手填的当前模型；服务在「模型服务」里记着模型名的，没有当前模型时列它。
 *  自定义、本机服务没有预置的缺省模型，记着的模型名是它们出现在候选里的唯一来由。 */
export function quickModelNames(provider: string, current?: string, saved?: string): string[] {
  const preset = PRESETS.find((p) => p.id === provider)
  const rest = preset?.quickModels
    ? [saved, ...preset.quickModels]
    : [current ?? (saved || preset?.defaultModel)]
  return [...new Set([current, ...rest])].filter((model): model is string => !!model)
}

/** 选了这个服务时模型名先填什么：「模型服务」里给它填过的；没填过时默认模型用着这个服务就用它的，
 *  再不然用预置的缺省；都没有为空串 */
export function providerModel(
  id: string,
  conf?: Pick<ProviderView, 'model'>,
  active?: ActiveModelChoice | null
): string {
  return (
    conf?.model ||
    (active?.provider === id ? active.model : '') ||
    PRESETS.find((p) => p.id === id)?.defaultModel ||
    ''
  )
}

/** 模型名输入框的候选：这个服务记着的排最前，再列预置的 */
export function modelCandidates(id: string, conf?: Pick<ProviderView, 'model'>): string[] {
  const preset = PRESETS.find((p) => p.id === id)
  return [...new Set([conf?.model, ...(preset?.models ?? [])])].filter(
    (model): model is string => !!model
  )
}

/** 服务端按整表保存：没改的服务原样写回去的那一行。记着的模型名要一起带上，不带就清掉了 */
export function providerRow(p: Partial<ProviderView> & Pick<ProviderView, 'id' | 'name' | 'base_url'>): ProviderConfig {
  return { id: p.id, name: p.name, base_url: p.base_url, ...(p.model ? { model: p.model } : {}) }
}

/** 内置模型：不用地址和密钥，只给村民过日子用，不写人设、不建镇 */
export const BUILTIN = 'builtin'

export interface ProviderModelOption extends ModelChoice {
  name: string
}

export function usableProvider(provider: ProviderView): boolean {
  return (
    !!provider.key_mask ||
    PRESETS.find((preset) => preset.id === provider.id)?.needsKey === false ||
    (!!provider.base_url && localEndpoint(provider.base_url))
  )
}

export function modelOptions(
  config: { active: ModelChoice | null; providers: ProviderView[] } | null
): ProviderModelOption[] {
  if (!config) return []
  const options: ProviderModelOption[] = []
  const providers = config.providers.filter(usableProvider)
  const add = (provider: ProviderView, model: string) => {
    if (
      !model ||
      options.some((option) => option.provider === provider.id && option.model === model)
    )
      return
    options.push({ provider: provider.id, model, name: provider.name })
  }
  const active = config.active
  const chosen = active && providers.find((provider) => provider.id === active.provider)
  if (chosen && active) add(chosen, active.model)
  for (const provider of providers) {
    const preset = PRESETS.find((preset) => preset.id === provider.id)
    if (preset) {
      add(provider, preset.defaultModel)
      for (const model of preset.models) add(provider, model)
    }
  }
  return options
}

/** 本机或局域网地址上的服务（Ollama、LM Studio 这类）不用密钥；和服务端的判断一致 */
export function localEndpoint(url: string): boolean {
  let host = ''
  try {
    host = new URL(url).hostname.replace(/^\[|\]$/g, '').toLowerCase()
  } catch {
    return false
  }
  return (
    host === 'localhost' ||
    host.endsWith('.localhost') ||
    host.endsWith('.local') ||
    host === '::1' ||
    /^(127\.|10\.|192\.168\.|169\.254\.|0\.0\.0\.0$)/.test(host) ||
    /^172\.(1[6-9]|2\d|3[01])\./.test(host)
  )
}

/** 要不要填密钥：预置里不用的不要，地址填在本机或局域网的（比如自定义地址接本机模型）也不要 */
export function needsKey(preset: ProviderPreset | undefined, baseUrl = ''): boolean {
  return !!preset?.needsKey && !localEndpoint(baseUrl)
}
