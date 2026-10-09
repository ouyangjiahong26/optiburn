# 品牌资产生成工具

本目录负责生成 optiburn 的 README 横幅与站点图标，全部产物可复现：改矢量母版后重跑命令即可，不需要手工修图。

## 目录内容

| 文件 | 用途 |
|---|---|
| `banner.svg` | README 横幅母版（1536×400） |
| `icon.svg` | 站点图标母版（深底圆角方块加银色光盘），光栅化为站点 favicon |
| `mark.svg` | 透明底光盘标记，复制为站点导航栏 logo |
| `build-assets.mjs` | 由矢量母版渲染 `assets/banner.png` 与 `docs-site/static/img/favicon.png`，并复制 `logo.svg` |

## 常用命令

```bash
cd tools/assets
npm install
npm run build
```

产物：

- `assets/banner.png`：`README.md` 与 `README.zh-CN.md` 开头的横幅
- `docs-site/static/img/favicon.png`、`docs-site/static/img/logo.svg`：文档站使用
