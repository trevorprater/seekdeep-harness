//! Generated Vitest driver templates for the built Client test runtime.

pub(crate) const CONFIG: &str = r"export default {
  resolve: {
    alias: {
      react: __REACT__,
      'react-dom': __REACT_DOM__,
      '@testing-library/react': __TESTING_REACT__,
      '@testing-library/dom': __TESTING_DOM__,
      vitest: __VITEST__,
      immer: __IMMER__,
    },
  },
  test: {
    environment: 'jsdom',
    include: [__TEST_FILE__],
  },
}
";

pub(crate) const TEST: &str = r"import { describe, expect, it } from 'vitest'
import {
  FixtureSession,
  SlotTestRuntime,
  TestRoot,
  TestSessions,
  TestWorkspaces,
  conversationSnapshot,
  makeTranslate,
  stubSettingsScope,
  workspaceListState,
} from __RUNTIME_MODULE__

describe('built Rust/WASM client test runtime', () => {
  it('loads the curated public API and assembles a disposable runtime', async () => {
    expect(conversationSnapshot('built').sessionId).toBe('built')
    expect(workspaceListState()).toMatchObject({ phase: 'ready', items: [] })
    expect(makeTranslate({ hello: 'Hello {name}' })('hello', { name: 'WASM' })).toBe('Hello WASM')
    expect(stubSettingsScope().scope.getSnapshot().status).toBe('loading')

    const runtime = await SlotTestRuntime.create()
    expect(runtime).toBeInstanceOf(SlotTestRuntime)
    expect(typeof runtime.ctx.constructor.filter).toBe('symbol')
    expect('slots' in runtime.ctx).toBe(true)
    expect(runtime.root).toBeInstanceOf(TestRoot)
    expect(runtime.sessions).toBeInstanceOf(TestSessions)
    expect(runtime.workspaces).toBeInstanceOf(TestWorkspaces)
    const inheritedInject = { slots: null }
    const classInject = Object.create(inheritedInject)
    classInject[Symbol.for('cordis.checkProto')] = true
    class BuiltClassPlugin {
      static inject = classInject
      constructor(ctx) {
        this[Symbol.for('cordis.initHooks')] = [
          () => ctx.provide('built-class-service', { ok: true }),
        ]
      }
    }
    const classHandle = await runtime.mount(BuiltClassPlugin)
    expect(runtime.ctx.get('built-class-service')).toEqual({ ok: true })
    await classHandle.dispose()
    expect(runtime.ctx.get('built-class-service')).toBeUndefined()
    await runtime.sessions.add({ id: 'built' })
    expect(runtime.sessions.behavior('built')).toBeInstanceOf(FixtureSession)
    await runtime.dispose()
  })
})
";

pub(crate) const TYPECHECK: &str = r"import { SlotTestRuntime } from '@seekdeep-ai/seekdeep-client-test-runtime'

declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface SlotMap {
    'smoke.panel': { kind: 'single'; scope: 'root'; owner: { label: string } }
  }
}

async function exercise(runtime: SlotTestRuntime) {
  await runtime.declare({ 'smoke.panel': { kind: 'single', scope: 'root' } })
  const view = runtime.renderSlot('smoke.panel', { label: 'first' })
  view.update({ label: 'second' })
  // @ts-expect-error the augmented Slot owner requires label
  runtime.renderSlot('smoke.panel', {})
  // @ts-expect-error undeclared Slot keys are rejected by the public type contract
  runtime.renderSlot('smoke.missing', {})
}

void exercise
";

pub(crate) const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "strict": true,
    "noEmit": true,
    "skipLibCheck": true,
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "target": "ES2022",
    "lib": ["ES2022", "DOM"],
    "ignoreDeprecations": "6.0",
    "baseUrl": __ROOT__,
    "paths": {
      "@seekdeep-ai/cordis": [__CORDIS_TYPES__],
      "@seekdeep-ai/seekdeep-client-runtime/client": [__RUNTIME_TYPES__],
      "@seekdeep-ai/seekdeep-client-ui-slots": [__SLOT_TYPES__],
      "@seekdeep-ai/seekdeep-client-test-runtime": [__TEST_RUNTIME_TYPES__],
      "@testing-library/dom": [__TESTING_DOM_TYPES__],
      "@testing-library/react": [__TESTING_REACT_TYPES__],
      "react": [__REACT_TYPES__],
      "vitest": [__VITEST_TYPES__]
    }
  },
  "files": [__TYPECHECK_FILE__]
}
"#;
