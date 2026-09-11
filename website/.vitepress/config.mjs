import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { withMermaid } from 'vitepress-plugin-mermaid'

const state = JSON.parse(readFileSync(new URL('../.cache/site-config.json', import.meta.url), 'utf8'))
const runtime = createRequire(import.meta.url)(state.runtime)
globalThis.__seekdeepDocsRuntime = runtime
const project = () => execFileSync(state.projector, ['--root', state.root, '--repository-ref', state.revision], { stdio: 'inherit' })
project()

const config = state.config
const editLink = runtime.editLinkPattern(state.editBranch)
config.themeConfig.editLink.pattern = editLink
config.locales.en.themeConfig.editLink.pattern = editLink
config.vite.plugins = [{
  name: 'seekdeep-harness-doc-projector',
  configureServer(server) {
    server.watcher.add(state.sources)
    server.watcher.on('change', changed => {
      if (runtime.shouldProject(state.sources, changed)) project()
    })
  },
}]
config.markdown = {
  config(md) {
    const text = md.renderer.rules.text
    const code = md.renderer.rules.code_inline
    runtime.validateMarkdownRules(text, code)
    md.renderer.rules.text = (...args) => runtime.escapeVueInterpolation(text(...args))
    md.renderer.rules.code_inline = (...args) => runtime.escapeVueInterpolation(code(...args))
  },
}

export default withMermaid(config)
