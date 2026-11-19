import { compressedAssetsPlugin } from '../../scripts/performance/compressed-assets-plugin.mjs'
import { defineConfig } from 'vite'
import { writeClientBuildState } from '../../scripts/client-build-state.mjs'
import { fileURLToPath } from 'node:url'
const proxy = {
  '/data/props': { target: `http://127.0.0.1:${process.env.TOWN_MANAGER_PORT ?? 8788}` },
  '/api/towns': { target: `http://127.0.0.1:${process.env.TOWN_MANAGER_PORT ?? 8788}`, ws: true },
}
const lan = process.env.TOWN_LAN === '1'
const lanHosts = (process.env.TOWN_LAN_HOSTS ?? '')
  .split(',')
  .map((host) => host.trim())
  .filter(Boolean)
if (lan && !lanHosts.length) throw Error('局域网模式需要设置 TOWN_LAN_HOSTS')
const host = lan ? '0.0.0.0' : '127.0.0.1'
export default defineConfig({
  publicDir: false,
  plugins: [
    compressedAssetsPlugin(),
    {
      name: 'formal-client-build-state',
      apply: 'build',
      async closeBundle() {
        await writeClientBuildState(
          fileURLToPath(new URL('../../', import.meta.url)),
          fileURLToPath(new URL('./dist', import.meta.url))
        )
      },
    },
  ],
  server: { port: 5174, host, allowedHosts: lanHosts, strictPort: true, proxy },
  preview: { port: 5174, host, allowedHosts: lanHosts, strictPort: true, proxy },
  build: {
    rollupOptions: {
      input: { index: 'index.html', game: 'game.html' },
    },
  },
})
