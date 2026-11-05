# MagneticTape

MagneticTape 是一个面向 Windows 的本地优先会议录音与转写工具。项目计划使用 Rust 构建可靠的麦克风与系统声音采集核心，并通过 Tauri 提供轻量桌面界面。

> [!WARNING]
> 项目目前处于早期开发阶段。双路录音流程已经可用，但尚未实现分片恢复、转写和正式发行打包，请勿将其作为重要会议的唯一录音工具。

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
- Windows 麦克风与系统声音设备发现、实时电平和双路 WAV 录制。
- 录音开始、暂停、恢复、停止及本地文件定位。
- 完成或恢复后可在桌面端分别回放麦克风与系统声音声轨。
- MiniMax 与 DeepSeek Provider 配置、统一 Chat Completions 调用和连通性测试。
- 本地历史录音资料库，可查看状态、时长并重新打开回放。
- 录音标题持久化及历史记录标题搜索。
- 每两秒提交一次 WAV 文件头；进程异常退出后仍可读取最近一次提交的音频。
- 启动时自动识别未正常结束的会话，并在完成页呈现恢复结果。
- 可交互的双栏桌面录音页面。

尚未实现：

- Ogg Opus 编码、分片写盘、会话清单与自动恢复流程。
- 设备热插拔、休眠唤醒和音频服务重启后的自动恢复。
- 转写、翻译和摘要能力。

## 项目结构

```text
MagneticTape/
├── apps/
│   └── desktop/          # Tauri v2 + Vanilla TypeScript 桌面端
│       └── src-tauri/    # 桌面端 Rust 后端
├── crates/
│   ├── audio/            # Windows 双路音频采集、实时电平和 WAV 写入
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
