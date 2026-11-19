import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import vm from 'node:vm'
import { createRequire } from 'node:module'

const require = createRequire(new URL('../package.json', import.meta.url))
const ts = require('typescript')

function harness(normalSpeed = 1) {
  class Element extends EventTarget {
    children = []
    attributes = new Map()
    disabled = false
    value = ''
    validity = ''
    open = false
    classList = { toggle() {} }
    append(...children) {
      this.children.push(...children)
    }
    replaceChildren(...children) {
      this.children = children
    }
    setAttribute(name, value) {
      this.attributes.set(name, value)
    }
    closest() {
      return null
    }
    getClientRects() {
      return [{}]
    }
    click() {
      if (!this.disabled) this.onclick()
    }
    setCustomValidity(message) {
      this.validity = message
    }
    reportValidity() {}
    focus() {}
    select() {}
    showModal() {
      this.open = true
    }
    close() {
      this.open = false
    }
  }
  const speeds = new Element()
  const document = {
    body: new Element(),
    createElement: () => new Element(),
    getElementById: () => speeds,
    querySelector: () => document.body.children.find((element) => element.open) ?? null,
  }
  const window = new EventTarget()
  let now = 0
  const load = (file) => {
    const scope = {
      exports: {},
      document,
      window,
      HTMLElement: Element,
      SVGElement: Element,
      performance: { now: () => now },
      require(name) {
        if (name === '../dom') return load('dom')
        if (name === '../ids') return { newId: () => 'speed-test' }
        if (name === '../game/pixel') return { pixelIcon: () => new Element(), setPixelIcon() {} }
        if (name === './game/icons') return { gameIcon: () => new Element() }
        if (name === './platform') return { isTouch: () => false }
        return {}
      },
    }
    vm.runInNewContext(
      ts.transpileModule(fs.readFileSync(new URL(`../src/${file}.ts`, import.meta.url), 'utf8'), {
        compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
      }).outputText,
      scope
    )
    return scope.exports
  }
  const sent = []
  const bar = load('shell/topbar').installSpeedBar(
    (msg) => sent.push(msg),
    () => {},
    normalSpeed
  )
  const hint = load('pauseHint').installPauseHint(
    new Element(),
    (speed) => (speed > 0 ? bar.start() : (bar.choose(speed), true)),
    1
  )
  const sync = (speed) => hint.update(bar.sync(speed), speed)
  const space = () =>
    window.dispatchEvent(
      Object.assign(new Event('keydown', { cancelable: true }), { code: 'Space' })
    )
  const editor = document.body.children[0]
  const form = editor.children[0]
  const input = form.children[1].children[0]
  const submit = (value) => {
    input.value = value
    input.dispatchEvent(new Event('input'))
    form.dispatchEvent(new Event('submit', { cancelable: true }))
  }
  return {
    bar,
    sent,
    buttons: speeds.children,
    editor,
    input,
    submit,
    sync,
    space,
    advance: () => (now += 4000),
  }
}

test('custom world speed sends real decimal multipliers and survives pause and server sync', () => {
  const { bar, sent, buttons, submit, sync, space } = harness()
  assert.equal(buttons.length, 2)
  assert.ok(buttons.every((control) => control.disabled))
  for (const control of buttons) control.disabled = false
  sync(0)
  buttons[1].click()
  submit('2.5')
  assert.equal(sent.at(-1).type, 'set_speed')
  assert.equal(sent.at(-1).speed, 2.5)
  assert.equal(bar.sync(0), 2.5)
  sync(2.5)
  assert.equal(buttons[1].textContent, '2.5×')
  buttons[0].click()
  sync(0)
  space()
  assert.equal(sent.at(-1).speed, 2.5)
  sync(2.5)
  space()
  assert.equal(sent.at(-1).speed, 0)
  sync(0)
  buttons[0].click()
  assert.equal(sent.at(-1).speed, 2.5)
  sync(2.5)
  sync(2)
  assert.equal(bar.current(), 2)
  assert.equal(buttons[1].textContent, '2×')
})

test('accelerated starts respect the model gate and rejected commands return to the server state', () => {
  const { bar, sent, buttons, submit, sync, advance } = harness()
  for (const control of buttons) control.disabled = false
  sync(0)
  let resume
  bar.setGate((go) => {
    resume = go
    return true
  })
  buttons[1].click()
  submit('8')
  assert.equal(sent.length, 0)
  assert.equal(bar.current(), 0)
  resume()
  assert.equal(sent.at(-1).speed, 8)
  sync(0)
  assert.equal(bar.current(), 8)
  advance()
  sync(0)
  assert.equal(bar.current(), 0)
})

test('speed editor rejects invalid values and cancellation or disconnection sends no command', () => {
  const { sent, buttons, editor, input, submit, sync, space } = harness()
  for (const control of buttons) control.disabled = false
  sync(1)
  buttons[1].click()
  space()
  assert.equal(sent.length, 0, '编辑时空格不切换暂停')
  submit('NaN')
  assert.ok(input.validity)
  assert.equal(editor.open, true)
  assert.equal(sent.length, 0)
  submit('601')
  assert.equal(sent.length, 0)
  submit('0')
  assert.equal(sent.length, 0)
  editor.children[0].children[3].children[0].click()
  assert.equal(editor.open, false)
  assert.equal(sent.length, 0)
  buttons[1].click()
  buttons[1].disabled = true
  submit('4')
  assert.equal(sent.length, 0)
})

test('the speed limit is the server speed, not the map normal speed', () => {
  const { sent, buttons, input, submit, sync } = harness(10)
  for (const control of buttons) control.disabled = false
  sync(10)
  buttons[1].click()
  assert.equal(input.max, '60')
  submit('60')
  assert.equal(sent.at(-1).speed, 600)
  submit('60.1')
  assert.equal(sent.at(-1).speed, 600)
})
