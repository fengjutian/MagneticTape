import { chromium } from "@playwright/test";

const browser = await chromium.launch({ channel: "msedge", headless: true });
const page = await browser.newPage({ viewport: { width: 1220, height: 900 }, deviceScaleFactor: 1 });
const errors = [];
page.on("console", (message) => {
  if (message.type() === "error") errors.push(message.text());
});
page.on("pageerror", (error) => errors.push(`pageerror: ${error.message}`));
await page.goto("http://127.0.0.1:4174/", { waitUntil: "networkidle" });
await page.waitForTimeout(800);
await page.screenshot({ path: "../../doc/workspace-1220.png" });
await page.setViewportSize({ width: 1440, height: 900 });
await page.waitForTimeout(300);
await page.screenshot({ path: "../../doc/workspace-1440.png" });
await page.setViewportSize({ width: 960, height: 640 });
await page.waitForTimeout(300);
await page.screenshot({ path: "../../doc/workspace-960.png" });
console.log(JSON.stringify({ errors }, null, 2));
await browser.close();
