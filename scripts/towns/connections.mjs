import http from 'node:http'
import { HttpError } from './http.mjs'

export function requestAccess({ lan, lanHosts }) {
  const allowed = new Set(['127.0.0.1', 'localhost'])
  if (lan) {
    if (!lanHosts.length) throw Error('局域网模式需要设置 TOWN_LAN_HOSTS（允许访问的 IP 或主机名）')
    for (const host of lanHosts) {
      if (!/^[a-zA-Z0-9.-]+$/.test(host)) throw Error('TOWN_LAN_HOSTS 只填写 IP 或主机名，不带端口')
      allowed.add(host.toLowerCase())
    }
  }
  return (req) => {
    const authority = req.headers.host ?? ''
    if (!/^[a-zA-Z0-9.-]+(?::\d+)?$/.test(authority)) throw new HttpError(403, '无效的访问地址')
    const url = new URL(`http://${authority}`)
    if (!allowed.has(url.hostname))
      throw new HttpError(403, lan ? '访问地址未获允许' : '仅提供本机服务')
    if (req.headers.origin && req.headers.origin !== url.origin)
      throw new HttpError(403, '不允许跨站访问小镇')
  }
}

export function createConnections({ hosts, validateRequest }) {
  const sockets = new Set()
  const requests = new Set()
  let stopping = false
  function upgrade(req, socket, head) {
    socket.on('error', () => socket.destroy())
    const reject = (status) =>
      socket.end(`HTTP/1.1 ${status}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`)
    if (stopping) return reject('503 Service Unavailable')
    let endpoint
    try {
      validateRequest(req)
      const url = new URL(req.url, 'http://127.0.0.1')
      const match = /^\/api\/towns\/([a-z0-9-]+)\/socket$/.exec(url.pathname)
      if (!match) throw new HttpError(404, '连接不存在')
      endpoint = hosts.connection(match[1], url.searchParams.get('session'))
      if (!endpoint) throw new HttpError(403, '世界连接已失效')
      if (req.headers.upgrade?.toLowerCase() !== 'websocket') throw new HttpError(400, '无效的连接')
    } catch (error) {
      return reject(error.status === 404 ? '404 Not Found' : '403 Forbidden')
    }
    sockets.add(socket)
    const target = new URL(endpoint)
    const upstream = http.request({
      hostname: target.hostname,
      port: target.port,
      path: target.pathname + target.search,
      headers: {
        Connection: 'Upgrade',
        Upgrade: 'websocket',
        'Sec-WebSocket-Key': req.headers['sec-websocket-key'],
        'Sec-WebSocket-Version': req.headers['sec-websocket-version'],
      },
    })
    requests.add(upstream)
    socket.once('close', () => {
      sockets.delete(socket)
      requests.delete(upstream)
      upstream.destroy()
    })
    upstream.setTimeout(10000, () => upstream.destroy(Error('连接世界超时')))
    upstream.once('error', () => socket.destroy())
    upstream.once('response', (response) => {
      response.resume()
      reject('502 Bad Gateway')
    })
    upstream.once('upgrade', (response, world, worldHead) => {
      upstream.setTimeout(0)
      world.setTimeout(0)
      requests.delete(upstream)
      if (stopping || socket.destroyed) return world.destroy()
      sockets.add(world)
      world.on('error', () => socket.destroy())
      world.once('close', () => {
        sockets.delete(world)
        socket.destroy()
      })
      socket.once('close', () => world.destroy())
      socket.write(
        `HTTP/1.1 101 Switching Protocols\r\n${Object.entries(response.headers)
          .map(([name, value]) => `${name}: ${value}\r\n`)
          .join('')}\r\n`
      )
      if (worldHead.length) socket.write(worldHead)
      if (head.length) world.write(head)
      socket.pipe(world).pipe(socket)
    })
    upstream.end()
  }
  function stop() {
    stopping = true
    for (const request of requests) request.destroy()
    for (const socket of sockets) socket.destroy()
  }
  return { upgrade, stop }
}
