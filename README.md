# LiveSub-Player

> **视频没字幕？边看边自动生成。** LiveSub-Player 根据视频音频实时生成字幕：本地语音识别、双语翻译、一键导出 SRT，安装包约 30 MB。

[![build](https://github.com/wu529778790/livesub-player/actions/workflows/build.yml/badge.svg)](https://github.com/wu529778790/livesub-player/actions/workflows/build.yml)
[![release](https://github.com/wu529778790/livesub-player/actions/workflows/release.yml/badge.svg)](https://github.com/wu529778790/livesub-player/releases)

## 这是什么

把本地视频（生肉/熟肉/无字幕资源）丢进播放器，画面立即开始播放，whisper 在后台实时生成字幕，翻译随后跟上——**原文 / 译文 / 双语三模式一键切换**。不用去字幕站搜字幕，不用折腾时间轴。官网：[livesub-player.shenzjd.com](https://livesub-player.shenzjd.com/)

## 当前状态：macOS（Apple Silicon）可日用 · v1.0.0

| 平台 | 状态 |
|---|---|
| macOS (Apple Silicon) | ✅ 可日用：实时字幕 + 翻译 + 播放控制 + 全片字幕导出 |
| Windows / Linux | 🔧 编译验证中，播放内核未接入——**Release 暂不提供安装包** |

**运行前提（macOS）**：`brew install ffmpeg`（识别字幕依赖；mpv 播放内核已内嵌进应用，无需另装）。应用启动时会检测 ffmpeg，缺失时给出引导。

**已实现**：libmpv 播放内核（render API 自绘 + VideoToolbox 硬解）、whisper 实时识别（追帧不追播、静音跳过、幻觉过滤）、识别结果字幕库（落盘复用：重开秒出全片字幕、回看零等待，看片期间后台自动补齐全片）、在线/本地混合翻译（缓存跨启动有效 + 就近优先；本地翻译引擎与模型应用内一键下载、断点续传）、按文件记住播放进度 + 最近播放列表、外挂字幕自动加载（含 GBK 转码）、模型按需下载器、全片字幕导出（SRT，优先复用字幕库）、应用内自动更新（下载进度条 + 自动重启替换）。

**未实现**：签名公证、Windows/Linux 播放内核、导出译文、学习模式。完整规划见 [docs/技术方案.md](docs/技术方案.md)。

## 翻译策略

```
在线免费翻译（默认，秒回、零算力）
   └─ 被内容过滤 / 断网 / 网络受限
        → 自动落回本地模型（全离线、无内容屏蔽）
        → 没装？设置页一键下载：翻译引擎（~12MB）+ 混元 MT 模型（~4.4GB），断点续传
```

安装包不内置模型，需要时按需下载。对「在线服务翻不出来的片源」，本地模型就是答案。

**隐私说明**：识别与播放完全本地；在线翻译会把字幕文本发往 Google 翻译接口；模型下载走 hf-mirror 镜像；更新检查走 GitHub。除此之外不上传任何数据。

## 安装（macOS，Apple Silicon）

不依赖付费签名公证（个人免费工具，省 $99/年），通过「下载时不打隔离标记」的通道分发，装完双击即开、无 Gatekeeper 拦截：

**方式一：一键脚本（推荐）**

```bash
curl -fsSL https://raw.githubusercontent.com/wu529778790/livesub-player/main/install.sh | bash
```

自动识别最新 Release、下载、校验、安装到 /Applications、清理旧版本。指定版本：`| bash -s -- v1.0.0`。

**方式二：手动下载**

从 [Releases](https://github.com/wu529778790/livesub-player/releases) 下载 `*-macOS.zip` 解压，拖进「应用程序」。浏览器下载的文件带隔离标记，**首次打开需右键 → 打开 → 再点打开**（只需一次），或终端执行 `xattr -cr /Applications/LiveSub-Player.app`。

**Homebrew**：暂未提供官方 tap（cask 定义见 [packaging/cask/livesub-player.rb](packaging/cask/livesub-player.rb)，可自行建 tap 使用）。

**Windows / Linux**：当前 Release 不提供安装包（播放内核接入后开放）。

## 从源码构建

```bash
# 依赖：Node 18+、Rust stable、cmake、libmpv、ffmpeg
brew install rust cmake mpv ffmpeg   # macOS

npm install
npm run tauri dev     # 开发
npm run tauri build   # 打包
```

Windows 需自行安装 mpv/ffmpeg 并配置链接路径（播放内核尚未接入）；Linux 需 libwebkit2gtk-4.1 等系统依赖（见 CI 配置）。

## 快捷键

| 按键 | 功能 |
|---|---|
| `空格` | 播放 / 暂停 |
| `←` / `→` | 快退 / 快进 5s（按钮为 10s） |
| `↑` / `↓` | 音量 ±5 |
| `F` | 全屏 |
| `S` | 截图到桌面 |
| `E` | 导出全片字幕 SRT（识别原文） |
| `1` / `2` / `3` | 译文 / 原文 / 双语 |

## 架构

```
┌────────────────────────────────────┐
│  WebView（Vue 3，透明，字幕 Overlay）│
│  ────────────────────────────────  │
│  libmpv render API 自绘（视频垫底） │
└──────────────┬─────────────────────┘
               │
        Rust 核心（Tauri 2）
        ├─ player  播放控制 + 事件推送（含播放失败上报）
        ├─ asr     ffmpeg 管道抽音频 → VAD → whisper（进程内）
        ├─ mt      在线免费翻译 ↔ llama-server 本地兜底（引擎应用内下载）
        └─ models  模型/引擎下载器（hf-mirror / 断点续传 / tar.gz 解压）
```

识别是软实时：识别领先播放 30s 自动等待，seek 直接丢弃当前窗口重启管线，时间戳以 ffmpeg 输出为基准不漂移。

## 反馈

问题与建议请到 [GitHub Issues](https://github.com/wu529778790/livesub-player/issues) 提交；排障时请附上「设置 → 通用 → 打开日志文件夹」里的最新 .log 文件。

## 路线图

- [x] M0 脚手架 + 三平台 CI
- [x] M1 播放内核（macOS）
- [x] M2 实时字幕管线
- [x] M3 翻译管线 + 在线/本地混合架构
- [x] M4 模型管理（设置页 / 首启向导 / 翻译引擎自动下载 / 混元 MT-7B 本地翻译实测通过）
- [x] M5a 应用内自动更新（Tauri updater + minisign 签名，不依赖 Apple 公证）
- [x] M5.5 全片字幕导出（异步识别，SRT 落盘）
- [ ] M5b 发布打磨（签名公证 / 真实截图与演示 / 隐私政策页）
- [ ] M1-Windows 播放内核接入（接入前 Release 不挂 Windows/Linux 安装包）

## License

本项目采用 [PolyForm Noncommercial License 1.0.0](LICENSE)：**禁止商用**——个人使用、学习、研究、非营利组织使用均免费，商用需另行授权。
