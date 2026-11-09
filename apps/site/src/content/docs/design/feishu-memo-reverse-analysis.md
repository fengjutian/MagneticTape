---
title: "飞书妙记「录音」功能逆向分析"
description: "对飞书妙记录音链路的逆向分析报告与技术基线。"
sidebar:
  order: 14
  label: "飞书妙记「录音」功能逆向分析"
editLink: false
---
> 本页由仓库 `doc/飞书妙记录音-逆向分析.md` 自动同步，内容以主仓库为准。
> [在 GitHub 上查看源文件](https://github.com/fengjutian/MagneticTape/blob/main/doc/%E9%A3%9E%E4%B9%A6%E5%A6%99%E8%AE%B0%E5%BD%95%E9%9F%B3-%E9%80%86%E5%90%91%E5%88%86%E6%9E%90.md)

> 目标:用 Rust 复刻一套等价的录音 → 实时转写 → 妙记产出功能。
> 分析对象:本机飞书 Windows 客户端 `D:\Feishu\`(版本 `D:\Feishu\app\manifest.json`)。
> 所有结论均来自二进制字符串、PE 导出/导入表、前端 bundle 与生成的 API 契约文件。

---

## 0. 一句话结论

飞书妙记的录音是一条 **「ByteRTC 采集 → 混合 PCM16 → RTC 房间上行 → 云端 ASR/摘要」** 的链路。
**采集与编码完全可以自研(纯本地、可离线);实时转写与摘要必须依赖飞书云(需自建 ASR 替代)。**
飞书自研的增量价值主要在 `voixcap.dll` 的**设备路由/看门狗/断线补拉**工程细节,而不在算法。

---

## 1. 组件与职责

| 文件 | 大小 | 角色 |
|---|---|---|
| `app/voixcap.dll` | 2.2 MB | **录音引擎**。C++ 类 `voixcap::VoixcapRecorder`,内置 libopus/celt/silk |
| `app/web_minutes.dll` | 6.2 MB | **妙记业务模块**。驱动 `VoixcapRecorder`,承载 IPC handler 与状态机 |
| `app/VolcEngineRTC.dll` | — | **ByteRTC 引擎**。采集房间、混音、发布流 |
| `app/bv-screen-capture.dll` | 2.0 MB | 会议画面采集(**非**妙记录音路径) |
| `app/anker_minutes.dll` | 101.7 MB | Chromium 内核 + WebRTC 栈,提供桌面捕获等底层能力 |
| `webcontent/doubao-feishu/.../s2-feishu-audio-recorder-runtime.js` | 26 KB | 前端录音运行时,事件路由与状态机胶水层 |

源码路径残留(来自 PDB 路径字符串):

```
C:\103489\byteview-voixcap\src\codec\ogg_file_encoder.cc
..\..\aha\iron\web_minutes\utility\modules\voixcap_host.cc
..\..\aha\iron\web_minutes\utility\modules\worker\voixcap_worker.cc
..\..\aha\iron\web_minutes\utility\modules\handler\voixcap_handler.cc
```

`byteview` = 火山引擎 RTC SDK;`iron` = 飞书客户端的 `aha` 框架层(模块化 utility/worker/handler)。

---

## 2. `voixcap.dll` 的 ABI

### 2.1 导出表(11 个)

```
VoixcapGetVersion
VoixcapGetLastErrorMessage
VoixcapSetGlobalLogMessageCallback
VoixcapSetGlobalTeaReportCallback
?Create@VoixcapRecorder@voixcap@@SAPEAV12@PEAVVoixcapMainRunner@2@@Z
?Destroy@VoixcapRecorder@voixcap@@SAXPEAV12@@Z
??0VoixcapRecorder@voixcap@@QEAA@XZ          // ctor
??1VoixcapRecorder@voixcap@@UEAA@XZ          // dtor
??0VoixcapRecorder@voixcap@@QEAA@AEBV01@@Z    // move ctor
??4VoixcapRecorder@voixcap@@QEAAAEAV01@AEBV01@@Z  // move assign
??_7VoixcapRecorder@voixcap@@6B@              // vftable
```

`Create` 需要 `VoixcapMainRunner*`,说明有**主线程 / 工作线程分离**的任务模型。

### 2.2 导入表

- `VolcEngineRTC.dll` → `enableRescaleAudioVolume`(注意:是 `enableRescale...`,飞书写错了字面量)
- `KERNEL32.dll` → 155 项(标准 CRT + `DeviceIoControl`)

**关键结论:`voixcap.dll` 自身不导入 `WINMM`/`AVRT`/`mmdevapi`** —— 它**不直接碰 WASAPI**,
音频设备访问全部通过 ByteRTC(`VolcEngineRTC.dll`)完成。这大幅简化了 Rust 复刻的设备层复杂度。

### 2.3 `VoixcapRecorderImpl` 的 75 个方法(节选)

按职责归类:

**生命周期**
```
Initialize / Uninitialize
Start / Stop / Pause / Resume
Start, already recording, uuid:            // uuid = 录音会话 id
Start, no audio capture enabled, not start recording
Start, no audio record device, not enable mic audio capture
Start, system audio capture disabled by config
Stop: stop and destroy opus encoder
Stop: stop and destroy wav encoder
```

**设备管理**(这是飞书自研增量最大的部分)
```
GetCurrentMediaDeviceInfo / TrackCurrentMicrophoneDevice
HandleDefaultMicrophoneDeviceChanged
HandleDefaultMicrophoneDeviceChanged: default microphone changed from ...
HandleDefaultMicrophoneDeviceChanged: no audio capture device, stop mic capture
SetDevice / SetAutoSwitchDevice / SetSupportedVirtualAudioDevice
OnAudioDeviceStateChanged: detect system audio service restarted
HandleSystemAudioServiceRestart: restart rtc system audio capture
```

**看门狗 / 死锁保护**(飞书踩过的坑)
```
CheckAudioProcessTimeout: reset audio process watchdog after delayed check, check_gap_ms:
NotifyAudioProcessTimeout: stage: / NotifyAudioProcessRecovered: stage:
OnDeadLockError, block_session_id:
```

**RTC**
```
EnsureRTCAppId / JoinRTCRoom / LeaveRTCRoom
SetSubChannels / SetRemoteAudioPlaybackVolume / SetProxyConfig / ClearProxyConfig
SetServerParams
```

**编码**
```
OggFileEncoder::InitOpusEncoder / CreateAudioFile / EndWritePcmData
WavFileEncoder::CreateWavFile / ProcessAudioFrame / UpdateWavHeader / SaveAndCloseCurrentWavFile
AsyncAudioStreamEncoder::OnPCMFrameData / Start / Stop / Pause / Resume
```

### 2.4 内部类

```
voixcap::MainRunner                      // Singleton<MainRunner>
voixcap::AsyncAudioStreamEncoder         // 异步编码线程(worker thread)
voixcap::RTCAudioFrameObserver           // onCaptureMixedAudioFrame / onMixedAudioFrame
                                           // / onRecordAudioFrame / onRecordScreenAudioFrame
voixcap::RTCEngine                       // ByteRTC 封装
voixcap::RTCRoom                         // 房间/发布状态
voixcap::DeviceDispatcher / DeviceController  // 设备枚举与热插拔
voixcap::OggFileEncoder / WavFileEncoder
```

---

## 3. 音频参数(逆向核心)

全部来自 `voixcap.dll` 内嵌的 libopus 断言字符串与立即数扫描。

### 3.1 采集侧

```
kAudioFrameTypePCM16                                 // 采集设备回调格式 = PCM16
bytertc::AudioFrameCallbackMethod::kCaptureMixed     // 取「混合采集帧」(麦克风 + 系统音频)
RTCEngine::StartAudioFrameCallback: sample_rate: ...
onAudioDeviceAPIkAudioFrameTypePCM16
```

即:**ByteRTC 负责设备枚举与混音,回调给 voixcap 的是 PCM16 混合帧。**

### 3.2 RTC 上行帧

```
assertion failed: frame_length == 12 * 10      // = 120 采样点
assertion failed: pcm_count == frame_size
assertion failed: ret==frame_size-packet_frame_size
assertion failed: ret==packet_frame_size
```

`12 * 10 = 120`。在 48 kHz 下 → **120 / 48000 = 2.5 ms/包**。
这是 ByteRTC 的标准 10 ms 时隙 × 12 的打包粒度。

### 3.3 Opus 编码

```
assertion failed: st->mode == opus_custom_mode_create(48000, 960, NULL)
assertion failed: st->Fs == 48000 || 24000 || 16000 || 12000 || 8000
assertion failed: st->channels == 1 || st->channels == 2
OPUS_APPLICATION_VOIP (2048)  -> mov ecx x6
OPUS_APPLICATION_AUDIO (2049) -> mov ecx x2
```

| 参数 | 值 | 依据 |
|---|---|---|
| 采样率 | **48000 Hz** | `opus_custom_mode_create(48000, 960, NULL)` 断言 |
| 编码帧长 | **960 采样点 = 20 ms** | 同上 |
| 声道 | **1 或 2** | `st->channels == 1 \|\| st->channels == 2` |
| Application | **VOIP (2048) 为主**,AUDIO (2049) 为辅 | 立即数计数 6 : 2 |
| 容器 | **Ogg Opus** | `OpusHead`/`OpusTags`/`OggS`/`.ogg` 头 + `write_ogg_page_header` |
| 调试转储 | WAV 或 Opus | `WavFileEncoder` / `OggFileEncoder` 双路 |

`OPUS_APPLICATION_VOIP` + 48kHz/960(20ms) 是**典型语音会议参数**,而非音乐参数。

### 3.4 Opus 编码器控制调用

```
opus_encoder_create
opus_encoder_ctl failed:
opus_encoder_ctl OPUS_RESET_STATE failed:
opus_set_lsb_depth
opus_encoder init success.
opus_reset_state
opus_encode
```

未发现显式的 `OPUS_SET_BITRATE_REQUEST` 立即数 → **码率走 libopus 默认(VOIP 模式下自适应)**,
或由 `VOIXCAP_*` 运行时参数覆盖。

### 3.5 Ogg 容器结构(实测确定,非静态推断)

`voixcap.dll` 只留下 `ogg_file_encoder.cc` 这个**源码路径残骸**,没有函数体,所以容器细节
无法从二进制直接读出。以下规则是拿 **ffmpeg 生成的对照文件**(`ref_ffmpeg.ogg`)逐页比对
反推出来的,再用 ffprobe / ffmpeg 解码验证。

对照文件的页面结构(6 页):

```
page#0 flags=0x02(BOS) granule=0      body=19   1 packet  "OpusHead"
page#1 flags=0x00       granule=0      body=60   1 packet  "OpusTags"
page#2 flags=0x00       granule=48000  body=9992 50 packets
page#3 flags=0x00       granule=96000  body=10072 50 packets
page#4 flags=0x00       granule=144000 body=11249 50 packets
page#5 flags=0x04(EOS) granule=144312 body=318  1 packet
```

| 规则 | 结论 | 踩错后的现象 |
|---|---|---|
| CRC-32 多项式 | `0x04c11db7`,不反射、不异或 | `CRC mismatch!` |
| CRC 覆盖范围 | **整页 = 页头 + 页体** | 只算页头 → `CRC mismatch!` |
| CRC 字节序 | **小端** | 写成大端 → `CRC mismatch!` |
| lacing / segment table | **按 Opus 包边界生成** | 按页体总长生成 → `Error parsing the packet header` |
| 页边界 | 必须落在包之间 | 切在包中间 → `Error parsing the packet header` |
| `pre-skip` | = 编码器 lookahead(**VOIP@48k = 312**) | 写死 960 → 解码时长对不上 |
| 末页 granule | `内容采样点数 + pre_skip` | 少加 pre_skip → 尾部少 `pre_skip` 点 |
| 尾包 | **每个分片都要补一帧** | 不补 → 每个分片少 312 点(6.5 ms) |

**libopus 1.3 没有 flush API**:`opus_encode(st, NULL, 0, ...)` 必然返回 `OPUS_BAD_ARG`
—— 源码 `frame_size_select()` 对 `frame_size < Fs/400` 直接返回 `-1`。所以补尾的办法是
**再喂一帧静音**,让编码器把内部压着的最后 `pre_skip` 个采样点吐出来;该包带真实内容尾部
+ 填充,因此它**不推进 granule**,但要计入包数并排在最后一页。

**分片是独立流**:每个 `.ogg` 分片有自己的 `OpusHead`(因而有自己的 `pre-skip`)、自己的
末页 `EOS`、**自己的尾包**。分片切分必须由「编码器 + 写包器」的组合体来做,否则中间
分片会各丢 312 个采样点。

判据(外部可证伪):

```powershell
ffprobe -v error -show_format -show_streams chunk.ogg      # 无 CRC mismatch / packet header error
ffmpeg -v error -i chunk.ogg   -f s16le out.pcm             # 采样点数 == 源采样点数
ffmpeg -v error -i source.wav -f s16le src.pcm
```

实测(3 s / 48 kHz / mono / 440 Hz 测试音,单分片):

| 文件 | 包数 | 末页 granule | 解码采样点 | 440 Hz 幅度 |
|---|---|---|---|---|
| 源 WAV | — | — | 144000 | 7272.7 |
| ffmpeg 对照 | 151 | 144312 | **144000** | 7280.7 |
| 本实现 | 151 | 144312 | **144000** | 7284.0 |

实测(同源切成 3 个 1 s 分片,每片独立解码):

| 分片 | 包数 | 解码采样点 |
|---|---|---|
| chunk_00000 | 51 | 48000 |
| chunk_00001 | 51 | 48000 |
| chunk_00002 | 51 | 48000 |
| 合计 | — | **144000 = 源** |

---

## 4. IPC 契约(前端 ↔ native)

### 4.1 传输机制

`web_minutes.dll` 走 **Mojo IPC**(Chromium 风格),不是裸 named pipe:
```
mojo/data_pipe_consumer / mojo/data_pipe_producer / mojo/message_pipe
aha/components/backend/command_dispatcher.cc   // CommandDispatcher@backend
aha/components/backend/command_sender.cc / command_receiver.cc
```
前端通过 `window.neotix.audioRecorder` 拿到对象,每个方法是一次 `rfc-request` / `rfc-response` 往返
(见 `335656` 模块的 `class r{...baseCall...}`)。

### 4.2 契约文件

`webcontent/doubao-feishu/feishu/static/js/async/41427.84c7d60c25.js` 内嵌**机器可读的完整契约**
(`generatedAt: 2026-09-22`,`totalFiles: 102`),包含 namespace → methods → 参数/返回类型。
这是逆向的最优入口 —— 已提取为 `audio_recorder_contract.json`。

namespace `audioRecorder`,**41 个方法**:

```
// 窗口
createWindow / closeWindow / minimizeWindow / restoreWindow / toggleFullscreen
hideWindow / showWindow / setWindowSize / setMiniWindowSize / setWindowConstraint
isWindowExist
showTooltip / hideTooltip

// 对话框
showPromptDialog / closePromptDialog / getExistingPromptDialog
createSummaryDialog / updateSummaryDialog / closeSummaryDialog

// 合规
setCompliancePreference / onComplianceDialogResult

// 生命周期
start / finish
restartCoreAudioProcess

// 录音引擎核心
voixcapPreStartProcess
voixcapStartProcess
voixcapShutdownProcess
voixcapStart / voixcapStop / voixcapPause / voixcapResume
voixcapJoinRTCRoom / voixcapLeaveRTCRoom
voixcapEnsureRTCAppId
voixcapSetRTCRuntimeParameter
voixcapSetAudioContext
voixcapEnableSystemAudioCapture
voixcapSetSubChannels
voixcapSetTranslationMuted
voixcapGetSystemMediaDeviceInfo
voixcapGetUncompleteRecords
voixcapCleanAudioPiece

// 实验/监控
setLibraExpEnabled / setMeetingMonitorEnabled / getMeetingMonitorIgnoredThrottle
```

### 4.3 native 侧 IPC 通道名

```
web-minutes-voixcap-enable-system-audio-capture
web-minutes-voixcap-ensure-rtc-app-id
web-minutes-voixcap-get-system-media-device-info
web-minutes-voixcap-join-rtc-room
web-minutes-voixcap-leave-rtc-room
web-minutes-voixcap-set-audio-context
web-minutes-voixcap-set-auto-switch-device
web-minutes-voixcap-set-rtc-runtime-parameter
web-minutes-voixcap-set-sub-channels
web-minutes-voixcap-set-translation-muted
```
相关但不由前端直调:
```
web-minutes-initialize-voixcap / web-minutes-uninitialize-voixcap
web-minutes-voixcap-set-device
web-minutes-voixcap-start / web-minutes-voixcap-pause / web-minutes-voixcap-resume
web-minutes-update-auth-info
web-minutes-setup-context / web-minutes-setup-settings
web-minutes-update-media-server-settings
web-minutes-update-proxy-settings
web-minutes-update-speed-up-nodes
```

---

## 5. 录音状态机

### 5.1 前端事件枚举(模块 `10306`)

```js
u = {
  CreateWindow:      "audio_recorder_create_window",
  PermissionStatus:  "audio_recorder_permission_status",
  RequestPrepare:    "audio_recorder_request_prepare",
  AudioData:         "audio-data",
  AudioContentData:  "audio-content-data",
  AudioWindowState:  "audio-window-state",
  RecordingStarted:  "audio_recorder_started",
  ArtifactOptions:   "artifact-options",
  RecordProcessExit: "record_process_exit",
}
```
辅助枚举:
```
START_ERROR = 10000, PERMISSION_ERROR = 10001
Prepare = "prepare"
```

### 5.2 事件分类

```js
// 需要归属校验的事件(会产生状态副作用)
D = { CreateWindow, PermissionStatus, RequestPrepare, RecordProcessExit }
// 纯数据/瞬时事件
x = { AudioData, AudioContentData, AudioWindowState }
// 系统级透传事件
K = { system_suspend, system_resume,
      REQUEST_AUTH_HEADERS_FROM_BACKGROUND, SYNC_AUTH_HEADERS_FROM_BACKGROUND,
      voixcap_recorder_state_changed, VOLUME_UPDATE, SUBTITLE_UPDATE,
      voixcap_audio_state_changed, recorder_window_switch, STATE_CHANGED }
```

### 5.3 归属校验(防串音)

前端用 `uniId / audioQueryMessageId / audioRecordIdV2 / attemptId` 四元组做**归属校验**,
只有匹配的录音实例才消费事件:

```js
// 任何 C(e,t) 为真的事件都要过这道闸
if (!a) return;                         // a = 当前 audioQueryMessageId
if (n && n !== a) return;               // n = 事件里的 audioQueryMessageId
if (r && r !== a) return;               // r = 事件里的 uniId
if (o) return;                          // o = audioRecordIdV2 已存在(说明已归属)
if (s) return s === i.attemptId ? a : void 0;  // attemptId 不匹配则丢弃
```

### 5.4 录音窗口状态集

```js
R = new Set(["start", "resume", "pause", "finish"])
```
`AudioWindowState` 事件只在这 4 个状态下算「有效」。

---

## 6. 启动流程

### 6.1 前端编排

```
1. 用户点击 → handleMeetingRecordStart({createFrom: "action_bar" | "sidebar_skill_click"
                                            | "shortcut_start" | "tray_start" | ...})
2. 预检: getExistingPromptDialog()          // 权限/合规弹窗
3. 提交流水线: SubmitPipelineEngine.run(... {audioRecorderLaunchSnapshot})
4. 广播: "user-trigger-recording"            // 通知其它 tab
5. voixcapPreStartProcess({biz: "recorder" | "interpretation"})
   biz = abilityType === SKILL_SIMULTANEOUS_INTERPRETATION ? "interpretation" : "recorder"
6. native 侧按 biz 分流:
     "recorder"      → 纯录音(结束才出妙记)
     "interpretation" → 实时同传(边录边转写)
```

`createWindow` 的入参(契约文件原样):
```
meetingType, createFrom, doubaoBizInfo, uniId, biz,
spokenLanguage, translationLanguage, context
```

### 6.2 native 侧顺序(由 `VoixcapRecorderImpl` 方法集合推断)

```
1. Initialize(config, observer)      // 校验 runtime_params(rtc_app_id 非空等)
2. EnsureRTCAppId(app_id, rtm_param)  // app_id 变化则重建 engine
3. CreateRTCEngine → Initialize(app_id) → Login → SetBusinessId
4. SetServerParams                    // 媒体服务器地址
5. EnableMicAudioCapture / EnableSysAudioCapture   // 麦克风 + 系统音频
6. StartAudioFrameCallback(kCaptureMixed, PCM16)   // 拿混合帧
7. JoinRTCRoom(room_id, business_id, rtm_param)
8. EnableMicAudioPublish / EnableSysAudioPublish
9. Start(uuid)                        // 开始写 Ogg/WAV + 上行
```

---

## 7. 音频上行:两条路径

### 7.1 实时路径(RTC 上行)

```
inner_->publishStreamAudio(value)      // 麦克风 / 混合流
inner_->publishScreenAudio(value)      // 系统音频
```
即:**混合 PCM16 帧被直接交给 ByteRTC 房间发布**,不经过本地文件。
云端从 RTC 流里做 ASR。

相关状态回调:
```
RTCRoom::onAudioPublishStateChanged
RTCRoom::onScreenAudioPublishStateChanged
RTCRoom::onStreamSyncInfoReceived      // 流同步信息
RTCRoom::onUserJoined / onUserLeave
RTCRoom::onRoomStateChanged / onRoomStateChangedWithReason
RTCRoom::onNetworkQuality
```

### 7.2 落盘路径(本地留档)

```
OggFileEncoder  -> .ogg (Opus)
WavFileEncoder  -> .wav (PCM16)
```
两者都支持「分片」:`OggFileEncoder::WriteChunkMetadata` 写 meta 文件,
`VoixcapRecorderImpl::OnChunkFileReady: file_index:` / `OnChunkFileFailure, code:` 回调分片就绪。

### 7.3 断线补偿

```
voixcapGetUncompleteRecords() -> Promise<UncompleteRecord[]>
```
**断线期间未上传完成的片段会被本地留存,重连后补拉**。这是飞书在真实弱网环境下的关键设计,
也是 Rust 复刻时最容易被忽略的一环。

---

## 8. 运行时调试开关(官方自带,逆向的黄金入口)

`voixcap.dll` 内置环境变量覆盖(`RuntimeParams::OverrideFromEnv`):

| 变量 | 作用 |
|---|---|
| `VOIXCAP_ENABLE_OPUS_DUMP` | 导出 opus 流(调试) |
| `VOIXCAP_OPUS_AUDIO_OUTPUT_DIR` | opus dump 输出目录 |
| `VOIXCAP_OPUS_AUDIO_DUR_SEC` | opus dump 时长上限 |
| `VOIXCAP_ENABLE_WAV_DUMP` | 导出 PCM WAV |
| `VOIXCAP_WAV_AUDIO_OUTPUT_DIR` | wav dump 目录 |
| `VOIXCAP_WAV_AUDIO_DUR_SEC` | wav dump 时长上限 |
| `VOIXCAP_RTC_DUMP` | 导出 RTC 音频 dump |
| `VOIXCAP_RTC_DUMP_MODE` | dump 模式(`custom` / fallback `all`) |
| `VOIXCAP_RTC_DUMP_TYPE` | dump 类型筛选 |
| `VOIXCAP_RTC_DUMP_LENGTH_MIN` | dump 最小时长 |
| `VOIXCAP_MAC_VIRTUAL_AUDIO_DEVICE` | macOS 虚拟音频设备 |

其它运行时参数:
```
runtime_params: rtc_app_id, rtc_business_id, rtc_rtm_param,
                audio_sample_rate, enable_system_audio_capture,
                ogg_encoder_timeout_ms, enable_opus_dump, ...
RuntimeParams::IsValid: rtc_app_id is empty
RuntimeParams::IsValid: opus dump enabled but output dir is empty
```

> **实操价值**:设置 `VOIXCAP_ENABLE_WAV_DUMP=1` + `VOIXCAP_WAV_AUDIO_OUTPUT_DIR=<dir>`,
> 在飞书里点一次录音,即可拿到**与产品行为完全一致的 PCM 波形**,
> 可直接作为 Rust 实现的黄金测试向量。这是验证复刻正确性最可靠的路径。

---

## 9. 可自研 vs 必须依赖飞书云

| 能力 | 飞书实现 | Rust 可否自研 | 说明 |
|---|---|---|---|
| 麦克风采集 | ByteRTC | ✅ | WASAPI loopback / 默认设备 |
| 系统音频采集 | ByteRTC `EnableSysAudioCapture` | ✅ | WASAPI loopback 即可 |
| 混音 | ByteRTC `kCaptureMixed` | ✅ | 自己做 PCM 混合 |
| 设备热插拔/默认设备切换 | `DeviceDispatcher` + 通知 | ✅ | Windows IMMNotificationClient |
| 采集看门狗/死锁恢复 | `CheckAudioProcessTimeout` | ✅ | 纯本地逻辑 |
| 断线补拉 | `GetUncompleteRecords` | ✅ | 本地分片 + 重传 |
| Opus 编码 | libopus | ✅ | `opus` crate / `audiopus` |
| Ogg 封装 | `OggFileEncoder` | ✅ | `ogg` crate |
| 录音窗口 UI | Chromium + Mojo | ⚠️ | 需另选 UI 方案(Tauri/egui) |
| **实时 ASR** | 飞书云 RTC 流 | ❌ | **需自建 ASR**(火山/讯飞/OpenAI Whisper) |
| **实时翻译** | 飞书云 | ❌ | 需自建翻译服务 |
| **AI 总结/待办/章节** | 飞书云 | ❌ | 需自建 LLM 摘要 |
| **妙记存储** | 飞书云 | ❌ | 需自建存储 |
| RTC 上行协议 | ByteRTC 私有 | ⚠️ | 除非也用 ByteRTC,否则换 WebSocket/自建 |

**结论**:「录音」这个功能本体(采集 + 编码 + 留档 + 恢复)**完全可以纯 Rust 离线复刻**,
难度中等偏上,但飞书没有算法护城河,只有工程细节。
「妙记」的完整价值(转写 + 摘要)取决于 ASR/LLM,必须自己接或用第三方。

---

## 10. Rust 复刻建议架构

```
┌────────────────────────────────────────────────────────┐
│ capture/          WASAPI 采集(麦克风 + 系统音频 loopback)│
│                   -> PCM16 f32/i16 @ 48kHz              │
├────────────────────────────────────────────────────────┤
│ mixer/            混合 + 重采样 + 音量归一(对标 AGC)     │
├────────────────────────────────────────────────────────┤
│ vad/              静音检测(对标 voixcap 的 VAD 分段)     │
├────────────────────────────────────────────────────────┤
│ encoder/          Opus 48kHz/960/VOIP + Ogg 封装         │
│                   -> .ogg 分片(对标 ChunkFile)            │
├────────────────────────────────────────────────────────┤
│ transport/        自建上行(WebSocket/HTTP) 或接 ByteRTC  │
│                   + 断线补拉队列                         │
├────────────────────────────────────────────────────────┤
│ asr/              实时转写(第三方 ASR 流式)              │
│ summary/          摘要 / 待办 / 章节(LLM)                │
├────────────────────────────────────────────────────────┤
│ ui/               录音浮窗 + 实时字幕                    │
└────────────────────────────────────────────────────────┘
```

### Rust crate 选型

| 需求 | 推荐 crate | 备注 |
|---|---|---|
| 音频采集 | `cpal` | 跨平台封装 WASAPI/CoreAudio/ALSA |
| 系统音频 | `wasapi` + `WASAPILoopback` | 或 `cpal` 的 loopback 支持 |
| 重采样 | `rubato` | 48k 内部统一 |
| VAD | `webrtcvad` 或 `silero-vad`(ONNX) | 对标飞书 VAD |
| 编码 | `audiopus` | 绑定 libopus |
| 容器 | `ogg` | |
| 异步运行时 | `tokio` | |
| IPC/服务 | `axum` | 替代 Mojo |
| UI | `tauri` | 替代 Chromium;或 `egui` 更轻 |

---

## 11. 验证方法(复刻正确性)

1. **黄金向量**:用 `VOIXCAP_ENABLE_WAV_DUMP=1` 抓飞书自己录的 PCM,
   与 Rust 实现同场景录制的 PCM 做波形/时长/能量比对。
2. **编码一致性**:同一段 PCM 分别喂给 libopus,比对输出码流(参数一致时应**逐字节相同**)。
3. **时序**:对齐 `frame_length == 120` 的分片边界。
4. **恢复行为**:强杀进程后重启,验证未完成片段能被补拉。

---

## 12. 未解问题(需要动态分析)

- `rtm_param` 的具体 JSON 结构(ByteRTC RTM 信令参数)
- RTC 上行的鉴权 token 从哪来(`update-auth-info` 的载荷)
- `voixcap.json` 配置文件的内容(运行时从磁盘读)
- `SetSubChannels` 的「保留首项」语义细节
- `ArtifactOptions` 与最终妙记产物的字段映射

> 这些需要在运行时用 x64dbg 附加到 `web_minutes.dll` 断点观察,
> 或用 `VOIXCAP_RTC_DUMP` 抓包比对。

---

## 13. 开源项目 MeetingTape 借鉴分析

参考项目：[patrick-fu/meetingtape](https://github.com/patrick-fu/meetingtape)（MIT License）。

MeetingTape 是面向 macOS 14.2+ / Apple Silicon 的 Swift 原生会议录音工具，支持系统音频与麦克风分轨、实时字幕、本地或云端转写、资料库和 Markdown 导出。它与本项目目标高度相似，适合作为**产品架构和可靠性设计参考**，但其 CoreAudio/AVFoundation/SwiftUI 平台实现不能直接移植到 Windows/Rust。

### 13.1 最值得借鉴的设计

| MeetingTape 设计 | 对本项目的价值 | Rust/Windows 落地方式 |
|---|---|---|
| 麦克风、系统音频分轨采集 | 保留音源身份，可区分我方与对方；需要时再混合 | 两路 WASAPI capture，内部保持独立 PCM 流 |
| 公共录音时间轴 | 设备切换、休眠、单路中断时保持音频与字幕时间戳一致 | 单调时钟 + source/generation + timeline span |
| 单路失败降级 | 麦克风或系统音频失败时继续保存另一轨 | 每路独立健康状态、重试和故障区间 |
| 自动重建设备 | 对应飞书默认设备切换、音频服务重启与看门狗恢复 | `IMMNotificationClient` + capture supervisor |
| checkpoint 崩溃恢复 | 强杀或崩溃后恢复已提交音频，避免整场丢失 | manifest + committed extent + 原子替换 + `FlushFileBuffers` |
| 采集与实时字幕解耦 | 网络或 ASR 故障不阻塞实时音频线程 | 有界队列；采集回调只复制/投递数据 |
| ASR provider 抽象 | 可并列接入豆包、Qwen、MOSS、Whisper 等 | 统一 `Transcriber` trait 和持久化任务模型 |
| 分来源、分模型保存结果 | 单路或单模型失败不丢弃其他成功结果 | session/source/provider 三级任务标识 |
| 实时稿与录后稿分离 | 避免低延迟临时结果覆盖高质量最终结果 | transcript version + primary selection |
| Markdown 导出契约 | 保留时间戳、音源、多模型版本和运行统计 | 独立 exporter，不耦合 UI |
| WebRTC Audio Processing | AEC/NS/AGC 比简单 PCM 相加更适合会议场景 | Rust FFI 接 WebRTC APM，或先以可替换接口隔离 |

关键参考文件：

- [`docs/adr/0003-capture-scope-and-automatic-recovery.md`](https://github.com/patrick-fu/meetingtape/blob/main/docs/adr/0003-capture-scope-and-automatic-recovery.md)：设备变化、休眠、单源故障和崩溃恢复策略。
- [`Sources/MeetingTapeCore/AudioCapture.swift`](https://github.com/patrick-fu/meetingtape/blob/main/Sources/MeetingTapeCore/AudioCapture.swift)：双源采集、健康检查和自动重建。
- [`Sources/MeetingTapeCore/CaptureTimeline.swift`](https://github.com/patrick-fu/meetingtape/blob/main/Sources/MeetingTapeCore/CaptureTimeline.swift)：不同音源映射到公共时间轴。
- [`Sources/MeetingTapeCore/RecoveryStore.swift`](https://github.com/patrick-fu/meetingtape/blob/main/Sources/MeetingTapeCore/RecoveryStore.swift)：manifest、checkpoint、同步落盘、临时文件和原子发布。
- [`docs/transcription.md`](https://github.com/patrick-fu/meetingtape/blob/main/docs/transcription.md)：录后转写、实时字幕、断线、积压限制、任务恢复和凭据保护。
- [`docs/research/apm-integration.md`](https://github.com/patrick-fu/meetingtape/blob/main/docs/research/apm-integration.md)：WebRTC APM 集成经验。
- [`docs/specs/markdown-export-contract.md`](https://github.com/patrick-fu/meetingtape/blob/main/docs/specs/markdown-export-contract.md)：转写结果导出契约。

### 13.2 不宜直接移植的部分

- `CaptureSupport.m` 基于 CoreAudio；Windows 应重新实现为 WASAPI 麦克风采集和 loopback。
- MeetingTape 使用 CAF 临时 PCM 和最终 M4A；本项目应延续 Ogg Opus 独立分片方案。
- SwiftUI、Apple 本地翻译、macOS 权限和休眠机制均为平台专用实现。
- MeetingTape 的连续 float32 PCM 恢复文件空间占用较大（文档估算双声道 48 kHz 每小时约 1.38 GB）；不适合作为本项目默认存储方式。
- 其代码结构可以转译为 Rust 领域模型，但不应为复用 Swift 代码而引入跨语言运行时。

### 13.3 建议采用的组合架构

飞书逆向结果提供目标行为，MeetingTape 提供公开的可靠性工程范例。建议组合两者：

```text
WASAPI 麦克风 ─┐
               ├─ CaptureTimeline ─┬─ Mixer/APM ── 实时 ASR 队列
WASAPI Loopback┘                    ├─ 独立 Ogg/Opus 分片
                                    └─ Recovery manifest/checkpoint
```

具体约束：

1. 麦克风和系统音频在内部始终保持两路，混合流只是派生结果。
2. 所有音频块携带统一单调时钟、音源、采样范围和 capture generation。
3. 设备不可用和系统休眠以 timeline gap/span 表达，不在恢复文件中实际写入长段静音。
4. 每个 Ogg 分片是独立合法流，具有自己的 `OpusHead`、`pre-skip`、尾包和 `EOS`。
5. manifest 只确认已经完整写入并同步到磁盘的分片；使用临时文件加原子重命名发布。
6. 实时 ASR 只消费已提交的 PCM 块或分片，不持有采集锁，不在音频回调中进行网络操作。
7. ASR 断网、超时或积压超过阈值时停止或降级字幕，但录音必须继续。
8. 实时稿、录后高质量稿、翻译稿和摘要分别保存，通过显式字段选择主版本，不互相覆盖。
9. 单音源、单 ASR provider 的失败独立记录和重试，不重跑已经成功的任务。
10. 正常停止时先完成编码尾包和最终 checkpoint，再异步执行转写、摘要与导出。

### 13.4 结论

MeetingTape 可以作为本项目的**第二份设计规格**：重点借鉴公共时间轴、故障降级、设备重建、崩溃恢复、任务持久化和字幕解耦；底层采集、容器编码、UI 与平台生命周期仍按 Windows/Rust 独立实现。许可证为 MIT，若复制或改写其具体代码，应在分发物中保留相应版权和许可证声明。
