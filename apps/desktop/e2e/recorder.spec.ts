import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(page.locator(".status-title")).toHaveText("等待开始录音");
});

test("records, pauses, resumes and stops into the workbench", async ({ page }) => {
  await page.locator(".start-button").click();
  await expect(page.locator(".status-title")).toHaveText("正在录音");

  await page.locator(".pause-button").click();
  await expect(page.locator(".status-title")).toHaveText("录音已暂停");

  await page.locator(".pause-button").click();
  await expect(page.locator(".status-title")).toHaveText("正在录音");

  await page.locator(".stop-button").click();
  await expect(page.locator(".status-title")).toHaveText("录音已保存");
  await expect(page.locator(".playback")).toBeVisible();
  await expect(page.locator(".transcript-panel")).toBeVisible();
});

test("transcribes the recording and exports meeting material", async ({ page }) => {
  await page.locator(".start-button").click();
  await page.locator(".stop-button").click();
  await expect(page.locator(".transcribe")).toBeVisible();

  await page.locator(".transcribe").click();
  await expect(page.locator(".transcript-line")).toHaveCount(2);
  await expect(page.locator(".transcript-line").first()).toContainText("大家好");
  await expect(page.locator(".transcript-state")).toContainText("E2E-ASR");

  await page.locator(".export-session").click();
  await expect(page.locator(".export-state")).toContainText("已导出 3 个文件");
});

test("opens the local Whisper settings panel", async ({ page }) => {
  await page.locator(".start-button").click();
  await page.locator(".stop-button").click();
  await page.locator(".asr-settings").click();

  await expect(page.locator(".asr-dialog")).toBeVisible();
  await page.locator(".asr-dialog-provider").selectOption("local_whisper");
  await expect(page.locator(".local-whisper")).toBeVisible();
  await expect(page.locator(".local-whisper-status")).toContainText("GPU");
  await expect(page.locator(".local-whisper-status")).toContainText("CPU");
});

test("moves between transcript, analysis, tasks and chat without losing content", async ({ page }) => {
  await page.locator(".start-button").click();
  await page.locator(".stop-button").click();
  await page.locator(".transcribe").click();
  await expect(page.locator(".transcript-line")).toHaveCount(2);

  await page.locator('.workspace-tab[data-tab="analysis"]').click();
  await page.locator(".meeting-notes").fill("- [ ] 制定技术方案\n- [x] 评估技术方案");
  await page.locator(".save-notes").click();
  await expect(page.locator(".insight-todos-list")).toContainText("评估技术方案");

  await page.locator('.workspace-tab[data-tab="tasks"]').click();
  await expect(page.locator(".tasks-content")).toContainText("制定技术方案");
  await expect(page.locator(".tasks-content")).toContainText("已完成");

  await page.locator('.workspace-tab[data-tab="chat"]').click();
  await page.locator(".workspace-chat-form .chat-input").fill("刚才讨论的技术问题是什么？");
  await page.locator(".workspace-chat-form .chat-send").click();
  await expect(page.locator(".workspace-chat-thread")).toContainText("E2E 纪要");
  await expect(page.locator(".chat-thread").first()).toContainText("E2E 纪要");

  await page.locator('.workspace-tab[data-tab="transcript"]').click();
  await expect(page.locator(".transcript-line")).toHaveCount(2);
});

test("opens settings and the knowledge placeholder", async ({ page }) => {
  await page.locator(".settings-button").click();
  await expect(page.locator(".settings-dialog")).toBeVisible();
  await page.locator(".settings-open-ai").click();
  await expect(page.locator(".ai-dialog")).toBeVisible();
  await page.keyboard.press("Escape");

  await page.locator(".knowledge-button").click();
  await expect(page.locator(".knowledge-dialog")).toBeVisible();
  await expect(page.locator(".knowledge-dialog")).toContainText("知识沉淀即将上线");
});

test("exposes the insight drawer at the minimum window width", async ({ page }) => {
  await page.setViewportSize({ width: 960, height: 640 });
  await expect(page.locator(".insight-column")).not.toBeInViewport();
  await page.locator(".insight-drawer-toggle").click();
  await expect(page.locator(".insight-column")).toBeInViewport();
});
