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
