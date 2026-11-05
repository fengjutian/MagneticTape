# MagneticTape

MagneticTape 是一个面向 Windows 的本地优先会议录音与转写工具。项目使用 Rust 构建可靠的双音源录音核心，并使用 Tauri 提供轻量桌面界面。

> 当前处于早期开发阶段。桌面端已经可以运行会话状态流程，但尚未接入 WASAPI 音频采集，请勿将当前版本用于正式录音。

## 目标

- 分别采集麦克风与系统声音，并映射到统一单调时间轴。
- 使用独立 Ogg Opus 分片降低存储开销。
- 通过 checkpoint 和恢复清单抵御崩溃与强制退出。
- 将实时转写、录后转写、翻译和摘要与采集线程解耦。
- 默认本地保存，云服务由用户显式配置和启用。

## 项目结构

```text
MagneticTape/
├── apps/
│   └── desktop/          # Tauri v2 + Vanilla TypeScript 桌面端
│       └── src-tauri/    # 桌面端 Rust 后端
├── crates/
│   └── core/             # 时间轴、分片元数据与崩溃恢复领域库
├── doc/                  # 逆向分析、测试音频和研究资料
├── scripts/              # 本地开发和质量检查脚本
└── Cargo.toml            # Rust workspace
```

## 环境要求

- Windows 10/11
- Rust stable（MSVC toolchain）
- Node.js 20 或更新版本
- Microsoft C++ Build Tools
- Microsoft Edge WebView2 Runtime

Tauri 的完整 Windows 环境要求见[官方文档](https://v2.tauri.app/start/prerequisites/)。

## 开发

```powershell
./scripts/dev.ps1
```

或分别执行：

```powershell
cd apps/desktop
npm install
npm run tauri dev
```

## 质量检查

```powershell
./scripts/check.ps1
```

该脚本会检查 Rust 格式、Clippy、单元测试和前端生产构建。

## 设计资料

- [飞书妙记录音逆向分析](doc/飞书妙记录音-逆向分析.md)

## 许可证

[MIT](LICENSE)

