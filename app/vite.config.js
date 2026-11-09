// 构建链路对齐（DEF-01）：页面运行时通过 importmap 引用 /vendor/*、按绝对路径请求
// /assets/*，而 Vite 默认只处理 publicDir，不会复制源码里的 vendor/ 与 assets/。
// 本插件在打包结束后把这两个目录原样拷进 dist，使 `npm run build` 的产物
// 与 uvicorn 直托管源码目录的行为一致。
// 注意：importmap 在构建产物中会被打包后的模块路径取代，保留不影响运行。
import { defineConfig } from 'vite';
import { cpSync, mkdirSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  publicDir: false,
  plugins: [
    {
      name: 'copy-vendor-and-assets',
      closeBundle() {
        const dist = resolve(root, 'dist');
        mkdirSync(dist, { recursive: true });
        for (const dir of ['vendor', 'assets']) {
          cpSync(resolve(root, dir), resolve(dist, dir), { recursive: true });
        }
      },
    },
  ],
  build: {
    // main.js 用了顶层 await，默认 target(es2020) 会在 esbuild 阶段直接报错；
    // 运行载体是本机现代浏览器 / WebView2，统一按 esnext 产出。
    target: 'esnext',
    chunkSizeWarningLimit: 4096,
  },
});
