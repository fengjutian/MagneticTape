// @ts-check
import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";

/**
 * MagneticTape 文档站（Astro + Starlight）
 *
 * - 内容分两类：`src/content/docs/start|guide` 是面向用户的手册，
 *   `src/content/docs/design` 由 `npm run sync:docs` 从仓库根的 `doc/` 同步生成。
 * - 构建产物为纯静态文件，可直接托管到 Netlify（见仓库根 `netlify.toml`）。
 */
export default defineConfig({
  site: "https://magnetictape.netlify.app",
  trailingSlash: "ignore",
  markdown: {
    // 允许在 .md 页面里使用 <figure> 等原生 HTML（截图展示依赖它）
    html: true,
  },
  build: {
    format: "directory",
  },
  integrations: [
    starlight({
      title: "MagneticTape",
      description:
        "Windows 本地优先的 AI 录音工作台：双路录音、本地转写、实时字幕与会议纪要。",
      logo: {
        src: "./src/assets/logo.svg",
        replacesTitle: false,
      },
      favicon: "/favicon.svg",
      social: [
        {
          icon: "github",
          label: "GitHub 仓库",
          href: "https://github.com/fengjutian/MagneticTape",
        },
      ],
      locales: {
        root: {
          label: "简体中文",
          lang: "zh-CN",
        },
      },
      defaultLocale: "root",
      customCss: ["./src/styles/aurora.css"],
      lastUpdated: true,
      editLink: {
        baseUrl:
          "https://github.com/fengjutian/MagneticTape/edit/main/apps/site/src/content/docs/",
      },
      sidebar: [
        {
          label: "开始",
          items: [
            { label: "项目概览", link: "/" },
            { label: "安装", link: "/start/install/" },
            { label: "快速开始", link: "/start/quick-start/" },
          ],
        },
        {
          label: "使用指南",
          items: [
            { label: "录音与回放", link: "/guide/recording/" },
            { label: "字幕与转写", link: "/guide/transcription/" },
            { label: "AI 能力", link: "/guide/ai/" },
            { label: "导出与整理", link: "/guide/export/" },
            { label: "设置参考", link: "/guide/settings/" },
            { label: "数据与隐私", link: "/guide/privacy/" },
            { label: "常见问题", link: "/guide/faq/" },
          ],
        },
        {
          label: "设计文档",
          items: [{ autogenerate: { directory: "design" } }],
        },
      ],
    }),
  ],
});
