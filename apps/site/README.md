# 文档站（apps/site）

MagneticTape 的公开文档站，基于 **Astro + Starlight**，构建产物是纯静态文件。

## 本地运行

```powershell
cd apps/site
npm install
npm run dev      # http://localhost:4321
```

构建（与 Netlify 上线用同一条命令）：

```powershell
npm run build    # 先同步设计文档，再 astro build，产物在 dist/
npm run preview  # 本地预览构建产物
```

## 目录结构

```text
apps/site/
├── astro.config.mjs            # 站点配置：标题、语言、侧边栏、主题色
├── netlify.toml                # 部署配置（在仓库根目录）
├── scripts/
│   └── sync-design-docs.mjs    # 把仓库 doc/ 同步成站点内容
├── src/
│   ├── assets/                 # logo / favicon
│   ├── content/docs/
│   │   ├── index.mdx           # 落地页（splash 模板）
│   │   ├── start/              # 安装、快速开始
│   │   ├── guide/              # 使用指南（面向最终用户）
│   │   └── design/             # ⚠️ 自动生成，勿手改
│   ├── content.config.ts       # 内容集合定义
│   └── styles/aurora.css       # 视觉令牌（与桌面端 v7 规范一致）
└── public/                     # 静态资源（截图等）
```

## 内容来源

| 目录 | 来源 | 谁维护 |
| --- | --- | --- |
| `src/content/docs/start`、`guide` | 手写 | 面向最终用户的使用手册 |
| `src/content/docs/design` | 仓库根 `doc/*.md` | `npm run sync:docs` 自动生成，**不要手改** |

`npm run sync:docs` 会：

1. 读取 `doc/` 下登记在 `scripts/sync-design-docs.mjs` 里的文档；
2. 生成 frontmatter（标题、摘要、侧栏顺序），并去掉正文首个 H1；
3. 把正文里的 `](同目录文档.md)` 相对链接改写成站点路径；
4. 输出到 `src/content/docs/design/<slug>.md`。

**新增一篇设计文档**时：在 `doc/` 下写好 Markdown，然后在 `scripts/sync-design-docs.mjs` 的 `DOCS` 数组里登记文件名、slug、标题、摘要和顺序。构建前会自动同步。

## 截图约定

真实界面截图放在 `apps/site/public/screenshots/`，用 `.png`：

```text
apps/site/public/screenshots/
├── recorder.png
├── transcript.png
└── insights.png
```

页面里这样引用（占位图会在图片缺失时自动顶替）：

```md
<figure class="mt-shot">
  <img src="/screenshots/recorder.png" alt="录音控制栏与状态卡" />
  <figcaption>录音控制栏：场景预设、设备选择与状态卡</figcaption>
</figure>
```

未提供截图前，`public/screenshots/*.svg` 里的占位图会显示在页面上（尺寸一致，不会破版）。真实截图到位后，把同名 `.png` 放进该目录即可，`.svg` 占位图可以保留作为兜底。

## 部署（Netlify）

1. Netlify 控制台 → Add new site → Import an existing project → 选 `fengjutian/MagneticTape`。
2. 构建配置会自动读取仓库根的 `netlify.toml`：

   | 项 | 值 |
   | --- | --- |
   | 构建目录 | `apps/site` |
   | 构建命令 | `npm run build` |
   | 发布目录 | `dist` |

3. 站点地址形如 `https://<站点名>.netlify.app`。若要换成自定义域名，在 Netlify 的 **Domain settings** 里添加域名并在 DNS 处配置 CNAME 即可。

仓库根的 `netlify.toml` 是唯一事实来源；本地用 `npx netlify-cli deploy` 也能复用同一份配置。

## 约束

- **不放任何第三方脚本与统计埋点**：项目主张本地优先，文档站也要守住这条线。
- **不引用外部图片 CDN**：所有资源进 `public/`，保证离线可读。
- **站点不与桌面端共享 `dist`**：`apps/desktop/dist` 是 Tauri 的打包目录，两者互不影响。
