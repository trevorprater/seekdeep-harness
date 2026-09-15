import DefaultTheme from 'vitepress/theme'
import { withBase } from 'vitepress'

export default {
  extends: DefaultTheme,
  async enhanceApp() {
    if (!import.meta.env.SSR) {
      // Public WASM assets need an HTTP URL so Vite does not transform their imports.
      const runtimeUrl = new URL(withBase('/_seekdeep/docs-site.mjs'), window.location.href).href
      await import(/* @vite-ignore */ runtimeUrl)
    }
  },
}
