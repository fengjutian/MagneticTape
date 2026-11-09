/**
 * 校验构建产物里的站内链接是否都指向真实存在的页面。
 *
 * 用法：npm run build && npm run check:links
 */
import { readdir, readFile, stat } from "node:fs/promises";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const distDir = resolve(here, "..", "dist");

async function* walk(dir) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) yield* walk(full);
    else yield full;
  }
}

function targetExists(pathname) {
  const clean = pathname.split("#")[0].split("?")[0];
  if (!clean || clean === "/") return existsSync(join(distDir, "index.html"));
  const candidates = [
    join(distDir, clean, "index.html"),
    join(distDir, `${clean}.html`),
    join(distDir, clean),
  ];
  return candidates.some((candidate) => existsSync(candidate));
}

const broken = [];
let checked = 0;

for await (const file of walk(distDir)) {
  if (!file.endsWith(".html")) continue;
  const html = await readFile(file, "utf8");
  const hrefs = [...html.matchAll(/href="([^"]+)"/g)].map((m) => m[1]);
  for (const href of hrefs) {
    if (!href.startsWith("/")) continue;
    if (href.startsWith("//")) continue;
    checked += 1;
    if (!targetExists(href)) {
      broken.push(`${file.replace(distDir, "dist")} -> ${href}`);
    }
  }
}

if (broken.length > 0) {
  console.error(`[check-links] 发现 ${broken.length} 个失效站内链接：`);
  for (const item of broken) console.error(`  ${item}`);
  process.exit(1);
}

console.log(`[check-links] 全部 ${checked} 个站内链接均有效`);
