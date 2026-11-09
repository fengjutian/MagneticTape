---
title: 安装
description: 下载并安装 MagneticTape，包含系统要求、安装包签名提示与从源码构建的步骤。
---

## 系统要求

| 项目 | 要求 |
| --- | --- |
| 操作系统 | Windows 10 或 Windows 11（64 位） |
| 运行库 | Microsoft Edge WebView2 Runtime（Windows 10 1803 及以后通常已预装） |
| 磁盘 | 安装包本身很小；录音与模型按需占用空间 |
| 网络 | 仅在下载安装包、下载本地转写模型或使用云端能力时需要 |

实时字幕与本地转写需要额外下载引擎与模型，详见[字幕与转写](/guide/transcription/)。

## 下载安装包

在 [GitHub Releases](https://github.com/fengjutian/MagneticTape/releases/latest) 下载最新的 `MagneticTape_<版本>_x64-setup.exe`，同目录下的 `.sha256` 文件是校验值。

:::caution
当前安装包**未做代码签名**。双击运行后 Windows SmartScreen 会显示「Windows 已保护你的电脑」并提示未知发布者，点击「更多信息」→「仍要运行」即可。
:::

## 安装步骤

1. 运行 `MagneticTape_<版本>_x64-setup.exe`。
2. 安装向导默认安装到用户目录下的应用文件夹，不需要管理员权限。
3. 安装完成后从开始菜单启动 **MagneticTape**。
4. 首次启动会初始化录音目录与日志目录，界面加载后即可使用。

## 更新

自动更新**尚未接入**。有新版本时，从 Releases 页面下载新的安装包覆盖安装即可；录音数据与设置保存在独立的用户数据目录，不会因为覆盖安装而丢失。

## 从源码构建

面向想参与开发的用户：

```text
环境要求
  Rust 1.90 或更高版本（MSVC toolchain）
  Node.js 20 或更高版本
  Microsoft C++ Build Tools
  Microsoft Edge WebView2 Runtime
```

安装前端依赖并启动开发环境：

```powershell
cd apps/desktop
npm install
npm run tauri dev
```

也可以在仓库根目录执行 `./scripts/dev.ps1` 一键启动。

构建安装包（产物位于 `target/release/bundle/nsis/`）：

```powershell
./scripts/build.ps1
```

## 卸载

从「设置 → 应用 → MagneticTape」卸载即可。卸载**不会删除**已保存的录音、转写与笔记；如需一并清理，请先在应用内删除对应音频或手动移除数据目录。
