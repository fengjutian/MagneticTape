# MagneticTape

MagneticTape 是一个面向 Windows 的本地优先会议录音与转写工具。项目计划使用 Rust 构建可靠的麦克风与系统声音采集核心，并通过 Tauri 提供轻量桌面界面。

> [!WARNING]
> 项目目前处于早期开发阶段。双路录音、分片恢复、转写、实时字幕和设备自动重连已经可用，但正式发行打包尚未完成，请勿将其作为重要会议的唯一录音工具。

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
- 每 15 秒一个独立可解码分片，配套原子提交的 JSON 侧车元数据。
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
- 实时字幕：录音中按已提交的 15 秒分片轮询转写，字幕流直接显示在右侧面板。
- 基于转写文本的 AI 翻译，以及与转写联动的结构化会议纪要生成。
- 设备与界面偏好持久化：麦克风/系统输出、转写语言、实时字幕开关、ASR 端点与模型。
- 音频流 watchdog：设备掉线、休眠唤醒或音频服务重启后自动重建采集流，并在页面提示重连状态。
- 可交互的双栏桌面录音页面。

尚未实现：

- 本地 Whisper 引擎（当前转写需要配置 MiniMax 或 SiliconFlow）。
- 安装包、代码签名、自动更新与正式发布流程。
- 音频/SRT/Markdown 导出与批量清理。

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

## 设计资料

- [录音页面布局需求](doc/录音页面布局需求.md)
- [飞书妙记录音逆向分析](doc/飞书妙记录音-逆向分析.md)

## 许可证

本项目基于 [MIT License](LICENSE) 开源。
