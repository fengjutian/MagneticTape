# 本地 Whisper 转写需求

> 状态：v1 开发中（本文档同时作为验收依据）
> 适用范围：`apps/desktop` Tauri 桌面端（Windows 10/11 x64）
> 更新日期：2026-11-06

## 1. 背景与目标

MagneticTape 当前只能通过 MiniMax 或 SiliconFlow 的云端 ASR 转写，必须具备网络、账号和 API Key。对「本地优先」的会议录音工具，这意味着：

- 无网络或企业内网环境无法转写；
- 会议音频必须上传第三方，存在合规与隐私顾虑；
- 转写按量计费，长会议成本不可控。

本需求引入**本地 Whisper**：在本机运行 whisper.cpp 推理，音频不离开设备，转写免费，且与现有云端 provider 并存，由用户选择。

### 1.1 本期范围（v1）

- 本地推理引擎与模型的**按需下载**、校验、删除与状态展示。
- 录后转写（完成页、历史会话）走本地 Whisper。
- 与现有转写管线完全复用：分片解码 → 双轨混音 → 16 kHz 分段 → 结果写 `transcript.json` / `transcript.md`。
- 转写进度与取消沿用现有机制。
- 失败时给出可操作的中文错误信息。

### 1.2 不在本期范围

- 实时字幕（本地逐 5 秒分片推理的延迟/CPU 占用需单独评估，列为 v2）。
- GPU 加速（CUDA/Vulkan 版本）与自动硬件探测。
- 说话人分离（Whisper 不提供，需额外 diarization 模型）。
- 内置 C/C++ 绑定（本期使用 whisper.cpp 官方预编译 CLI，后续评估内嵌）。
- macOS/Linux（项目当前仅支持 Windows）。

## 2. 方案选型

| 方案 | 结论 |
|---|---|
| whisper.cpp 官方预编译 CLI（子进程） | **采用**：无需用户安装 C++/LLVM，包体小，CPU 即可运行，MIT |
| whisper-rs 静态绑定 | 暂不采用：构建需要 libclang 与 C++ 工具链，开发/CI 门槛高 |
| ONNX Runtime | 暂不采用：额外运行时、模型转换链路长 |
| Python faster-whisper | 不采用：需要 Python + PyTorch，不符合桌面端分发 |

推理引擎固定为 whisper.cpp 官方 Release 的 Windows CPU 包，按版本锁定下载。

## 3. 分发与存储

### 3.1 目录

全部内容位于应用数据目录，不写入仓库、不进入安装包：

```text
%APPDATA%/com.magnetictape.desktop/local-whisper/
├── engine/                 # whisper-cli.exe + ggml*.dll + whisper.dll
└── models/
    ├── ggml-base.bin
    ├── ggml-small.bin
    └── ggml-large-v3-turbo-q5_0.bin
```

### 3.2 引擎

| 项 | 值 |
|---|---|
| 来源 | `https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-x64.zip` |
| 大小 | 8,928,640 字节（解压约 23 MB） |
| SHA-256 | `6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb` |
| 解压规则 | 仅提取 `Release/` 下的文件，平铺到 `engine/` |
| 完整性 | 下载后校验 SHA-256，不符则删除并报错 |
| 平台 | Windows x64；其他平台明确报错 |

### 3.3 模型

来源均为 Hugging Face `ggerganov/whisper.cpp`：

| 模型 ID | 显示名 | 大小（字节） | SHA-256 | 建议用途 |
|---|---|---|---|---|
| `base` | Base（快） | 147,951,465 | `60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe` | 低配 CPU / 英文为主 |
| `small` | Small（推荐） | 487,601,967 | `1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b` | 中文会议默认 |
| `large-v3-turbo-q5_0` | Large v3 Turbo（高质量·量化） | 574,041,195 | `394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2` | 质量优先、可接受较慢 |

- 默认模型：`small`。
- 只允许下载上表中的模型，模型 ID 不接受任意字符串。
- 已安装即跳过下载；下载中断保留 `.part` 文件，下次续传从 0 开始（v1 不做断点续传，但会清理临时文件）。
- 删除模型仅删除对应 `.bin`，不影响引擎与其他模型。

## 4. 转写数据流

```text
分片侧车（mic-*/sys-*.json）
        ↓ build_segments：按共享时间轴解码、混音、降采样 16 kHz、8 分钟切段
segment-000.wav …
        ↓ 逐段 spawn whisper-cli -m <model> -f segment.wav -l <lang> -oj -of <tmp>
<tmp>.json（transcription[].offsets.from/to 毫秒 + text）
        ↓ 加上 segment.start_ms，合并、按时间排序
transcript.json / transcript.md（现有格式不变）
```

- 语言映射：`auto`→`auto`，`zh-CN`→`zh`，`en-US`→`en`；其他值透传。
- 命令行参数：`-oj`（JSON）、`-of`、`-nt`（不打印时间戳到 stdout）、`-np`（不打印进度）。
- 子进程使用 `CREATE_NO_WINDOW`，stdout/stderr 丢弃，避免弹黑框。
- 段级失败不终止整个任务：记录失败原因，其他段成功则返回部分结果；全部失败才返回错误。
- 单段超时：`max(180 秒, 音频时长 × 4)`，超时杀掉子进程并记为失败。
- 取消：沿用 `TranscriptionRun`，在每个段开始前检查取消标志。
- 临时目录沿用现有 `%TEMP%/magnetic-tape-asr-<session>-<时间戳>`，结束后删除。

## 5. 设置

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `localWhisperModel` | string | `small` | 本地转写使用的模型 ID |
| `asrProvider` | string | `minimax` | 允许新增 `local_whisper`，用于记住用户选择 |

- 模型 ID 非法时回退到 `small`。
- 本地转写不需要 API Key，不读写 Windows 凭据管理器。

## 6. 界面需求

### 6.1 转写设置对话框

- 服务商下拉新增「本地 Whisper（离线）」。
- 选择本地后：
  - 隐藏「接口地址 / 模型名 / API Key」输入与「清除密钥」；
  - 显示模型下拉（三个模型）、安装状态、占用空间；
  - 「下载引擎」按钮：引擎缺失时可见，下载中显示百分比并禁用；
  - 「下载模型」按钮：当前模型缺失时可见，下载中显示百分比与已下载/总量；
  - 「删除模型」按钮：当前模型已安装时可见，需二次确认；
  - 下载失败显示错误文本，可重试。
- 保存时持久化本地模型选择；无需联网校验。

### 6.2 完成页

- 转写服务下拉新增「本地 Whisper」。
- 未安装引擎或模型时点击「开始转写」，提示「请先在转写设置中下载本地引擎/模型」，不启动任务。
- 转写中沿用进度条文案（`正在转写 n/m 段`）与取消按钮。

### 6.3 实时字幕

- v1 选择本地 Whisper 时，实时字幕开关不启动本地推理：
  - 前端提示「本地 Whisper 暂不支持实时字幕，请使用云端服务或录音后转写」；
  - 后端 `start_live_transcription` 对本地 provider 返回同样错误，防止绕过界面调用。

## 7. 性能与资源目标

- 引擎下载 ≤ 10 MB；默认模型 ≤ 500 MB。
- 内存：运行 whisper.cpp 时进程峰值不超过「模型大小 + 1 GB」。
- 典型 8 分钟段在 8 核桌面 CPU、small 模型下 ≤ 6 分钟转完（约 1× 实时）；不满足时用户可切换 base。
- 转写期间 UI 保持可交互（推理在阻塞线程池执行，不占用 async 运行时）。
- 下载期间展示进度，单次下载失败不影响录音与已有转写。

## 8. 错误处理

| 场景 | 行为 |
|---|---|
| 引擎未安装 | 返回「本地 Whisper 引擎未安装，请在转写设置中下载」 |
| 模型未安装 | 返回「本地 Whisper 模型未下载：<模型名>」 |
| 下载 HTTP/网络错误 | 事件推送失败原因，保留可重试状态，不写半成品文件 |
| SHA-256 不匹配 | 删除临时文件并报「下载文件校验失败」 |
| whisper-cli 非零退出 | 捕获退出码与 stderr 摘要（截断 500 字符）写入该段失败原因 |
| 子进程超时 | 杀掉进程，记为该段失败 |
| 全部段失败 | 命令返回错误，前端展示 |
| 部分段失败 | 返回已有结果，前端状态显示段数与「部分失败」提示（沿用现有文案） |
| 并发下载 | 同一时间只允许一个引擎/模型下载任务，第二次请求直接报错 |

## 9. 隐私与安全

- 音频、转写结果、模型与引擎全部保存在本机；本地转写不发任何网络请求（下载阶段除外）。
- 下载源仅限上表固定 URL；模型文件名由白名单映射，不接受路径分隔符。
- 解压时拒绝绝对路径与 `..`（zip slip），只允许 `Release/` 前缀。
- 引擎与模型不写入安装目录，卸载应用不删除用户模型（文档中说明）。

## 10. 验收标准

1. 在未安装任何本地组件的机器上，打开转写设置能看到引擎/模型状态与下载按钮。
2. 下载引擎与 `small` 模型后断网，完成一段录音可成功得到带时间戳的转写，网络抓包无外部请求。
3. 删除模型后再次转写返回明确的中文错误，不崩溃、不产生空 `transcript.json`。
4. 转写过程中可取消，取消后不写入半成品转写。
5. 模型 ID 非法、路径穿越、校验失败均被拒绝。
6. `cargo fmt --check`、`cargo clippy -D warnings`、`cargo test`、`npm run build` 全部通过。
7. 实时字幕在本地 provider 下被明确拒绝，并给出可操作提示。

## 11. 后续迭代

- v2：本地实时字幕（tiny/base + 增量推理、VAD 切分、低延迟调度）。
- v2：GPU 加速包（CUDA 12.x / Vulkan）与自动探测。
- v3：说话人分离（sherpa-onnx / pyannote 类模型）与标点、热词。
- v3：内嵌 whisper.cpp 绑定，去掉子进程与外部二进制。
