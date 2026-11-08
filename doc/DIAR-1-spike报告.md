# DIAR-1 说话人分离 Spike 报告

> 状态：Spike 完成，结论为**可行**（2026-11-08）
> 目的：在投入正式开发前，验证 sherpa 系方案在本机（Windows x64）能否跑通中文说话人分离
> 关联：[功能规划 DIAR-1](功能规划.md)、[端侧模型选型调研](端侧模型选型调研.md)

## 1. 结论

**可行，且比原规划的 sherpa-rs 路线更简单。**

关键发现：**sherpa-rs 的 Rust 绑定在本机不可用**（构建需要 libclang，本机无 LLVM），但 **sherpa-onnx 官方提供了 Windows x64 预编译二进制**，其中包含现成的 `sherpa-onnx-offline-speaker-diarization.exe`。

因此 DIAR-1 的落地形态应改为：**官方 CLI 子进程**，与项目现有的 whisper.cpp 子进程模式完全同构（`local_whisper.rs` 已有下载 → SHA-256 校验 → 解压 → spawn → 解析的完整框架，可直接复用）。

实测：官方中文四人音频正确分出 4 个说话人，**RTF 0.087**（57 秒音频 4.97 秒处理完）；项目自己的真实录音正确识别为单说话人，无误判。

## 2. 关键路线修正

### 2.1 sherpa-rs 不可行的原因（实测）

`sherpa-rs 0.6.8` 的构建流程（`sherpa-rs-sys-0.6.8/build.rs`）：

- 第 309–340 行：`SHERPA_SKIP_GENERATE_BINDINGS` 环境变量可以跳过 bindgen，改用预置的 `src/bindings.rs`；
- **但 `src/bindings.rs` 并未随 crate 发布**（实测 `sherpa-rs-sys-0.6.8/src/` 只有 `download_binaries.rs` 和 `lib.rs`），所以该跳过路径不可用；
- 结果：无论如何都会走 bindgen，而 bindgen 需要 libclang。

实测报错：

```
thread 'main' panicked at bindgen-0.69.5/lib.rs:622:31:
Unable to find libclang: "couldn't find any valid shared libraries matching:
['clang.dll', 'libclang.dll'], set the `LIBCLANG_PATH` environment variable ..."
```

本机核查：`C:\Program Files\LLVM\bin\libclang.dll` 不存在，`C:\Program Files\Microsoft Visual Studio\2022` 目录也不存在。

> **注意**：这是对「[端侧模型选型调研](端侧模型选型调研.md) 第 4.2 节」的**修正**。该文当时写「sherpa-rs 有官方 crate、预编译二进制齐，Windows 可用」，这个判断只对「运行时二进制」成立，**对「Rust 绑定能否编译」不成立**——两者是不同的问题。

### 2.2 可行路线：官方 CLI 子进程

sherpa-onnx v1.12.9 的 Windows x64 预编译包（`sherpa-onnx-v1.12.9-win-x64-shared.tar.bz2`，24,139,676 字节）提供：

```
bin/
  sherpa-onnx-offline-speaker-diarization.exe   ← 说话人分离
  sherpa-onnx-vad.exe                            ← VAD
  sherpa-onnx-offline-punctuation.exe            ← 标点
  sherpa-onnx-offline-denoiser.exe               ← 降噪
  sherpa-onnx-microphone-offline-speaker-identification.exe
  onnxruntime.dll
lib/
  sherpa-onnx-c-api.dll / .lib                   ← C API（供 FFI 方案备选）
  sherpa-onnx-cxx-api.dll / .lib
  onnxruntime.dll
include/
  sherpa-onnx/c-api/c-api.h                      ← 完整 C API 头文件
```

三种可选形态：

| 形态 | 做法 | 评价 |
|---|---|---|
| **A. CLI 子进程** | spawn `sherpa-onnx-offline-speaker-diarization.exe`，解析 stdout | **推荐**。与项目现有 whisper.cpp 模式同构，零构建依赖 |
| B. C API FFI | 手写 `extern "C"` 声明链接 `sherpa-onnx-c-api.dll` | 可行但需手写 bindings；官方有 `c-api.h` 可对照 |
| C. sherpa-rs | Rust crate | **不可行**（见 2.1） |

## 3. 实测数据

### 3.1 模型资产（全部实测下载成功）

| 文件 | 大小 | 来源 |
|---|---|---|
| `sherpa-onnx-pyannote-segmentation-3-0.tar.bz2` | 6,958,444 B | `releases/tag/speaker-segmentation-models` |
| └ `model.onnx`（FP32） | — | 解压所得 |
| └ `model.int8.onnx`（INT8） | — | 解压所得 |
| `3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx` | 28,281,138 B | `releases/tag/speaker-recongition-models` |
| `0-four-speakers-zh.wav`（中文四人官方样例） | 1,819,586 B | `releases/tag/speaker-segmentation-models` |
| `sherpa-onnx-v1.12.9-win-x64-shared.tar.bz2` | 24,139,676 B | `releases/tag/v1.12.9` |

### 3.2 分割模型精度与线程的影响

输入：`0-four-speakers-zh.wav`（56.861 秒，中文四人），声纹模型固定为 3dspeaker campplus zh-cn。

| 分割模型 | 线程 | 耗时 | RTF | 说话人分组 |
|---|---|---|---|---|
| `model.onnx` (FP32) | 1 | 14.511 s | 0.255 | 4 组 ✅ |
| `model.int8.onnx` | 1 | 6.295 s | 0.111 | 4 组 ✅ |
| `model.int8.onnx` | 4 | **4.974 s** | **0.087** | 4 组 ✅ |
| `model.int8.onnx` + directml | 1 | 7.642 s | 0.134 | 4 组 ✅ |

**结论**：
- **INT8 分割模型比 FP32 快 2.3 倍**，且分组结果完全一致 → 生产环境用 INT8；
- **4 线程比 1 线程再快 1.27 倍** → 用 `--segmentation.num-threads=4` / `--embedding.num-threads=4`；
- **DirectML 可用但更慢**（RTF 0.134 vs 0.087）——模型小，GPU 传输与调度开销占优。**默认走 CPU**，除非实测长音频下 GPU 反超。

### 3.3 自动聚类阈值（说话人数未知时）

输入同上，用 `--clustering.cluster-threshold` 而非指定簇数：

| 阈值 | 分出的说话人 | 判定 |
|---|---|---|
| 0.5 | speaker_00/01/02/05/07（**8 个**） | ❌ 过度分割 |
| **0.7** | speaker_00/01/02/03（**4 个**） | ✅ **与指定 4 簇完全一致** |
| 0.8 | speaker_00/01/02（3 个） | ⚠️ 合并过度 |
| 0.9 | speaker_00/01/02（3 个） | ⚠️ 合并过度 |

**结论**：`threshold = 0.7` 是自动聚类的最佳默认值（对四人中文场景）。

> 官方 CLI 的 `--help` 默认值是 0.5，实测会过度分割，**必须显式传 0.7**。

### 3.4 真实录音验证

用项目自己的 `doc/chunk_00000.ogg`（经 ffmpeg 转 16 kHz 单声道 WAV，3.0 秒）：

```
Duration : 3.000 s
Elapsed seconds: 0.026 s
Real time factor (RTF): 0.026 / 3.000 = 0.009
0.031 -- 0.993 speaker_01
```

单说话人 → 正确输出单个说话人，无误判、无碎片化。

### 3.5 输出格式

CLI 输出到 stdout：

```
Duration : 56.861 s
Elapsed seconds: 4.974 s
Real time factor (RTF): 4.974 / 56.861 = 0.087
OfflineSpeakerDiarizationConfig(...)
Started
0.638 -- 6.848 speaker_00
7.017 -- 10.679 speaker_01
...
```

**说话人行格式稳定**：`{start} -- {end} speaker_{NN}`（浮点秒 + 说话人编号），可直接正则解析。

注意：`progress xx%` 行会持续输出到 stderr，正式实现需过滤；`parse-options.cc:Read:372` 的告警是 PowerShell 把 stderr 当错误流显示，实际不影响退出码。

## 4. 落地实施方案

### 4.1 依赖获取（复用现有框架）

在 `local_whisper.rs` 的下载校验框架旁新增一份规格：

| 项 | 值 |
|---|---|
| 引擎包 | `https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.12.9/sherpa-onnx-v1.12.9-win-x64-shared.tar.bz2` |
| 包大小 | 24,139,676 字节 |
| SHA-256 | 待下载后填入（`releases/tag/v1.12.9` 的 `checksum.txt` 提供） |
| 解压目标 | `%APPDATA%/com.magnetictape.desktop/local-diarization/engine/` |
| 可执行文件 | `sherpa-onnx-offline-speaker-diarization.exe` |
| 运行时 DLL | `bin/onnxruntime.dll`（同目录） |

模型清单：

| 模型 | 大小 | 建议 |
|---|---|---|
| `sherpa-onnx-pyannote-segmentation-3-0/model.int8.onnx` | 解压所得 | ✅ 生产用 INT8 |
| `3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx` | 28,281,138 B | ✅ 中文声纹 |

### 4.2 调用形态

```
sherpa-onnx-offline-speaker-diarization.exe \
  --clustering.cluster-threshold=0.7 \
  --segmentation.pyannote-model=<abs path>/model.int8.onnx \
  --segmentation.num-threads=4 \
  --embedding.model=<abs path>/3dspeaker_campplus_zh-cn.onnx \
  --embedding.num-threads=4 \
  <16k mono wav>
```

- 输入必须是 **16 kHz 单声道 WAV**（项目转写管线已有 16 kHz 分段，可直接复用 `asr::build_segments` 的产物）；
- 输出解析 `^([\d.]+) -- ([\d.]+) speaker_(\d+)$`；
- 子进程用 `CREATE_NO_WINDOW`，同 whisper-cli 的处理方式。

### 4.3 与转写结果的合并

按时间重叠把说话人标签分配给 `transcript.json` 的每个 segment：
- diarization 输出的是说话人片段，转写输出的是文本片段；
- 合并规则：**文本片段的中点落在哪个说话人区间内**即归属该说话人；
- 未覆盖的片段标记为「未知说话人」，不丢弃文本。

### 4.4 双轨归属（DIAR-1 原需求中未定的一项）

项目内部保持麦克风轨与系统声轨分离：
- **混合轨**做 diarization → 得到说话人（人）；
- 单轨仍用于回放；
- UI 的说话人标签来自 diarization，与「麦克风/系统」轨道是**两个正交维度**，不能互相冒充。

### 4.5 工作量重估

| 项 | 估算 |
|---|---|
| 下载校验 + 解压 + 状态展示（复用 local_whisper 框架） | 0.5 天 |
| CLI 调用 + 输出解析 + 取消/超时 | 0.5 天 |
| 与转写结果合并 + `speaker` 字段写入 transcript.json | 0.5 天 |
| UI：说话人标签、重命名、按说话人筛选 | 1–2 天 |
| **合计** | **2.5–3.5 天**（原估 5–8 天） |

比原估低，因为：下载校验框架可复用、CLI 无构建依赖、模型管理复用现有结构。

## 5. 遗留问题

1. **双人真实会议音频尚未验证**——官方样例是四人，真实会议是双人 + 混响 + 远场。规划里的验收线「≥ 85% 段落说话人正确」仍需真实素材验证。
2. **Silero VAD 模型未下载成功**（网络错误，非方案问题）——VAD 是 WHISPER-3 的前置，与 DIAR-1 共用同一 sherpa-onnx 引擎包，引擎侧已验证可用。
3. **实时模式未验证**——CLI 是离线的。实时说话人分离需要流式方案（sherpa 的 online API 或 MNN 后端），是独立工作项。
4. **多语言声纹模型未评估**——当前只用中文模型，英文/粤语需另选。

## 6. 复现步骤

```powershell
# 1. 下载引擎
$ProgressPreference='SilentlyContinue'
Invoke-WebRequest -Uri "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.12.9/sherpa-onnx-v1.12.9-win-x64-shared.tar.bz2" -OutFile "$env:TEMP\sherpa.tar.bz2"
tar -xjf "$env:TEMP\sherpa.tar.bz2" -C "$env:TEMP"

# 2. 下载模型
Invoke-WebRequest -Uri "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2" -OutFile "$env:TEMP\seg.tar.bz2"
tar -xjf "$env:TEMP\seg.tar.bz2" -C "$env:TEMP"
Invoke-WebRequest -Uri "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx" -OutFile "$env:TEMP\emb.onnx"
Invoke-WebRequest -Uri "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/0-four-speakers-zh.wav" -OutFile "$env:TEMP\four.wav"

# 3. 运行
$env:PATH="$env:TEMP\sherpa-onnx-v1.12.9-win-x64-shared\bin;$env:PATH"
& "$env:TEMP\sherpa-onnx-v1.12.9-win-x64-shared\bin\sherpa-onnx-offline-speaker-diarization.exe" `
  --clustering.cluster-threshold=0.7 `
  --segmentation.pyannote-model="$env:TEMP\sherpa-onnx-pyannote-segmentation-3-0\model.int8.onnx" `
  --segmentation.num-threads=4 `
  --embedding.model="$env:TEMP\emb.onnx" `
  --embedding.num-threads=4 `
  "$env:TEMP\four.wav"
```

## 7. 引用来源

- sherpa-onnx 仓库：https://github.com/k2-fsa/sherpa-onnx
- v1.12.9 发布页（含 Windows 预编译包）：https://github.com/k2-fsa/sherpa-onnx/releases/tag/v1.12.9
- 说话人分离模型：https://github.com/k2-fsa/sherpa-onnx/releases/tag/speaker-segmentation-models
- 说话人识别模型：https://github.com/k2-fsa/sherpa-onnx/releases/tag/speaker-recongition-models
- sherpa-rs crate（本 spike 判定不可用）：https://crates.io/crates/sherpa-rs
- 说话人分离文档：https://k2-fsa.github.io/sherpa/onnx/speaker-diarization/index.html
