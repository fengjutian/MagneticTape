# MagneticTape

MagneticTape 是一个面向 Windows 的本地优先会议录音与转写工具。项目计划使用 Rust 构建可靠的麦克风与系统声音采集核心，并通过 Tauri 提供轻量桌面界面。

> [!WARNING]
> 项目目前处于早期开发阶段。双路录音、分片恢复、转写、实时字幕、设备自动重连和 NSIS 安装包已经可用，但签名与自动更新尚未完成，请勿将其作为重要会议的唯一录音工具。

## 设计目标

- 分别采集麦克风和系统声音，并映射到统一的单调时间轴。
- 使用独立的 Ogg Opus 分片控制存储开销并降低数据损坏范围。
- 通过只增不改的 checkpoint 和恢复清单应对崩溃及强制退出。
- 将实时转写、录后转写、翻译和摘要与音频采集线程解耦。
- 默认在本地保存数据，仅在用户明确配置后使用云服务。

## 当前进度

已实现：

- 48 kHz 内部采样率约定。
- 麦克风、系统声音及混音轨道的共享时间轴模型。
- 时间片重叠和溢出校验。
- 录音分片元数据与恢复清单的数据模型及校验。
- 原子写入、按序加载且不覆盖旧文件的 checkpoint 存储。
- Tauri v2 桌面端工程骨架和录音页视觉样式。
- Windows 麦克风与系统声音设备发现、实时电平和双路 Ogg Opus 分片录制。
- 每 5 秒一个独立可解码分片，配套原子提交的 JSON 侧车元数据。
- 录音开始、暂停、恢复、停止及本地文件定位。
- 完成或恢复后可在桌面端按分片顺序回放麦克风与系统声音声轨。
- MiniMax 与 DeepSeek Provider 配置、统一 Chat Completions 调用和连通性测试。
- AI API Key 使用 Windows 凭据管理器加密保存和清除。
- 会话级 Markdown 会议笔记，以及 MiniMax/DeepSeek 结构化纪要生成。
- 本地历史录音资料库，可查看状态、时长并重新打开回放。
- 录音标题持久化及历史记录标题搜索。
- 历史录音空间占用统计，以及移入 Windows 回收站的安全删除。
- 基于 `RecoveryManifest` 与 `RecoveryStore` 的只增不改 checkpoint，记录分片、时间轴与提交进度。
- 启动时扫描未正常结束的会话，结合 checkpoint 与分片侧车自动恢复，并在完成页呈现恢复结果。
- 兼容旧版本双 WAV 会话的读取与回放。
- 录后转写：解码 Ogg Opus、双轨混音、降采样到 16 kHz、按 8 分钟切段上传。
- MiniMax ASR（支持时间戳与说话人分离）与 SiliconFlow SenseVoice 两种转写服务，密钥保存在 Windows 凭据管理器。
- 转写结果持久化为 `transcript.json` 与带时间戳的 `transcript.md`，历史会话可重新打开。
- 实时字幕：录音中按已提交的 5 秒分片低延迟轮询转写，最多并发处理两个 ASR 请求，乱序结果按时间轴合并后显示。
- AI 同传：对新增实时字幕做增量翻译，并在右侧按段显示原文与中文/英文译文；翻译失败不影响录音和 ASR。
- 基于转写文本的 AI 翻译：长转写按行分块请求后合并，译文保存为 `translation-<语言>.md`，重新打开会话或导出时复用。
- AI 会议纪要采用分片提取 + 归并（map-reduce），长会议不会因单次输入或输出上限被截断；生成后自动写入会话笔记。
- 长任务进度与取消：转写按音频分段汇报进度并支持取消，翻译与纪要按片段汇报进度。
- 统一回放时间轴：麦克风与系统声音分片按共享时间轴连续播放，可整体拖动进度、切换 0.75–2× 倍速；点击字幕时间戳跳转并高亮当前句。
- 设备与界面偏好持久化：麦克风/系统输出、转写语言、实时字幕开关、ASR 端点与模型。
- 音频流 watchdog：设备掉线、休眠唤醒或音频服务重启后自动重建采集流，并在页面提示重连状态。
- 一键导出：混音 WAV、TXT/SRT/VTT 字幕、翻译稿、Markdown 纪要与会话元数据。
- 音频保留策略：可选“转写后删除音频”，转写成功或停止时已有实时字幕才会删除分片；也可在完成页手动删除，转写、笔记与恢复清单元数据保留。
- 历史库容量视图与批量清理：显示总会话/总占用/音频占用，可清理 30/90/180 天前已有转写的音频。
- 自动清理与存储上限：可设置启动时清理 30/90/180 天前音频，或按 5/10/20 GB 上限从最旧会话开始删除音频（保留转写）。
- NSIS 安装包配置、`scripts/build.ps1` 与 GitHub Release 工作流。
- 本地 Whisper（离线）：whisper.cpp 引擎与 GGML 模型按需下载并做 SHA-256 校验，录后转写完全在本机完成，无需 API Key 与网络。
- 本地实时字幕：录音中按 5 秒分片用 tiny/base 模型增量推理，单窗口串行调度；支持热词/初始提示词。
- GPU 加速（可选）：`nvidia-smi` 自动探测 NVIDIA GPU，已安装 CUDA 引擎时优先使用，否则回退 CPU 引擎。
- 可交互的双栏桌面录音页面。

尚未实现：

- 说话人分离（需要额外的 diarization 模型）与内嵌 whisper.cpp 绑定（当前为子进程）。
- 代码签名、自动更新与崩溃上报。
- 端到端 UI 自动化测试。

完整清单、优先级与里程碑见 [功能规划（未完成项）](doc/功能规划.md)。

## 项目结构

```text
MagneticTape/
├── apps/
│   └── desktop/          # Tauri v2 + Vanilla TypeScript 桌面端
│       └── src-tauri/    # 桌面端 Rust 后端
├── crates/
│   ├── audio/            # Windows 双路音频采集、实时电平和 Ogg Opus 分片编码
│   └── core/             # 时间轴、分片元数据和崩溃恢复领域库
├── doc/                  # 需求、逆向分析及测试素材
├── scripts/              # 本地开发和质量检查脚本
└── Cargo.toml            # Rust workspace
```

## 环境要求

- Windows 10 或 Windows 11
- Rust 1.90 或更高版本，使用 MSVC toolchain
- Node.js 20 或更高版本
- Microsoft C++ Build Tools
- Microsoft Edge WebView2 Runtime

完整的 Windows 依赖安装方式请参阅 [Tauri v2 前置要求](https://v2.tauri.app/start/prerequisites/)。

## 开发

安装前端依赖：

```powershell
cd apps/desktop
npm install
```

可在仓库根目录启动桌面开发环境：

```powershell
./scripts/dev.ps1
```

也可以直接运行：

```powershell
cd apps/desktop
npm run tauri dev
```

## 测试与质量检查

运行 Rust 格式检查、Clippy、全部单元测试和前端生产构建：

```powershell
./scripts/check.ps1
```

也可以分别检查 Rust workspace：

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

## 构建安装包

```powershell
./scripts/build.ps1
```

产物位于 `target/release/bundle/nsis/MagneticTape_<version>_x64-setup.exe`。也可以用 `npm run tauri build -- --debug` 快速验证打包流程（debug 安装包）。

当前安装包未做代码签名，Windows SmartScreen 会显示未知发布者提示；自动更新也尚未接入。`.github/workflows/release.yml` 会在推送 `v*` 标签时构建并上传安装包。

## 导出

录音完成页的“导出会议资料”会在 `文档/MagneticTape/<标题>-<会话编号>/` 生成：

- `meeting.md`：标题、时长、转写与笔记
- `transcript.txt` / `transcript.srt` / `transcript.vtt`（有转写时）
- `audio.wav`：双轨混音后的 48 kHz 单声道 WAV
- `notes.md`、`session.json`

音频删除后历史库仍保留该会话（标记“仅转写”），可继续查看/导出转写与笔记。

## 设计资料

- [UI 设计规范](doc/UI设计规范.md)
- [录音页面布局需求](doc/录音页面布局需求.md)
- [本地 Whisper 转写需求](doc/本地Whisper转写需求.md)
- [功能规划（未完成项）](doc/功能规划.md)
- [飞书妙记录音逆向分析](doc/飞书妙记录音-逆向分析.md)

## 许可证

本项目基于 [MIT License](LICENSE) 开源。
