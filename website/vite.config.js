import { resolve } from 'node:path'
import { defineConfig } from 'vite'

// Static multi-page site. Build emits index.html + docs.html.
export default defineConfig({
  base: './',
  build: {
    rollupOptions: {
      input: {
        main: resolve(__dirname, 'index.html'),
        docs: resolve(__dirname, 'docs.html'),
        demo: resolve(__dirname, 'demo.html'),
      },
    },
  },
})
