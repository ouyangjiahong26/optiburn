// 把 SVG 渲染成位图资产（README 横幅、站点图标）。
// 用法：npm install && node build-assets.mjs
import { readFile, writeFile, mkdir, copyFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Resvg } from '@resvg/resvg-js';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '..', '..');

/** 用 resvg 渲染 SVG 文件到指定位图路径。 */
async function renderTo(svgRel, outRel, width) {
  const svg = await readFile(path.resolve(repo, svgRel));
  const png = new Resvg(svg, { fitTo: { mode: 'width', value: width }, background: 'rgba(0,0,0,0)' })
    .render()
    .asPng();
  const out = path.resolve(repo, outRel);
  await mkdir(path.dirname(out), { recursive: true });
  await writeFile(out, png);
  console.log(`${outRel} ← ${svgRel}（宽 ${width}）`);
}

await renderTo('tools/assets/banner.svg', 'assets/banner.png', 1536);
await renderTo('tools/assets/icon.svg', 'docs-site/static/img/favicon.png', 64);

// 透明标记是位图无关的矢量，直接复制给站点导航栏
await copyFile(
  path.resolve(repo, 'tools/assets/mark.svg'),
  path.resolve(repo, 'docs-site/static/img/logo.svg'),
);
console.log('docs-site/static/img/logo.svg ← tools/assets/mark.svg');
