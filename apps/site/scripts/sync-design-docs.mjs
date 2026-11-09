/**
 * 把仓库根 `doc/` 下的设计文档同步成文档站内容。
 *
 * - 输入：`<repo>/doc/*.md`（唯一事实来源，不修改）
 * - 输出：`apps/site/src/content/docs/design/*.md`（ASCII 文件名 + 中文 frontmatter 标题）
 * - 同步内容：frontmatter（title / description / 侧栏顺序）、去掉正文首个 H1、
 *   把 `](xxx.md)` 这类同目录相对链接改写成站点路径。
 *
 * 用法：npm run sync:docs（已由 npm run build 自动调用）
 */
import { readFile, writeFile, readdir, mkdir, rm, copyFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const siteRoot = resolve(here, "..");
const repoRoot = resolve(siteRoot, "..", "..");
const docDir = join(repoRoot, "doc");
const outDir = join(siteRoot, "src", "content", "docs", "design");
const designAssetDir = join(siteRoot, "public", "screenshots", "design");
const manifestPath = join(here, "design-image-manifest.json");

const GITHUB_BLOB = "https://github.com/fengjutian/MagneticTape/blob/main";

/** 源文件名 → 站点 slug / 标题 / 摘要 / 侧栏顺序 */
const DOCS = [
  {
    file: "UI设计规范.md",
    slug: "ui-guidelines",
    title: "UI 设计规范",
    description: "桌面端的色彩、间距、组件与交互规范基线。",
    order: 1,
  },
  {
    file: "视觉风格-v7-极光玻璃.md",
    slug: "visual-style-v7",
    title: "视觉风格 v7：极光玻璃",
    description: "极光画布、玻璃外壳与绿 / Iris 双语义色的完整令牌体系。",
    order: 2,
  },
  {
    file: "录音页面布局需求.md",
    slug: "recorder-layout",
    title: "录音页面布局需求",
    description: "录音页各区域的布局约束与响应式规则。",
    order: 3,
  },
  {
    file: "AI录音工作台-UIUX需求.md",
    slug: "workspace-requirements",
    title: "AI 录音工作台 UI/UX 需求",
    description: "三栏工作台的产品方向、信息架构与交互需求。",
    order: 4,
  },
  {
    file: "AI录音工作台-布局实现规格.md",
    slug: "workspace-layout-spec",
    title: "AI 录音工作台布局实现规格",
    description: "工作台布局从需求到代码的逐项实现规格。",
    order: 5,
  },
  {
    file: "Workspace-UI-V1.0-详细设计规格.md",
    slug: "workspace-ui-v1-spec",
    title: "Workspace UI V1.0 详细设计规格",
    description: "工作台 V1.0 的详细设计与组件规格。",
    order: 6,
  },
  {
    file: "本地Whisper转写需求.md",
    slug: "local-whisper-requirements",
    title: "本地 Whisper 转写需求",
    description: "离线转写引擎、模型下载与校验的需求定义。",
    order: 7,
  },
  {
    file: "端侧模型选型调研.md",
    slug: "on-device-model-research",
    title: "端侧模型选型调研",
    description: "端侧 ASR / LLM 候选模型的实测与选型结论。",
    order: 8,
  },
  {
    file: "DIAR-1-spike报告.md",
    slug: "diar-1-spike-report",
    title: "DIAR-1 spike 报告",
    description: "说话人分离路线实测：sherpa-onnx CLI 子进程与阈值定标。",
    order: 9,
  },
  {
    file: "实时字幕与说话人分离.md",
    slug: "realtime-subtitle-and-speakers",
    title: "实时字幕与说话人分离",
    description: "本地流式字幕引擎、声纹聚类与说话人标注方案。",
    order: 10,
  },
  {
    file: "功能规划.md",
    slug: "feature-roadmap",
    title: "功能规划（未完成项）",
    description: "尚未实现或尚未验证的功能清单、优先级与里程碑。",
    order: 11,
  },
  {
    file: "技术改进与修复计划.md",
    slug: "technical-improvement-plan",
    title: "技术改进与修复计划",
    description: "围绕正确性、稳定性与工程化的改进计划。",
    order: 12,
  },
  {
    file: "对标飞书妙记差距分析.md",
    slug: "gap-analysis-vs-feishu",
    title: "对标飞书妙记差距分析",
    description: "与飞书妙记在录音内核、产品层与桌面集成上的逐项差距量化。",
    order: 13,
  },
  {
    file: "飞书妙记录音-逆向分析.md",
    slug: "feishu-memo-reverse-analysis",
    title: "飞书妙记「录音」功能逆向分析",
    description: "对飞书妙记录音链路的逆向分析报告与技术基线。",
    order: 14,
  },
];

const byFile = new Map(DOCS.map((d) => [d.file, d]));

/** 把正文里的同目录相对链接改写成站点路径 */
function rewriteLinks(body) {
  return body.replace(/\]\(([^)\s]+?\.md)(#[^)\s]*)?\)/g, (match, target, anchor = "") => {
    const file = decodeURIComponent(target.split("/").pop() ?? target);
    const doc = byFile.get(file);
    if (doc) return `](/design/${doc.slug}/${anchor})`;
    if (file === "README.md") return `(${GITHUB_BLOB}/README.md)`;
    return match;
  });
}

/** 去掉正文首个一级标题（标题已由 frontmatter 提供） */
function stripLeadingTitle(body) {
  const lines = body.split(/\r?\n/);
  const idx = lines.findIndex((line) => /^#\s+/.test(line));
  if (idx === -1) return body.trimStart();
  lines.splice(idx, 1);
  return lines.join("\n").trimStart();
}

/**
 * 把正文里的相对图片路径改写成站点路径 `/screenshots/design/<file>`。
 * 源文件存在就复制过去；不存在则登记进清单，由 make-placeholder.ps1 生成占位图。
 */
function rewriteImages(body, registry) {
  return body.replace(/!\[([^\]]*)\]\(([^)\s]+)(\s+"[^"]*")?\)/g, (match, alt, src) => {
    if (/^(https?:)?\/\//.test(src) || src.startsWith("/")) return match;
    const file = src.split("/").pop();
    if (!registry.has(file)) {
      registry.set(file, { file, alt: alt.trim(), source: join(docDir, file) });
    }
    return `![${alt}](/screenshots/design/${file})`;
  });
}

function frontmatter(doc) {
  return [
    "---",
    `title: "${doc.title}"`,
    `description: "${doc.description}"`,
    `sidebar:`,
    `  order: ${doc.order}`,
    `  label: "${doc.title}"`,
    "editLink: false",
    "---",
    "",
  ].join("\n");
}

async function main() {
  if (!existsSync(docDir)) {
    throw new Error(`找不到文档目录：${docDir}`);
  }

  const sources = (await readdir(docDir)).filter((name) => name.endsWith(".md"));
  const known = sources.filter((name) => byFile.has(name));
  const unknown = sources.filter((name) => !byFile.has(name));

  if (unknown.length > 0) {
    console.warn(`[sync-docs] 未登记的文档（已跳过）：${unknown.join("、")}`);
  }

  if (existsSync(outDir)) {
    await rm(outDir, { recursive: true, force: true });
  }
  await mkdir(outDir, { recursive: true });
  if (existsSync(designAssetDir)) {
    await rm(designAssetDir, { recursive: true, force: true });
  }
  await mkdir(designAssetDir, { recursive: true });

  const imageRegistry = new Map();

  for (const doc of DOCS) {
    if (!sources.includes(doc.file)) {
      console.warn(`[sync-docs] 源文件缺失，已跳过：${doc.file}`);
      continue;
    }
    const raw = await readFile(join(docDir, doc.file), "utf8");
    const body = rewriteLinks(rewriteImages(stripLeadingTitle(raw), imageRegistry));
    const note = [
      `> 本页由仓库 \`doc/${doc.file}\` 自动同步，内容以主仓库为准。`,
      `> [在 GitHub 上查看源文件](${GITHUB_BLOB}/doc/${encodeURI(doc.file)})`,
      "",
    ].join("\n");
    const content = `${frontmatter(doc)}${note}\n${body.trimStart()}`;
    await writeFile(join(outDir, `${doc.slug}.md`), content, "utf8");
  }

  // 复制仓库里真实存在的配图，登记缺失的配图供占位脚本生成
  const missing = [];
  for (const entry of imageRegistry.values()) {
    if (existsSync(entry.source)) {
      await copyFile(entry.source, join(designAssetDir, entry.file));
    } else {
      missing.push({ file: entry.file, alt: entry.alt });
    }
  }
  await writeFile(manifestPath, `${JSON.stringify(missing, null, 2)}\n`, "utf8");

  console.log(
    `[sync-docs] 已同步 ${known.length} 篇设计文档 → ${outDir}（配图 ${imageRegistry.size} 张，其中 ${missing.length} 张待补）`,
  );
}

await main();
