---
title: "对标飞书妙记差距分析"
description: "与飞书妙记在录音内核、产品层与桌面集成上的逐项差距量化。"
sidebar:
  order: 13
  label: "对标飞书妙记差距分析"
editLink: false
---
> 本页由仓库 `doc/对标飞书妙记差距分析.md` 自动同步，内容以主仓库为准。
> [在 GitHub 上查看源文件](https://github.com/fengjutian/MagneticTape/blob/main/doc/%E5%AF%B9%E6%A0%87%E9%A3%9E%E4%B9%A6%E5%A6%99%E8%AE%B0%E5%B7%AE%E8%B7%9D%E5%88%86%E6%9E%90.md)

> 状态：差距盘点完成，未实现（2026-11-08）
> 适用范围：`apps/desktop`、`crates/audio`、`crates/core`
> 基线：[飞书妙记「录音」功能逆向分析报告](/design/feishu-memo-reverse-analysis/)（2026-11-06 完成）
> 说明：本文把逆向报告的规格逐项映射到当前代码，给出**已达成 / 部分达成 / 未达成**判定与量化差距。
> 本文所有代码判定均于 2026-11-08 通过实际源码核对，引用格式为 `文件:行号`。

## 1. 一句话结论

**录音内核（采集 / 编码 / 留档 / 恢复）已经基本追平飞书，差距约 10–15%；但「妙记」产品层差距约 45–55%。**

差距不在算法——飞书没有算法护城河（逆向报告第 0 节原话），而在于：
1. **实时链路形态不同**：飞书走 RTC 房间上行（流式、无分片延迟），本项目走 5 秒分片轮询；
2. **产品化外壳缺失**：托盘 / 全局快捷键 / 浮窗 / 单实例 / 退出确认一项都没有；
3. **说话人分离缺失**：飞书云端 ASR 自带分离能力，本项目本地引擎完全没有；
4. **弱网补偿缺失**：飞书的「断线补拉」是逆向报告点名的关键设计，本项目只有崩溃恢复，没有断线补拉。

## 2. 能力矩阵总表

图例：✅ 已达成 · ◐ 部分达成 · ❌ 未达成

| # | 能力 | 飞书实现 | 本项目状态 | 判定 | 证据 |
|---|---|---|---|---|---|
| 1 | 麦克风采集 | ByteRTC | WASAPI via cpal | ✅ | `crates/audio/src/recording.rs:206` |
| 2 | 系统音频采集 | ByteRTC `EnableSysAudioCapture` | WASAPI loopback | ✅ | `crates/audio/src/recording.rs:206` |
| 3 | 双轨分离保留 | 独立 mic/sys | mic/sys 独立分片 | ✅ | `crates/audio/src/opus.rs:47` `Track` 枚举 |
| 4 | 混音 | ByteRTC `kCaptureMixed` | 自行 PCM 混合 | ✅ | `recording.rs:156` `drain_realtime_pcm` |
| 5 | 采样率 48 kHz | `opus_custom_mode_create(48000, 960)` | `OPUS_SAMPLE_RATE = 48_000` | ✅ | `opus.rs:22` |
| 6 | Opus 编码 | libopus, VOIP 为主 | libopus, **已改为 Application::Voip** | ✅ | `opus.rs:136`、CAP-1 已完成 |
| 7 | Ogg 封装 / CRC / pre-skip | 自研 OggFileEncoder | 完整实现，含 312 pre-skip | ✅ | `opus.rs:239`、逆文 3.5 逐条对齐 |
| 8 | 分片独立流 | 每片独立 OpusHead + EOS | 同 | ✅ | `opus.rs:326` `ensure_chunk` |
| 9 | 尾包补静音 | 每片补一帧 | 同 | ✅ | `opus.rs:295` 注释明确 |
| 10 | 设备看门狗 / 死锁恢复 | `CheckAudioProcessTimeout` | 3 秒健康轮询 + `rebuild()` | ✅ | `recording.rs:308`、`main.ts:653` |
| 11 | 设备热插拔 / 默认设备切换 | `DeviceDispatcher` + `HandleDefaultMicrophoneDeviceChanged` | 仅设备掉线重建，**无默认设备切换跟随** | ◐ | `recording.rs:308` `rebuild` 需外部传 id |
| 12 | 崩溃恢复 | manifest + checkpoint | 完整实现 | ✅ | `crates/core/src/recovery.rs` |
| 13 | **断线补拉** | `GetUncompleteRecords` | **无** | ❌ | 逆向报告 7.3 点名「最容易被忽略的一环」 |
| 14 | **RTC 流式上行** | 混合 PCM 直推 RTC 房间 | **5 秒分片轮询** | ❌ | `main.ts:283` 250ms 轮询 + 分片 |
| 15 | 实时 ASR | 云端 RTC 流 ASR | 云端分片 ASR + 本地 whisper | ◐ | `components.ts:121` |
| 16 | 实时字幕 | SUBTITLE_UPDATE 事件 | 有，5 秒增量 | ✅ | `main.ts:282` `pollLive` |
| 17 | 实时翻译（同传） | 云端 | 有，增量翻译 | ✅ | `main.ts:66` `liveTranslationGeneration` |
| 18 | **说话人分离** | 云端 ASR 自带 | **无** | ❌ | 规划 DIAR-1 |
| 19 | AI 摘要 / 纪要 | 云端 LLM | 有，map-reduce | ✅ | `ai.rs` + `main.ts:1204` |
| 20 | 待办 / 章节结构化 | 云端 | **仅自由文本 Markdown**，无字段化待办/章节 | ◐ | `components.ts:296` textarea |
| 21 | 妙记存储 | 飞书云 | 本地文件库 | ✅（本地化替代） | README 历史库 |
| 22 | 导出 | 妙记分享链接 | TXT/SRT/VTT/MD/混音 WAV | ✅ | `src-tauri/src/export.rs` |
| 23 | **录音浮窗 / 小窗** | `createWindow` / `setMiniWindowSize` | **无**（单窗口） | ❌ | 逆文 4.2 窗口方法族 |
| 24 | **托盘** | `tray_start` 入口 | **无** | ❌ | 无 tray 相关依赖 |
| 25 | **全局快捷键** | `shortcut_start` | **无** | ❌ | `Cargo.toml` 无 `global-shortcut` |
| 26 | **单实例** | 重复启动聚焦已有窗口 | **无** | ❌ | 无 `single-instance` plugin |
| 27 | **录音中退出确认** | 二次确认弹窗 | **无** | ❌ | 逆文 6.1 预检流程 |
| 28 | **合规 / 权限预检弹窗** | `getExistingPromptDialog` / `setCompliancePreference` | **无** | ❌ | 逆文 4.2 合规方法族 |
| 29 | 多录音实例归属校验 | `uniId / audioQueryMessageId / audioRecordIdV2 / attemptId` 四元组 | **无**（单实例模型） | ❌ | 逆文 5.3 |
| 30 | 窗口状态集 | `start/resume/pause/finish` 四态 | 有对应状态机 | ✅ | `components.ts` 状态渲染 |
| 31 | 本地 VAD | 采集侧分段 | **无** | ❌ | 规划 WHISPER-3 |
| 32 | APM（降噪/回声消除） | ByteRTC 内置 | **无**，裸 PCM 混合 | ❌ | 规划 UX-6 |
| 33 | 代码签名 | 飞书自有证书 | 无 | ❌ | 规划 REL-1 |
| 34 | 自动更新 | 飞书自有 | 无 | ❌ | 规划 REL-2 |

**统计**：✅ 17 项 · ◐ 4 项 · ❌ 13 项。

## 3. 录音内核：已追平的部分

这一层是逆向报告第 9 节判定「完全可以纯 Rust 离线复刻」的部分，**当前实现覆盖度最高**。

### 3.1 编码一致性

逆文 3.5 列了 7 条 Ogg 封装规则，每一条踩错都有明确症状。当前实现逐条对齐：

| 规则 | 逆文症状 | 本项目 | 判定 |
|---|---|---|---|
| CRC-32 `0x04c11db7` 不反射不异或 | `CRC mismatch!` | 已实现 | ✅ |
| CRC 覆盖整页（页头+页体） | `CRC mismatch!` | 已实现 | ✅ |
| CRC 小端 | `CRC mismatch!` | 已实现 | ✅ |
| lacing 按 Opus 包边界 | `Error parsing the packet header` | 已实现 | ✅ |
| 页边界落在包之间 | `Error parsing the packet header` | 已实现 | ✅ |
| `pre-skip` = 编码器 lookahead | 解码时长对不上 | **312**（VOIP@48k 实测值） | ✅ |
| 末页 granule = 内容点 + pre_skip | 尾部少 pre_skip 点 | `opus.rs:315` | ✅ |
| 每片补尾帧 | 每片少 312 点 | `opus.rs:295` | ✅ |

> 注：`pre_skip` 取 312（`opus.rs:239` 构造期占位，真实值来自 `Encoder::lookahead()`）。
> **实测结论（2026-11-08）**：`Application::Audio` 与 `Application::Voip` 在 48 kHz mono 下的 lookahead **都是 312**，
> 码流大小与实际码率也几乎一致（3 秒音：12,193 B vs 12,171 B，均约 32.5 kbps）。
> 因此切换 VOIP 模式**不会改变 Ogg 封装行为**，收益来自 VOIP 对语音频段的带宽分配与噪声底优化，而非修正 pre-skip。

### 3.2 分片与恢复

- 分片是独立流（各有 `OpusHead`、末页 EOS、尾包）——`opus.rs:326`
- 侧车元数据原子提交（临时文件 + rename）——`opus.rs:189` `write_new_atomic`
- 崩溃恢复用 manifest + checkpoint，只增不改——`crates/core/src/recovery.rs`
- `decode_ogg_opus` 解码后按 committed frame count 截断，避免尾包泄漏进时间轴——`crates/audio/src/decode.rs:29`

这套「只增不改 + 原子发布」的设计与 MeetingTape 的 RecoveryStore（逆文 13.1）理念一致，说明前期架构选型做对了。

## 4. 关键差距（按价值排序）

### 4.1 差距一：实时链路形态 ❌（最大差距）

**飞书**：混合 PCM16 帧**直接**交给 ByteRTC 房间发布，不经过本地文件。云端从 RTC 流做 ASR。所以飞书的实时字幕延迟由网络 RTT 决定，与录音分片无关。

**本项目**：录音分 5 秒一片落盘 → 前端 250ms 轮询已提交分片 → 送 ASR（`main.ts:283`、`:297`）。

**后果**：
- 实时字幕固有延迟 ≥ 5 秒（分片周期）而非飞书的亚秒级；
- 分片边界断词/重复——这正是规划里 WHISPER-3 要解决的问题；
- 弱网下轮询会积压。

**能不能追平？** 不能完全追平，但可以压缩：
- **滚动窗口推理**（上一片+当前片）把有效延迟压到单片级别；
- **VAD 前端**跳过静音片，直接减少幻觉与无效推理；
- **减少分片周期**（5 秒 → 2 秒）代价是分片数×2.5、请求数×2.5。

**代价对比**：飞书为了流式放弃了本地离线（必须连飞书云），本项目用「延迟换离线」。这是**有意的产品取舍**，不是单纯落后。若目标是「离线可用」，当前形态是对的；若目标是「实时同传的体验」，则需要引入真正的流式 ASR（OpenAI Realtime 已经是这个形态，`components.ts:121` 里有 `openai_realtime` provider，但那是云端流式，不是本地）。

### 4.2 差距二：断线补拉 ❌

逆文 7.3 明确点名这是「飞书在真实弱网环境下的关键设计，也是 Rust 复刻时最容易被忽略的一环」。

**飞书**：`voixcapGetUncompleteRecords()` 返回断线期间未上传完成的片段，重连后补拉。

**本项目现状**：
- 有**崩溃恢复**（进程重启后从 checkpoint 恢复）——`crates/core/src/recovery.rs`；
- 但没有**断线补拉**（进程活着、网络断了、期间产生的音频没送出去）。

**差距性质**：这不是崩溃恢复能覆盖的场景。录音继续进行但转写链路断了，本地有音频、有分片，但没有「重试队列」把这批漏掉的补送。

**实现难度**：低。项目已有 `RecoveryManifest` + 分片元数据，缺的只是一个「哪些分片尚未被云端确认」的账本 + 重试循环。**建议归入 WHISPER-2（下载/重试机制）或独立成项**，估 1–2 天。

### 4.3 差距三：说话人分离 ❌

飞书妙记的转写里带说话人（云端 ASR 提供），本项目完全没有。

这条差距**横跨两个维度**：
- 技术：规划 DIAR-1 未开工，路线已收敛为 sherpa-rs（见 [端侧模型选型调研](/design/on-device-model-research/)）；
- 产品：UI 没有说话人标签、没有重命名、没有按说话人筛选。

注意逆文第 7 节和规划第 7 节的共同边界：**不要用「麦克风/系统」轨道冒充说话人分离**。本项目双轨是「我方麦克风」vs「对方系统声」，不是说话人。

### 4.4 差距四：桌面集成外壳 ❌（13 项里占 6 项）

飞书有 41 个 IPC 方法，其中一大类是窗口与桌面集成：`createWindow` / `closeWindow` / `minimizeWindow` / `restoreWindow` / `toggleFullscreen` / `hideWindow` / `showWindow` / `setWindowSize` / `setMiniWindowSize` / `setWindowConstraint` / `showTooltip` / `hideTooltip`。

对应到本项目（`Cargo.toml` 只有 `tauri` + `protocol-asset`）：

| 能力 | 缺失后果 | 对应飞书方法 |
|---|---|---|
| 托盘 | 录音中最小化到托盘、后台常驻 | `tray_start` |
| 全局快捷键 | 不切窗口就能开始/停止录音 | `shortcut_start` |
| 单实例 | 重复启动产生两个实例，双份录音 | — |
| 录音浮窗 / 小窗 | 全屏会议时需要一个小的悬浮录音窗 | `setMiniWindowSize` |
| 录音中退出确认 | 误关窗口丢掉整场会议 | `showPromptDialog` |
| 合规 / 权限预检 | 开录前告知录音用途 | `setCompliancePreference` |

这一组**都是低风险、无外部依赖**的 Tauri 插件工作（`tauri-plugin-global-shortcut`、`tauri-plugin-single-instance`、`tauri-plugin-autostart`、托盘 API 均为官方插件），且直接决定「能不能在真实会议场景里顺手用」。规划里归在 UX-4（估 2–3 天），但**价值密度高于其优先级**，因为它是「录音中不丢数据」的兜底。

### 4.5 差距五：音频处理质量 ❌

| 项 | 飞书 | 本项目 |
|---|---|---|
| AEC / 降噪 / AGC | ByteRTC 内置 APM | 无，裸 PCM 混合 |
| 采集侧 VAD | 有 | 无 |

混音是简单 PCM 相加，两路信号时延不一致时会相位抵消；飞书走 WebRTC APM 链路。规划里对应 UX-6（增益/降噪/AEC，估 5–10 天）。

**影响面**：AEC 缺失在「笔记本麦克风 + 扬声器外放」场景下会明显影响 ASR 准确率——这是最常见的会议场景。**这条对转写质量的影响可能大于说话人分离**。

### 4.6 差距六：待办 / 章节未结构化 ◐

飞书妙记的产物是结构化的（章节、待办、摘要）。本项目只有自由文本 Markdown 笔记 + AI 生成的纪要文本。

当前 `components.ts:296` 是一个 `<textarea class="meeting-notes">`，用户手打或让 AI 生成 Markdown，没有「待办项列表」「章节树」这类字段化对象。

**影响**：UX-2（转写编辑、说话人重命名、分段合并）依赖说话人字段，也在此列。

## 5. 差距量化汇总

| 维度 | 达成度 | 说明 |
|---|---|---|
| 录音内核（采集/编码/留档/恢复） | **85–90%** | Opus 参数有一处待实测（见 6.1），断线补拉缺失 |
| 实时转写链路 | **50–60%** | 形态不同（分片轮询 vs RTC 流式），功能齐但延迟高 |
| 妙记产品层（摘要/待办/结构化产物） | **40–50%** | 摘要有、待办与章节未结构化 |
| 桌面集成与用户体验 | **10–20%** | 13 项缺失占 6 项 |
| 音频处理质量 | **20–30%** | 无 APM、无 VAD |
| 语音增强（说话人分离） | **0%** | 未开工 |
| 发布工程（签名/更新） | **0%** | 依赖外部证书 |
| **整体** | **45–55%** | 录音能用，妙记不够用 |

## 6. 建议的行动顺序

### 6.1 先做「验证 + 补漏」（1–2 天）

1. **Opus Application 参数对齐**（`opus.rs:136`）✅ **已完成 2026-11-08**
   - 改为 `Application::Voip`，与飞书的 `OPUS_APPLICATION_VOIP` 一致（逆文 3.3，立即数计数 6:2）。
   - **实测修正**：原判断「`pre_skip = 312` 是 VOIP 的 lookahead、与 AUDIO 编码器不匹配」**不成立**——两种模式 lookahead 都是 312，码流与码率也几乎一致。收益来源修正为 **VOIP 对语音频段的带宽分配与噪声底优化**，切换不影响 Ogg 封装。
   - 已加回归测试 `encoder_uses_voip_mode_with_the_expected_lookahead` 锁定 VOIP + pre_skip == 312。
   - 验收：`cargo fmt --check` ✅ / `cargo clippy --workspace --all-targets -D warnings` ✅ / `cargo test --workspace --all-targets` ✅（桌面端 34 passed + 2 ignored，crate 12 passed）。
   - **仍缺**：黄金向量比对（逆文 11 节：用 `VOIXCAP_ENABLE_WAV_DUMP=1` 抓飞书自己的 PCM 做逐字节码流比对）需要飞书客户端环境，本机未做。

2. **断线补拉**（1–2 天）
   - 在 `RecoveryManifest` 之上加「云端已确认分片」账本 + 重试队列；
   - 验收：拔网线录音 10 分钟，恢复后分片全部补送，最终转写无缺口。

### 6.2 再做「桌面集成」（2–3 天，UX-4 提前）

托盘 + 全局快捷键 + 单实例 + 录音中退出确认。这四项都是官方 Tauri 插件，无外部依赖，直接对应飞书的 `tray_start` / `shortcut_start` / `createWindow` 方法族。

**理由**：不是「飞书有我们没有」，而是「录音场景的实际需求」——会议中不方便切窗口，误关窗口等于整场丢失。

### 6.3 顺带补上（并入既有项）

| 项 | 并入 | 说明 |
|---|---|---|
| 本地 VAD | WHISPER-3 | 与 DIAR-1 共用 sherpa-rs 依赖 |
| 说话人分离 | DIAR-1 | 路线已收敛，见选型调研 |
| 采集侧 APM | UX-6 | 对转写质量影响可能大于说话人分离 |
| 待办/章节结构化 | UX-3 / 新增 | 等说话人字段就绪后一起做 |

### 6.4 关于实时链路的取舍（需要决策）

**这是个产品决策，不是技术债**：

| 方案 | 延迟 | 离线 | 成本 |
|---|---|---|---|
| 维持分片轮询（当前） | 5 秒级 | ✅ | 低 |
| 滚动窗口 + VAD | ~2–3 秒 | ✅ | 中 |
| 分片周期降到 2 秒 | ~2 秒 | ✅ | 请求数 ×2.5 |
| 引入流式 ASR | 亚秒 | ❌ | 需云端或自建流式引擎 |

**建议**：先做滚动窗口 + VAD（WHISPER-3），把延迟压到 2–3 秒且保持离线。若实测仍不满意，再评估 2 秒分片。**不建议**为此引入 RTC——那是飞书为了用自家云 ASR 而付出的架构代价，本项目走的是相反路线（本地优先），不该抄它的传输层。

## 7. 飞书自身未解 / 不必追平的部分

以下不是差距，是不同取舍：

- **RTC 上行协议**（逆文 9 节标 ⚠️）：私有协议，除非也用 ByteRTC，否则换 WebSocket/自建。本项目用「本地落盘 + 拉取」，等价且更简单。
- **ByteRTC 引擎**：飞书用它做设备枚举与混音。本项目用 cpal + WASAPI 直接做，功能等价。
- **`rtm_param` / `update-auth-info` 载荷**（逆文 12 节未解）：与云 RTC 强绑定，本项目不需要。
- **妙记云存储与分享链接**：本项目走本地文件库 + 本地导出，符合「本地优先」定位。

## 8. 引用来源

- 规格基线：[doc/飞书妙记录音-逆向分析.md](/design/feishu-memo-reverse-analysis/)
- 端侧模型选型：[doc/端侧模型选型调研.md](/design/on-device-model-research/)
- 路线图：[doc/功能规划.md](/design/feature-roadmap/)
- 飞书妙记录音运行时契约（逆文引用的 `audio_recorder_contract.json`，41 个方法）——逆向阶段提取，未纳入本仓库
