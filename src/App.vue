<template>
  <div class="shell" :class="{ 'ui-hidden': uiHidden, 'is-empty': !loaded }" @mousemove="onUiActivity">
    <header class="topbar">
      <svg class="topbar-icon" viewBox="0 0 48 48" aria-hidden="true">
        <rect x="2" y="2" width="44" height="44" rx="12" fill="#ffd479" />
        <path d="M19 16.2 33.4 24 19 31.8Z" fill="#16121c" />
      </svg>
      <span class="filename">{{ filename || "LiveSub-Player" }}</span>
      <button class="bar-btn" @click="settingsOpen = true">设置</button>
    </header>

    <!-- 设置抽屉：右侧滑出，点遮罩或 ✕ 关闭 -->
    <transition name="fade">
      <div v-if="settingsOpen" class="drawer-mask" @click="settingsOpen = false" />
    </transition>
    <div class="drawer" :class="{ open: settingsOpen }">
      <div class="drawer-head">
        <span>设置</span>
        <button class="drawer-close" title="关闭" @click="settingsOpen = false">✕</button>
      </div>

      <div class="set-tabs">
        <button
          v-for="t in SETTING_TABS"
          :key="t.id"
          class="set-tab"
          :class="{ on: setTab === t.id }"
          @click="setTab = t.id"
        >{{ t.label }}</button>
      </div>

      <!-- 字幕 -->
      <template v-if="setTab === 'subtitle'">
        <label class="set-row">
          <span>字幕字号</span>
          <select :value="subFont" @change="changeSubFont">
            <option value="small">小</option>
            <option value="medium">中（推荐）</option>
            <option value="large">大</option>
          </select>
        </label>
        <label class="set-row">
          <span>字幕位置</span>
          <select :value="subPos" @change="changeSubPos">
            <option value="bottom">贴底（推荐）</option>
            <option value="raised">上移一档</option>
            <option value="high">更高（避开进度条信息）</option>
          </select>
        </label>
        <label class="set-row">
          <span>默认字幕模式</span>
          <select :value="mode" @change="changeSubMode">
            <option value="双语">双语（推荐）</option>
            <option value="译文">译文</option>
            <option value="原文">原文</option>
          </select>
        </label>
        <p class="set-hint">播放中也可随时用右键菜单或 1 / 2 / 3 切换字幕模式。</p>
      </template>

      <!-- 识别与翻译 -->
      <template v-if="setTab === 'ai'">
        <label class="set-row">
          <span>识别语言</span>
          <select :value="asrLang" @change="changeAsrLang">
            <option value="auto">自动检测（推荐）</option>
            <option value="zh">中文</option>
            <option value="en">英语</option>
            <option value="ja">日语</option>
            <option value="ko">韩语</option>
          </select>
        </label>
        <p class="set-hint">看已知语种的片时固定语言，识别更快更准；选自动则跟随内容切换。</p>
        <label class="set-row">
          <span>识别档位</span>
          <select :value="whisperModel" @change="changeWhisperModel">
            <option value="small">均衡（默认，推荐）</option>
            <option value="base">快速（电脑配置较低时）</option>
          </select>
        </label>
        <label class="set-row">
          <span>翻译引擎</span>
          <select :value="engine" @change="changeEngine">
            <option value="auto">自动（优先在线，失败用本地）</option>
            <option value="online">仅在线翻译</option>
            <option value="local">仅本地模型（离线）</option>
          </select>
        </label>

        <div class="drawer-section">模型管理</div>
        <div v-for="m in modelStatus" :key="m.name" class="model-row">
          <span class="model-desc">{{ m.desc }}</span>
          <span v-if="m.installed" class="model-ok">已装 {{ m.sizeMb }}MB</span>
          <template v-else>
            <button class="btn small" :disabled="downloadingName === m.name" @click="download(m.name)">
              {{ downloadingName === m.name ? `下载中 ${downloadPct}%` : "下载" }}
            </button>
          </template>
          <button
            v-if="m.installed"
            class="btn small ghost"
            @click="removeModel(m.name)"
          >{{ pendingDelete === m.name ? "确认删除" : "删除" }}</button>
        </div>
        <p class="set-hint">
          在线翻译需要能访问 Google（自动走系统代理）；本地翻译 = 引擎 + 模型两个都装好后完全离线，没有内容限制。
        </p>
      </template>

      <!-- 播放 -->
      <template v-if="setTab === 'playback'">
        <div class="set-row">
          <span class="set-label">记住音量与倍速<small>下次启动沿用本次关闭前的音量 / 倍速</small></span>
          <button class="switch" :class="{ on: rememberPlayer }" title="切换" @click="toggleRemember"><i /></button>
        </div>
        <div class="set-row">
          <span class="set-label">启动时恢复上次播放<small>自动打开上次看的视频并跳到上次进度</small></span>
          <button class="switch" :class="{ on: resumeLast }" title="切换" @click="toggleResume"><i /></button>
        </div>
      </template>

      <!-- 通用 -->
      <template v-if="setTab === 'general'">
        <div class="set-row">
          <span class="set-label">自动检查更新<small>启动时静默检查，有新版才提示</small></span>
          <button class="switch" :class="{ on: autoUpdate }" title="切换" @click="toggleAutoUpdate"><i /></button>
        </div>
        <div class="set-row">
          <span>当前版本</span>
          <span class="set-value">v{{ appVersion || "…" }}</span>
        </div>
        <button class="btn small ghost set-btn" :disabled="checkingUpdate" @click="manualCheckUpdate">
          {{ checkingUpdate ? "检查中…" : "检查更新" }}
        </button>
        <button class="btn small ghost set-btn" @click="openLogDir">打开日志文件夹</button>
        <p class="set-hint">遇到问题时把日志文件夹里的最新 .log 文件发给开发者，可快速定位。</p>

        <div class="drawer-section">项目</div>
        <div class="set-row">
          <span>官网</span>
          <button class="link-btn" title="在浏览器打开官网" @click="openUrl(SITE_URL)">{{ SITE_URL.replace("https://", "") }}</button>
        </div>
        <div class="set-row">
          <span>GitHub</span>
          <button class="link-btn" title="在浏览器打开仓库" @click="openUrl(GITHUB_URL)">wu529778790/livesub-player</button>
        </div>
      </template>
    </div>

    <!-- 视频区域：播放中背景全透明，透出下层 mpv 画面；未加载时整窗不透明（见 .shell.is-empty）；输入由前端接管 -->
    <main
      class="stage"
      @click="onStageClick"
      @dblclick="toggleFullscreen"
      @contextmenu.prevent="openCtx"
      @wheel.prevent="onWheel"
      @dragover.prevent
      @drop.prevent="onDrop"
    >
      <!-- 未加载视频：欢迎页 -->
      <div v-if="!loaded" class="welcome">
        <div class="welcome-brand">
          <!-- 与应用图标 app-icon.svg 同构 -->
          <svg class="welcome-logo" viewBox="0 0 1024 1024" fill="none" aria-hidden="true">
            <defs>
              <linearGradient id="wl-bg" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0" stop-color="#241e2e" />
                <stop offset="1" stop-color="#110f16" />
              </linearGradient>
              <linearGradient id="wl-amber" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0" stop-color="#ffe3a1" />
                <stop offset="1" stop-color="#f2b544" />
              </linearGradient>
              <radialGradient id="wl-glow" cx="0.5" cy="0.34" r="0.6">
                <stop offset="0" stop-color="#ffd479" stop-opacity="0.16" />
                <stop offset="1" stop-color="#ffd479" stop-opacity="0" />
              </radialGradient>
            </defs>
            <rect x="100" y="100" width="824" height="824" rx="185" fill="url(#wl-bg)" />
            <rect x="100" y="100" width="824" height="824" rx="185" fill="url(#wl-glow)" />
            <rect x="101.5" y="101.5" width="821" height="821" rx="183.5" fill="none" stroke="#ffffff" stroke-opacity="0.08" stroke-width="3" />
            <rect x="352" y="300" width="320" height="320" rx="84" fill="url(#wl-amber)" />
            <path d="M466 386 L598 460 L466 534 Z" fill="#1b1520" />
            <rect x="382" y="660" width="260" height="32" rx="16" fill="#ffffff" fill-opacity="0.88" />
            <rect x="427" y="708" width="170" height="32" rx="16" fill="#ffd479" />
          </svg>
          <h1 class="welcome-title">LiveSub-Player</h1>
          <p class="welcome-tagline">AI 实时语音识别字幕 · 双语翻译播放器</p>
        </div>

        <button class="dropzone" :class="{ hot: dragHover }" @click="openFile">
          <svg class="dz-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M12 4v10m0 0-4-4m4 4 4-4" />
            <path d="M4 16v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2" />
          </svg>
          <span class="dz-main">把视频文件拖到这里，或点击选择</span>
          <span class="dz-sub">支持 MP4、MKV、MOV、WebM 等常见格式</span>
        </button>

        <template v-if="showWizard">
          <div class="wizard-card">
            <p class="wizard-title">首次使用：下载离线模型</p>
            <p class="wizard-text">
              语音识别需要下载识别模型（约 466MB）；<br />
              本地翻译 = 翻译引擎（约 12MB）+ 翻译模型（约 4.4GB），全离线、无视内容限制。<br />
              在线翻译开箱即用，无需下载。下载支持断点续传，中途关掉也没关系。
            </p>
            <div class="wizard-actions">
              <button class="btn" :disabled="wizardDownloading" @click.stop="wizardDownloadAll">
                {{ wizardButtonLabel }}
              </button>
              <button class="btn ghost" :disabled="wizardDownloading" @click.stop="wizardDismiss">
                暂不下载，直接开始
              </button>
            </div>
          </div>
        </template>

        <!-- ffmpeg 缺失：识别字幕的硬前提，必须显式引导（播放功能不依赖它） -->
        <template v-if="!ffmpegOk">
          <div class="wizard-card">
            <p class="wizard-title">还差一步：安装 ffmpeg</p>
            <p class="wizard-text">
              字幕识别依赖 ffmpeg（播放不依赖）。在终端执行下面的命令后重启本应用：<br />
              <code>brew install ffmpeg</code>
            </p>
            <div class="wizard-actions">
              <button class="btn" @click.stop="copyFfmpegHint">复制安装命令</button>
            </div>
          </div>
        </template>

        <p v-if="status" class="status">{{ status }}</p>
      </div>

      <!-- 实时字幕 Overlay（技术方案决策 B：DOM 渲染，不用 mpv sub 轨）
           原则：只要识别出了原文就展示原文，翻译没到位不吞字幕 -->
      <div v-if="activeSub" class="subtitle-overlay" :class="overlayClass">
        <div v-if="mode !== '译文' || !activeTranslation" class="sub-src">{{ activeSub.text }}</div>
        <div v-if="mode !== '原文' && activeTranslation" class="sub-dst">{{ activeTranslation }}</div>
        <div v-else-if="mode !== '原文' && mtAvailable" class="sub-dst pending">译文生成中…</div>
      </div>

      <!-- 识别进度指示（§2.2 进度可见）：只对「有人声且未识别完」的时段出现，
           5s 无新进度自动消失——静音/对白间隙/识别完成后不再常驻挡画面 -->
      <transition name="fade">
        <div v-if="loaded && asrProgressLive && recognizing && !activeSub" class="asr-progress">
          <template v-if="recognizing.start >= timePos">
            正在识别 {{ fmtTime(recognizing.start) }} – {{ fmtTime(recognizing.end) }}
          </template>
          <template v-else>
            字幕识别中，已处理到 {{ fmtTime(recognizing.end) }}，请稍候
          </template>
        </div>
      </transition>
      <div v-if="asrError" class="asr-error">{{ asrError }}</div>

      <!-- 全片字幕导出进度（异步识别整个视频，可取消） -->
      <div v-if="exporting !== null" class="asr-progress export-pill">
        <span>正在识别全片字幕 {{ exporting }}%</span>
        <button class="btn small ghost" @click.stop="cancelExport">取消</button>
      </div>

      <!-- 通知区：错误条 / 兜底引导 / 新版本，纵向堆叠避免互相遮挡 -->
      <div class="notices">
        <div v-if="errorBar" class="err-bar">
          <span>{{ errorBar }}</span>
          <button class="err-close" title="关闭" @click.stop="errorBar = ''">✕</button>
        </div>

        <!-- 在线翻译失败 → 引导下载本地引擎+模型（产品决策：告知 + 兜底） -->
        <div v-if="needLocalModel" class="cta-bar">
          <span>在线翻译不可用（可能被过滤或网络受限）。本地翻译可离线、无视屏蔽（约 4.4GB）。</span>
          <button class="btn small" :disabled="downloading" @click.stop="downloadLocalModel">
            {{ downloading ? `下载中 ${downloadPct}%` : "下载本地翻译" }}
          </button>
        </div>

        <!-- 新版本提示：应用内自动更新（下载完成后自动重启替换） -->
        <div v-if="updateInfo?.hasUpdate" class="update-bar">
          <span>新版本 v{{ updateInfo.latest }} 可用（当前 v{{ updateInfo.current }}）</span>
          <button v-if="updatePct === null" class="btn small" @click.stop="installUpdate">立即更新</button>
          <button v-else class="btn small" disabled>更新中 {{ updatePct }}%</button>
        </div>
      </div>

      <!-- 操作反馈 OSD（截图/快进快退等，2.5s 自动消失） -->
      <div v-if="toast" class="osd">{{ toast }}</div>
    </main>

    <!-- 控制栏：布局对齐标准播放器惯例（IINA/PotPlayer）——
         左：播放/快退/快进 ｜中：时间+进度 ｜右：倍速/音量/工具。
         图标语义：环形箭头+数字 = 跳转秒数（双三角留给「上一曲/下一曲」， playlist 接入前不用） -->
    <footer v-if="loaded" class="controls">
      <button class="ctl-btn" :title="paused ? '播放（空格）' : '暂停（空格）'" aria-label="播放或暂停" @click="togglePause">
        <svg v-if="paused" viewBox="0 0 24 24"><path d="M8 5v14l11-7z" /></svg>
        <svg v-else viewBox="0 0 24 24"><path d="M6 5h4v14H6zM14 5h4v14h-4z" /></svg>
      </button>
      <button class="ctl-btn" title="快退 10s（键盘 ← 为 5s）" aria-label="快退 10 秒" @click="seekBy(-10)">
        <svg viewBox="0 0 24 24"><path d="M12 5V1L7 6l5 5V7c3.31 0 6 2.69 6 6s-2.69 6-6 6-6-2.69-6-6H4c0 4.42 3.58 8 8 8s8-3.58 8-8-3.58-8-8-8z" /><text x="12" y="16.5" text-anchor="middle" font-size="9.5" font-weight="700" fill="currentColor" stroke="none">10</text></svg>
      </button>
      <button class="ctl-btn" title="快进 10s（键盘 → 为 5s）" aria-label="快进 10 秒" @click="seekBy(10)">
        <svg viewBox="0 0 24 24"><path d="M12 5V1l5 5-5 5V7c-3.31 0-6 2.69-6 6s2.69 6 6 6 6-2.69 6-6h2c0 4.42-3.58 8-8 8s-8-3.58-8-8 3.58-8 8-8z" /><text x="12" y="16.5" text-anchor="middle" font-size="9.5" font-weight="700" fill="currentColor" stroke="none">10</text></svg>
      </button>
      <span class="time">{{ fmtTime(displayTime) }}</span>
      <div
        ref="barRef"
        class="progress"
        @pointerdown="startDrag"
        @pointermove="onBarMove"
        @pointerup="endDrag"
        @pointercancel="endDrag"
        @pointerenter="hovering = true"
        @pointerleave="onBarLeave"
      >
        <div class="progress-fill" :style="{ width: progressPct }" />
        <div class="progress-thumb" :style="{ left: progressPct }" />
        <div v-if="hovering" class="progress-tip" :style="{ left: hoverPct }">
          {{ fmtTime(hoverTime) }}
        </div>
      </div>
      <span class="time">/ {{ fmtTime(duration) }}</span>
      <div class="speed-wrap">
        <button class="speed-btn" title="倍速（滚轮+Shift 微调，[ ] 快捷键）" @click.stop="speedPopOpen = !speedPopOpen">
          {{ fmtSpeed(speed) }}
        </button>
        <div v-if="speedPopOpen" class="speed-pop" @click.stop>
          <div class="speed-grid">
            <button
              v-for="s in SPEED_PRESETS"
              :key="s"
              class="speed-chip"
              :class="{ on: speed === s }"
              @click="setSpeed(s)"
            >{{ fmtSpeed(s) }}</button>
          </div>
          <div class="speed-grid">
            <button class="speed-chip" @click="setSpeed(speed - 0.1)">−0.1</button>
            <button class="speed-chip" @click="setSpeed(1)">重置</button>
            <button class="speed-chip" @click="setSpeed(speed + 0.1)">+0.1</button>
          </div>
        </div>
      </div>
      <button class="ctl-btn" :title="volume === 0 ? '取消静音' : '静音'" :aria-label="volume === 0 ? '取消静音' : '静音'" @click="toggleMute">
        <svg v-if="volume === 0" viewBox="0 0 24 24"><path d="M16.5 12c0-1.77-1.02-3.29-2.5-4.03v2.21l2.45 2.45c.03-.2.05-.41.05-.63zm2.5 0c0 .94-.2 1.82-.54 2.64l1.51 1.51A8.8 8.8 0 0 0 21 12c0-4.28-2.99-7.86-6.99-8.77v2.06c2.89.86 5 3.54 5 6.71zM4.27 3 3 4.27 7.73 9H3v6h4l5 5v-6.73l4.25 4.25c-.67.52-1.42.93-2.25 1.18v2.06a8.99 8.99 0 0 0 3.69-1.81L19.73 21 21 19.73l-9-9L4.27 3zM12 4 9.91 6.09 12 8.18V4z" /></svg>
        <svg v-else viewBox="0 0 24 24"><path d="M3 10v4h4l5 5V5L7 10H3zm13.5 2c0-1.77-1.02-3.29-2.5-4.03v8.05c1.48-.73 2.5-2.25 2.5-4.02zM14 3.23v2.06c2.89.86 5 3.54 5 6.71s-2.11 5.85-5 6.71v2.06c4-.91 7-4.49 7-8.77s-3-7.86-7-8.77z" /></svg>
      </button>
      <input
        class="vol"
        type="range"
        min="0"
        max="100"
        :value="volume"
        aria-label="音量"
        @input="changeVolume"
      />
      <button class="ctl-btn" title="截图到桌面（S）" aria-label="截图" @click="screenshot">
        <svg viewBox="0 0 24 24" fill-rule="evenodd"><path d="M9 3 7.17 5H4a2 2 0 0 0-2 2v11a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2h-3.17L15 3H9zm3 4.5a4.5 4.5 0 1 1 0 9 4.5 4.5 0 0 1 0-9zm0 2a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5z" /></svg>
      </button>
      <button class="ctl-btn" title="导出全片字幕 SRT（E）" aria-label="导出字幕" @click="exportSubs">
        <svg viewBox="0 0 24 24"><path d="M20 4H4a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V6a2 2 0 0 0-2-2zM4 12h4v2H4v-2zm10 6H4v-2h10v2zm6 0h-4v-2h4v2zm0-4H10v-2h10v2z" /></svg>
      </button>
      <button class="ctl-btn" title="全屏（F）" aria-label="全屏" @click="toggleFullscreen">
        <svg viewBox="0 0 24 24"><path d="M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z" /></svg>
      </button>
    </footer>

    <!-- 右键菜单 -->
    <div v-if="ctxOpen" class="ctx-mask" @click="ctxOpen = false" @contextmenu.prevent="ctxOpen = false" />
    <div v-if="ctxOpen" class="ctx-menu" :style="{ left: ctxX + 'px', top: ctxY + 'px' }">
      <button class="ctx-item" @click="ctxOpen = false; openFile()">打开文件…</button>
      <button class="ctx-item" @click="ctxOpen = false; togglePause()">{{ paused ? "播放" : "暂停" }}</button>
      <button class="ctx-item" @click="ctxOpen = false; screenshot()">截图</button>
      <button class="ctx-item" @click="ctxOpen = false; exportSubs()">导出全片字幕…</button>
      <button class="ctx-item" @click="ctxOpen = false; toggleFullscreen()">全屏</button>
      <div class="ctx-divider" />
      <div class="ctx-label">字幕模式</div>
      <button
        v-for="m in ['双语', '译文', '原文']"
        :key="m"
        class="ctx-item"
        :class="{ on: mode === m }"
        @click="setMode(m); ctxOpen = false"
      >{{ mode === m ? "✓ " : "" }}{{ m }}</button>
      <div class="ctx-divider" />
      <div class="ctx-label">倍速</div>
      <div class="ctx-speeds">
        <button
          v-for="s in SPEED_PRESETS"
          :key="s"
          class="ctx-speed"
          :class="{ on: speed === s }"
          @click="setSpeed(s); ctxOpen = false"
        >{{ fmtSpeed(s) }}</button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open, save } from "@tauri-apps/plugin-dialog";

const VIDEO_EXTS = ["mp4", "mkv", "avi", "mov", "webm", "m4v", "ts", "flv", "wmv", "rmvb"];

const SPEED_PRESETS = [0.5, 0.75, 1, 1.25, 1.5, 2];

const filename = ref("");
const videoPath = ref(""); // 完整路径：导出字幕时默认同目录同名
const status = ref("");
const loaded = ref(false);
const paused = ref(false);
const timePos = ref(0);
const duration = ref(0);
const volume = ref(100);
const speed = ref(1);
const displayTime = ref(0); // 拖动进度条时的临时显示值
const dragging = ref(false);

// 实时字幕状态（asr:// 事件）
interface Sub {
  start: number;
  end: number;
  text: string;
}
const subs = ref<Sub[]>([]);
const translations = ref<{ start: number; end: number; src: string; dst: string }[]>([]);
const mtAvailable = ref(true);
const settingsOpen = ref(false);
const speedPopOpen = ref(false);
// 拖文件悬停在窗口上时点亮拖放区
const dragHover = ref(false);
// 右键菜单
const ctxOpen = ref(false);
const ctxX = ref(0);
const ctxY = ref(0);
// 模型删除二次确认（3s 内再点一次才真正删）
const pendingDelete = ref("");
let pendingDeleteTimer: number | undefined;
// 播放中鼠标静止自动隐藏顶栏/控制栏
const idle = ref(false);
let idleTimer: number | undefined;
const engine = ref("auto");
const needLocalModel = ref(false);
const downloading = ref(false);
const downloadPct = ref(0);

// 模型管理与首启向导
interface ModelStatus {
  name: string;
  desc: string;
  installed: boolean;
  sizeMb: number;
}
const modelStatus = ref<ModelStatus[]>([]);
const downloadingName = ref("");
const whisperModel = ref("small");
const showWizard = ref(false);
const wizardDownloading = ref(false);

// 新版本检查（应用内自动更新）
const updateInfo = ref<{ current: string; latest: string; hasUpdate: boolean; notes?: string | null } | null>(null);
const updatePct = ref<number | null>(null); // null=未在更新中
const recognizing = ref<{ start: number; end: number } | null>(null);
// 识别指示条存活状态：进度事件后 5s 内为 true，超时自动消失（不再常驻）
const asrProgressLive = ref(false);
let asrProgressTimer: number | undefined;
const asrError = ref("");
const mode = ref<"译文" | "原文" | "双语">("双语");

// ---- 设置（四组：字幕 / 识别与翻译 / 播放 / 通用） ----
type SetTab = "subtitle" | "ai" | "playback" | "general";
const SETTING_TABS: { id: SetTab; label: string }[] = [
  { id: "subtitle", label: "字幕" },
  { id: "ai", label: "识别·翻译" },
  { id: "playback", label: "播放" },
  { id: "general", label: "通用" },
];
const SITE_URL = "https://livesub-player.shenzjd.com/";
const GITHUB_URL = "https://github.com/wu529778790/livesub-player";
const setTab = ref<SetTab>("subtitle");
const subFont = ref("medium"); // 字幕字号：small / medium / large
const subPos = ref("bottom"); // 字幕位置：bottom / raised / high
const asrLang = ref("auto"); // 识别语言：auto / zh / en / ja / ko
const rememberPlayer = ref(true); // 记住音量与倍速
const resumeLast = ref(true); // 启动时恢复上次播放
const autoUpdate = ref(true); // 启动时自动检查更新
const appVersion = ref("");
const checkingUpdate = ref(false);
let volSaveTimer: number | undefined;
let lastPosSave = 0;

const overlayClass = computed(() => [`of-${subFont.value}`, `op-${subPos.value}`]);

const activeSub = computed(() => {
  for (let i = subs.value.length - 1; i >= 0; i--) {
    const s = subs.value[i];
    if (s.start <= timePos.value && timePos.value <= s.end) return s;
  }
  return null;
});
// 译文：优先时间重叠，其次按源文本匹配（缓存命中时时间可能略有出入）
const activeTranslation = computed(() => {
  const ts = translations.value;
  for (let i = ts.length - 1; i >= 0; i--) {
    const t = ts[i];
    if (t.start <= timePos.value && timePos.value <= t.end) return t.dst;
  }
  const src = activeSub.value?.text;
  if (src) {
    for (let i = ts.length - 1; i >= 0; i--) {
      if (ts[i].src === src) return ts[i].dst;
    }
  }
  return "";
});

const barRef = ref<HTMLElement | null>(null);

// 进度条悬停预览
const hovering = ref(false);
const hoverTime = ref(0);
const hoverPct = computed(
  () => `${(hoverTime.value / (duration.value || 1)) * 100}%`
);

// OSD 操作反馈
const toast = ref("");
let toastTimer: number | undefined;
function showToast(msg: string) {
  toast.value = msg;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (toast.value = ""), 2500);
}

// 全局错误条：播放中 status 无载体，错误一度静默丢失——统一走这里（12s 自动消失，可手动关）
const errorBar = ref("");
let errorBarTimer: number | undefined;
function showError(msg: string) {
  errorBar.value = msg;
  window.clearTimeout(errorBarTimer);
  errorBarTimer = window.setTimeout(() => (errorBar.value = ""), 12000);
}

// ffmpeg 可用性（识别字幕的硬前提，缺失时欢迎页给出引导卡）
const ffmpegOk = ref(true);

let unlisten: UnlistenFn | null = null;

const progressPct = computed(() => {
  const d = duration.value || 1;
  return `${Math.min(100, ((dragging.value ? displayTime.value : timePos.value) / d) * 100)}%`;
});

// ---- mpv 状态事件（Rust 属性观察推送） ----
onMounted(async () => {
  unlisten = await listen<{
    timePos: number | null;
    duration: number | null;
    paused: boolean | null;
  }>("player://state", (e) => {
    const p = e.payload;
    if (p.duration != null) {
      duration.value = p.duration;
      loaded.value = loaded.value || p.duration > 0;
    }
    if (p.timePos != null && !dragging.value) {
      timePos.value = p.timePos;
      displayTime.value = p.timePos;
      // 播放进度每 3s 落盘一次（供「启动恢复上次播放」用）
      if (Math.abs(p.timePos - lastPosSave) >= 3) {
        lastPosSave = p.timePos;
        void invoke("cmd_set_setting", { key: "last_video_pos", value: String(Math.floor(p.timePos)) }).catch(() => {});
      }
    }
    if (p.paused != null) paused.value = p.paused;
  });
  // 初始音量/倍速
  const st = await invoke<{ timePos: number | null; duration: number | null; paused: boolean | null }>("cmd_player_get_state").catch(() => null);
  if (st?.duration) {
    loaded.value = true;
    duration.value = st.duration;
    if (st.timePos != null) timePos.value = st.timePos;
    if (st.paused != null) paused.value = st.paused;
  }
  window.addEventListener("keydown", onKey);
  // asr 事件
  await listen<Sub>("asr://subtitle", (e) => {
    subs.value.push(e.payload);
  });
  await listen<{ start: number; end: number }>("asr://progress", (e) => {
    recognizing.value = e.payload;
    asrProgressLive.value = true;
    window.clearTimeout(asrProgressTimer);
    asrProgressTimer = window.setTimeout(() => (asrProgressLive.value = false), 5000);
  });
  await listen<string>("asr://error", (e) => {
    asrError.value = String(e.payload);
  });
  await listen<string>("player://error", (e) => {
    // mpv 异步播放失败（坏文件/不支持的编码）：loadfile 同步成功但 END_FILE 报错
    showError(String(e.payload));
  });
  await listen<{
    start: number;
    end: number;
    src: string;
    dst: string;
  }>("asr://translation", (e) => {
    translations.value.push(e.payload);
  });
  // 全片字幕导出任务（异步识别整个视频）
  await listen<{ percent: number }>("export://progress", (e) => {
    exporting.value = Math.round(e.payload.percent);
  });
  await listen<{ srt: string }>("export://done", async (e) => {
    exporting.value = null;
    await saveExportedSrt(e.payload.srt);
  });
  await listen<string>("export://error", (e) => {
    exporting.value = null;
    if (String(e.payload) === "已取消") showToast("已取消导出");
    else showError(`导出失败: ${e.payload}`);
  });
  await listen<string>("mt://unavailable", () => {
    mtAvailable.value = false;
    showToast("本地翻译暂不可用，可在设置中检查引擎与模型");
  });
  await listen("mt://available", () => {
    mtAvailable.value = true;
  });
  await listen<string>("mt://need-local-model", (e) => {
    void e;
    needLocalModel.value = true;
  });
  await listen<{ name: string; downloadedMb: number; totalMb: number }>(
    "models://progress",
    (e) => {
      downloading.value = true;
      downloadingName.value = e.payload.name;
      downloadPct.value = Math.min(
        100,
        Math.round((e.payload.downloadedMb / (e.payload.totalMb || 1)) * 100)
      );
    }
  );
  await listen<{ name: string }>("models://done", async () => {
    downloading.value = false;
    downloadingName.value = "";
    needLocalModel.value = false;
    await refreshModelStatus();
    // 下载队列：上一个完成才启动下一个（后端同时只允许一个下载任务）
    if (downloadQueue.value.length > 0) {
      downloadQueue.value = downloadQueue.value.slice(1);
      pumpQueue();
      return;
    }
    if (wizardDownloading.value) {
      wizardDownloading.value = false;
      // 识别模型已装好即视为完成首启引导（M3：向导卡片不能再挂着）
      if (modelStatus.value.find((m) => m.name === "whisper-small")?.installed) {
        showWizard.value = false;
        void invoke("cmd_set_setting", { key: "wizard_done", value: "1" }).catch(() => {});
        showToast("模型就绪，拖入视频开始看片");
        return;
      }
    }
    showToast("模型下载完成");
  });
  await listen<string>("models://error", (e) => {
    downloading.value = false;
    downloadingName.value = "";
    downloadQueue.value = [];
    wizardDownloading.value = false;
    showError(`模型下载失败: ${e.payload}`);
  });
  engine.value = await invoke<string>("cmd_mt_get_engine");
  whisperModel.value = (await invoke<string>("cmd_get_setting", { key: "whisper_model" })) || "small";
  // 完整设置装载（缺省值与 UI 选项一一对应）
  subFont.value = (await invoke<string>("cmd_get_setting", { key: "sub_font" })) || "medium";
  subPos.value = (await invoke<string>("cmd_get_setting", { key: "sub_pos" })) || "bottom";
  asrLang.value = (await invoke<string>("cmd_get_setting", { key: "asr_language" })) || "auto";
  mode.value =
    ((await invoke<string>("cmd_get_setting", { key: "sub_mode" })) as "译文" | "原文" | "双语") || "双语";
  rememberPlayer.value = ((await invoke<string>("cmd_get_setting", { key: "remember_player" })) ?? "1") !== "0";
  resumeLast.value = ((await invoke<string>("cmd_get_setting", { key: "resume_last" })) ?? "1") !== "0";
  autoUpdate.value = ((await invoke<string>("cmd_get_setting", { key: "auto_update" })) ?? "1") !== "0";
  appVersion.value = await getVersion().catch(() => "");
  ffmpegOk.value = await invoke<boolean>("cmd_check_ffmpeg").catch(() => true);
  void applyPersistedPlayer();
  await refreshModelStatus();
  // 首启向导：识别模型未装且未跳过过
  showWizard.value =
    !modelStatus.value.find((m) => m.name === "whisper-small")?.installed &&
    (await invoke<string>("cmd_get_setting", { key: "wizard_done" })) !== "1";
  // 新版本检查（设置可关；静默失败即可）
  if (autoUpdate.value) {
    updateInfo.value = await invoke<{
      current: string;
      latest: string;
      hasUpdate: boolean;
      notes?: string | null;
    }>("cmd_check_update").catch(() => null);
  }
  await listen<number>("update://progress", (e) => {
    updatePct.value = e.payload;
  });
  // 启动恢复上次播放（延迟 1.2s：避开 mpv 初始化与首帧渲染，且不打断自动化钩子）
  window.setTimeout(async () => {
    if (loaded.value || !resumeLast.value) return;
    const last = await invoke<string>("cmd_get_setting", { key: "last_video" }).catch(() => null);
    if (!last) return;
    status.value = "正在恢复上次播放…";
    await loadVideo(last);
    const pos = Number(await invoke<string>("cmd_get_setting", { key: "last_video_pos" }).catch(() => null)) || 0;
    if (pos > 5) {
      await invoke("cmd_player_seek", { target: pos, absolute: true }).catch(() => {});
    }
    showToast(`已恢复上次播放：${filename.value}`);
  }, 1200);
});

async function installUpdate() {
  if (updatePct.value !== null) return;
  updatePct.value = 0;
  const ok = await invoke("cmd_install_update").catch((e) => {
    showError(String(e));
    updatePct.value = null; // 失败后恢复「立即更新」按钮可点
    return false;
  });
  if (!ok && updatePct.value !== null && updatePct.value < 100) updatePct.value = null;
  // 成功路径：安装完成后应用自动重启替换，无需收尾
}

async function refreshModelStatus() {
  modelStatus.value = await invoke<ModelStatus[]>("cmd_model_status");
}

// 下载队列：后端同时只允许一个下载任务，前端排队串行（向导「下载全部」/ 引导条都用它）
const downloadQueue = ref<string[]>([]);

function enqueueDownload(names: string[]) {
  downloadQueue.value.push(...names);
  pumpQueue();
}

function pumpQueue() {
  if (downloading.value || downloadQueue.value.length === 0) return;
  void download(downloadQueue.value[0]);
}

async function download(name: string) {
  if (downloading.value) return;
  await invoke("cmd_model_download", { name }).catch((e) => {
    downloadQueue.value = [];
    showError(String(e));
  });
}

async function removeModel(name: string) {
  // 二次确认：3s 内再点一次才真正删除（防误触删掉几 GB 的模型）
  if (pendingDelete.value !== name) {
    pendingDelete.value = name;
    window.clearTimeout(pendingDeleteTimer);
    pendingDeleteTimer = window.setTimeout(() => (pendingDelete.value = ""), 3000);
    return;
  }
  pendingDelete.value = "";
  window.clearTimeout(pendingDeleteTimer);
  await invoke("cmd_model_delete", { name }).catch((e) => showError(String(e)));
  await refreshModelStatus();
  showToast("模型已删除");
}

async function changeWhisperModel(e: Event) {
  const v = (e.target as HTMLSelectElement).value;
  whisperModel.value = v;
  await invoke("cmd_set_setting", { key: "whisper_model", value: v }).catch((e) =>
    showError(String(e))
  );
  showToast("识别档位已保存，下次播放生效");
}

const MODEL_LABELS: Record<string, string> = {
  "whisper-small": "识别模型",
  "whisper-base": "识别模型（快速档）",
  "translation-engine": "翻译引擎",
  translation: "翻译模型",
};

const wizardButtonLabel = computed(() => {
  if (!wizardDownloading.value) return "下载全部模型（识别 + 本地翻译）";
  const label = MODEL_LABELS[downloadingName.value] || "模型";
  return `下载中 ${downloadPct}%（${label}）`;
});

function wizardDownloadAll() {
  wizardDownloading.value = true;
  enqueueDownload(["whisper-small", "translation-engine", "translation"]);
}

async function wizardDismiss() {
  await invoke("cmd_set_setting", { key: "wizard_done", value: "1" });
  showWizard.value = false;
}

async function changeEngine(e: Event) {
  const v = (e.target as HTMLSelectElement).value;
  engine.value = v;
  await invoke("cmd_mt_set_engine", { engine: v }).catch((e) => showError(String(e)));
  if (v === "local") showToast("已切换为仅本地翻译：需装好引擎与模型");
}

// 字幕模式统一入口：右键菜单 / 快捷键 / 设置页都走这里，保证落盘一致
function setMode(m: "译文" | "原文" | "双语") {
  if (mode.value === m) return;
  mode.value = m;
  void saveSetting("sub_mode", m);
  showToast(`字幕模式：${m}`);
}

// ---- 设置项处理器（四组） ----
async function saveSetting(key: string, value: string) {
  await invoke("cmd_set_setting", { key, value }).catch((e) => showError(String(e)));
}

function changeSubFont(e: Event) {
  subFont.value = (e.target as HTMLSelectElement).value;
  void saveSetting("sub_font", subFont.value);
}

function changeSubPos(e: Event) {
  subPos.value = (e.target as HTMLSelectElement).value;
  void saveSetting("sub_pos", subPos.value);
}

function changeSubMode(e: Event) {
  setMode((e.target as HTMLSelectElement).value as "译文" | "原文" | "双语");
}

function changeAsrLang(e: Event) {
  asrLang.value = (e.target as HTMLSelectElement).value;
  void saveSetting("asr_language", asrLang.value);
  showToast(
    asrLang.value === "auto"
      ? "识别语言：自动检测"
      : `识别语言固定为 ${asrLang.value.toUpperCase()}，下个识别会话生效`
  );
}

function toggleRemember() {
  rememberPlayer.value = !rememberPlayer.value;
  void saveSetting("remember_player", rememberPlayer.value ? "1" : "0");
}

function toggleResume() {
  resumeLast.value = !resumeLast.value;
  void saveSetting("resume_last", resumeLast.value ? "1" : "0");
}

function toggleAutoUpdate() {
  autoUpdate.value = !autoUpdate.value;
  void saveSetting("auto_update", autoUpdate.value ? "1" : "0");
}

async function manualCheckUpdate() {
  if (checkingUpdate.value) return;
  checkingUpdate.value = true;
  const info = await invoke<{
    current: string;
    latest: string;
    hasUpdate: boolean;
    notes?: string | null;
  }>("cmd_check_update").catch(() => null);
  checkingUpdate.value = false;
  if (!info) {
    showToast("检查更新失败，请检查网络");
    return;
  }
  updateInfo.value = info;
  if (!info.hasUpdate) showToast(`已是最新版本 v${info.current}`);
}

async function openLogDir() {
  await invoke("cmd_open_log_dir").catch((e) => showError(String(e)));
}

async function openUrl(url: string) {
  await invoke("cmd_open_url", { url }).catch((e) => showError(String(e)));
}

function copyFfmpegHint() {
  navigator.clipboard
    .writeText("brew install ffmpeg")
    .then(() => showToast("已复制，粘贴到终端执行后重启应用"))
    .catch(() => showError("复制失败，请手动输入：brew install ffmpeg"));
}

// 音量/倍速持久化：音量拖动高频触发，防抖 800ms 落盘
function persistVolumeDebounced() {
  if (!rememberPlayer.value) return;
  window.clearTimeout(volSaveTimer);
  volSaveTimer = window.setTimeout(() => {
    void invoke("cmd_set_setting", { key: "volume", value: String(Math.round(volume.value)) }).catch(() => {});
  }, 800);
}

// 启动/加载视频时回放记忆的音量倍速（mpv 未就绪时静默失败，loadVideo 后会再应用一次）
async function applyPersistedPlayer() {
  if (!rememberPlayer.value) return;
  const v = Number(await invoke<string>("cmd_get_setting", { key: "volume" }).catch(() => null));
  if (Number.isFinite(v) && v > 0 && v <= 100) {
    volume.value = v;
    await invoke("cmd_player_set_volume", { volume: v }).catch(() => {});
  }
  const sp = Number(await invoke<string>("cmd_get_setting", { key: "speed" }).catch(() => null));
  if (Number.isFinite(sp) && sp >= 0.25 && sp <= 3) {
    speed.value = Math.round(sp * 10) / 10;
    await invoke("cmd_player_set_speed", { speed: speed.value }).catch(() => {});
  }
}

async function downloadLocalModel() {
  if (downloading.value) return;
  // 引擎 + 模型二件套：先引擎（12MB）后模型（4.4GB），队列串行
  enqueueDownload(["translation-engine", "translation"]);
}

onUnmounted(() => {
  unlisten?.();
  window.removeEventListener("keydown", onKey);
  window.clearTimeout(asrProgressTimer);
});

// ---- 控制 ----
async function togglePause() {
  try {
    paused.value = await invoke<boolean>("cmd_player_toggle_pause");
  } catch (e) {
    showError(String(e));
  }
}

async function toggleFullscreen() {
  try {
    await invoke("cmd_player_toggle_fullscreen");
  } catch (e) {
    showError(String(e));
  }
}

async function seekAbsolute(t: number) {
  try {
    await invoke("cmd_player_seek", { target: t, absolute: true });
    timePos.value = t;
    displayTime.value = t;
  } catch (e) {
    showError(String(e));
  }
}

async function seekBy(delta: number) {
  const next = Math.min(duration.value, Math.max(0, timePos.value + delta));
  await seekAbsolute(next);
  showToast(`${delta > 0 ? "快进" : "快退"} ${Math.abs(delta)}s → ${fmtTime(next)}`);
}

async function screenshot() {
  try {
    const p = await invoke<string>("cmd_player_screenshot");
    showToast(`截图已保存：${String(p).split("/").pop() || "桌面"}`);
  } catch (e) {
    showError(String(e));
  }
}

// ---- 导出全片字幕：后台异步识别整个视频（0 → 片尾），完成后选位置保存 ----
const exporting = ref<number | null>(null); // null=空闲，否则为百分比
const exportDefaultPath = ref("");

async function exportSubs() {
  if (exporting.value !== null) {
    showToast("全片识别已在进行中");
    return;
  }
  if (!loaded.value || !videoPath.value || !duration.value) {
    showToast("请先打开视频");
    return;
  }
  const dir = videoPath.value.split("/").slice(0, -1).join("/");
  const stem = (filename.value || "subtitles").replace(/\.[^.]+$/, "");
  exportDefaultPath.value = dir ? `${dir}/${stem}.srt` : `${stem}.srt`;
  const ok = await invoke("cmd_export_full_srt", {
    path: videoPath.value,
    duration: duration.value,
  }).catch((e) => {
    showToast(String(e));
    return null;
  });
  if (ok !== null) showToast("开始识别全片字幕，完成后选择保存位置");
}

async function cancelExport() {
  await invoke("cmd_cancel_export").catch(() => {});
}

async function saveExportedSrt(srt: string) {
  const target = await save({
    defaultPath: exportDefaultPath.value,
    filters: [{ name: "SRT 字幕", extensions: ["srt"] }],
  });
  if (!target) {
    // 保存对话框点取消：自动落到桌面兜底——全片识别可能等了几十分钟到几小时，
    // 不能因为一次误点就全部丢弃
    const auto = await invoke<string>("cmd_export_srt_auto", {
      content: srt,
      videoPath: videoPath.value,
    }).catch(() => null);
    if (auto) showToast(`已自动保存到桌面：${String(auto).split("/").pop()}`);
    else showError("字幕未能保存，请重新导出");
    return;
  }
  await invoke("cmd_export_srt", { path: target, content: srt }).catch((e) =>
    showError(String(e))
  );
  showToast("字幕已保存");
}

async function changeVolume(e: Event) {
  const v = Number((e.target as HTMLInputElement).value);
  volume.value = v;
  await invoke("cmd_player_set_volume", { volume: v }).catch((e) => showError(String(e)));
  persistVolumeDebounced();
}

function fmtSpeed(s: number): string {
  return `${Math.round(s * 100) / 100}x`;
}

async function setSpeed(s: number) {
  // 0.1 步进、钳制 0.25~3（mpv 端下限 0.25，钳到一致避免 UI 显示 0.1 实际 0.25），
  // 四舍五入消除浮点误差
  const next = Math.min(3, Math.max(0.25, Math.round(s * 10) / 10));
  speed.value = next;
  speedPopOpen.value = false;
  await invoke("cmd_player_set_speed", { speed: next }).catch((e) => showError(String(e)));
  if (rememberPlayer.value) {
    void invoke("cmd_set_setting", { key: "speed", value: String(next) }).catch(() => {});
  }
  showToast(`倍速 ${fmtSpeed(next)}`);
}

// ---- 滚轮：音量；Shift+滚轮：倍速微调（PotPlayer 习惯） ----
async function onWheel(e: WheelEvent) {
  if (!loaded.value) return;
  if (e.shiftKey) {
    await setSpeed(speed.value + (e.deltaY < 0 ? 0.1 : -0.1));
  } else {
    const v = Math.min(100, Math.max(0, volume.value + (e.deltaY < 0 ? 5 : -5)));
    await changeVolumeTo(v);
    showToast(`音量 ${Math.round(v)}`);
  }
}

// ---- 右键菜单（防溢出屏幕） ----
function openCtx(e: MouseEvent) {
  ctxX.value = Math.min(e.clientX, window.innerWidth - 230);
  ctxY.value = Math.min(e.clientY, window.innerHeight - 420);
  ctxOpen.value = true;
}

// ---- 播放中鼠标静止 2.5s 隐藏顶栏/控制栏，动一下即恢复 ----
function onUiActivity() {
  idle.value = false;
  window.clearTimeout(idleTimer);
  idleTimer = window.setTimeout(() => {
    if (
      loaded.value && !paused.value && !dragging.value &&
      !settingsOpen.value && !speedPopOpen.value && !ctxOpen.value
    ) {
      idle.value = true;
    }
  }, 2500);
}
const uiHidden = computed(
  () =>
    idle.value && loaded.value && !paused.value && !dragging.value &&
    !settingsOpen.value && !speedPopOpen.value && !ctxOpen.value
);

// ---- 进度条拖动 + 悬停预览 ----
function ratioAt(e: PointerEvent): number {
  const bar = barRef.value;
  if (!bar || !duration.value) return 0;
  const rect = bar.getBoundingClientRect();
  return Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
}
function startDrag(e: PointerEvent) {
  dragging.value = true;
  (e.target as HTMLElement).setPointerCapture(e.pointerId);
  displayTime.value = ratioAt(e) * duration.value;
}
function onBarMove(e: PointerEvent) {
  hoverTime.value = ratioAt(e) * duration.value;
  if (dragging.value) displayTime.value = hoverTime.value;
}
function onBarLeave() {
  hovering.value = false;
}
function endDrag(e: PointerEvent) {
  if (!dragging.value) return;
  displayTime.value = ratioAt(e) * duration.value;
  dragging.value = false;
  seekAbsolute(displayTime.value);
}

// ---- 打开文件 ----
async function openFile() {
  const path = await open({
    multiple: false,
    filters: [{ name: "视频", extensions: VIDEO_EXTS }],
  });
  if (typeof path === "string") await loadVideo(path);
}

async function onDrop(e: DragEvent) {
  const files = e.dataTransfer?.files;
  if (!files?.length) return;
  // Tauri 拖放：用 webviewWindow 事件拿真实路径（file:// 输入只兜底）
  const path = decodeURIComponent((files[0] as File & { path?: string }).path || "");
  if (path) await loadVideo(path);
}

async function loadVideo(path: string) {
  // 换片先清空上一部的字幕/错误状态：旧片字幕串到新片、红条永不清除都是这个坑
  subs.value = [];
  translations.value = [];
  asrError.value = "";
  recognizing.value = null;
  errorBar.value = "";
  status.value = "加载中…";
  try {
    await invoke("cmd_player_load", { path });
    videoPath.value = path;
    filename.value = path.split("/").pop() || path;
    loaded.value = true;
    status.value = "";
    // 记住本次播放的文件（启动恢复用）+ 回放记忆的音量倍速
    void invoke("cmd_set_setting", { key: "last_video", value: path }).catch(() => {});
    void applyPersistedPlayer();
  } catch (e) {
    // 同步失败（mpv 初始化/loadfile 被拒）：停在欢迎页并给出可见报错
    status.value = "";
    showError(String(e));
  }
}

// ---- 输入接管（技术方案决策 A：视频区点击全在 WebView） ----
function onStageClick() {
  if (speedPopOpen.value) {
    speedPopOpen.value = false;
    return;
  }
  if (loaded.value) togglePause();
}

function onKey(e: KeyboardEvent) {
  // 不抢系统/应用快捷键：⌘S、⌘F、Ctrl+C 等组合键一律放行
  if (e.metaKey || e.ctrlKey || e.altKey) return;
  onUiActivity(); // 纯键盘操作也要唤醒自动隐藏的控制栏
  if (settingsOpen.value) {
    if (e.key === "Escape") settingsOpen.value = false;
    return; // 设置开着时不响应播放快捷键（1/2/3、空格会误触）
  }
  if (!loaded.value) return;
  switch (e.key) {
    case " ":
      e.preventDefault();
      togglePause();
      break;
    case "1":
      setMode("译文");
      break;
    case "2":
      setMode("原文");
      break;
    case "3":
      setMode("双语");
      break;
    case "ArrowLeft":
      seekBy(-5);
      break;
    case "ArrowRight":
      seekBy(5);
      break;
    case "ArrowUp":
      e.preventDefault();
      changeVolumeTo(Math.min(100, volume.value + 5));
      break;
    case "ArrowDown":
      e.preventDefault();
      changeVolumeTo(Math.max(0, volume.value - 5));
      break;
    case "f":
    case "F":
      toggleFullscreen();
      break;
    case "s":
    case "S":
      screenshot();
      break;
    case "e":
    case "E":
      exportSubs();
      break;
    case "[":
      setSpeed(speed.value - 0.1);
      break;
    case "]":
      setSpeed(speed.value + 0.1);
      break;
    case "Escape":
      ctxOpen.value = false;
      speedPopOpen.value = false;
      settingsOpen.value = false;
      break;
  }
}

async function changeVolumeTo(v: number) {
  volume.value = v;
  await invoke("cmd_player_set_volume", { volume: v }).catch((e) => showError(String(e)));
  persistVolumeDebounced();
}

// 喇叭图标点按 = 静音/恢复（标准播放器惯例；滚轮/滑杆仍直接调值）
let lastVolume = 100;
function toggleMute() {
  if (volume.value > 0) {
    lastVolume = volume.value;
    void changeVolumeTo(0);
  } else {
    void changeVolumeTo(lastVolume || 100);
  }
}

function fmtTime(t: number): string {
  if (!Number.isFinite(t)) return "0:00";
  const s = Math.floor(t % 60);
  const m = Math.floor((t / 60) % 60);
  const h = Math.floor(t / 3600);
  const mm = h > 0 ? String(m).padStart(2, "0") : String(m);
  return h > 0 ? `${h}:${mm}:${String(s).padStart(2, "0")}` : `${mm}:${String(s).padStart(2, "0")}`;
}

// 注册 Tauri 原生拖放（拿真实文件路径 + 驱动拖放区高亮）
onMounted(async () => {
  await getCurrentWebviewWindow().onDragDropEvent((ev) => {
    const t = ev.payload.type;
    if (t === "drop") {
      dragHover.value = false;
      if (ev.payload.paths.length > 0) loadVideo(ev.payload.paths[0]);
    } else if (t === "enter" || t === "over") {
      dragHover.value = true;
    } else {
      dragHover.value = false;
    }
  });
});
</script>

<style scoped>
.shell {
  width: 100vw;
  height: 100vh;
  display: flex;
  flex-direction: column;
  color: #f2f2f2;
  font-family: -apple-system, "PingFang SC", "Microsoft YaHei", sans-serif;
  user-select: none;
}

.topbar {
  display: flex;
  align-items: center;
  gap: 12px;
  /* 内容中心对齐红绿灯按钮（Overlay 模式下红绿灯中心在 ~15px）；
     左侧留出红绿灯悬浮空间（titleBarStyle: Overlay） */
  height: 26px;
  padding: 3px 16px 0 84px;
  background: rgba(16, 16, 20, 0.72);
  -webkit-app-region: drag;
}

.topbar-icon {
  width: 18px;
  height: 18px;
  flex-shrink: 0;
  border-radius: 5px;
}

.filename {
  flex: 1;
  font-size: 12px;
  color: #b9b9c4;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* macOS 工具栏风格：无边框文字按钮，悬停出底色（实测三元素中心线已对齐，
   之前的「不协调」来自高对比胶囊造型浮在角落） */
.bar-btn {
  -webkit-app-region: no-drag;
  padding: 5px 10px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: #c9c9d2;
  cursor: pointer;
  font-size: 12px;
  line-height: 1.4;
}

.bar-btn:hover {
  background: rgba(255, 255, 255, 0.12);
  color: #fff;
}

/* 关键：视频区域背景透明，透出 mpv 子窗口 */
.stage {
  flex: 1;
  position: relative;
  background: transparent;
  cursor: default;
}

/* 窗口本身 transparent: true（字幕 overlay 依赖），未加载视频时 mpv 子窗口为空，
   整窗会看穿到桌面/其他应用——此时整窗（含顶栏）给不透明底色；加载后必须保持透明透出画面 */
.shell.is-empty {
  background: radial-gradient(1100px 620px at 50% -8%, #1b1b26, #0e0e13 70%);
}

/* ---- 欢迎页（未加载视频） ---- */
.welcome {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 30px;
  padding: 24px;
  text-align: center;
}

.welcome-brand {
  display: flex;
  flex-direction: column;
  align-items: center;
}

.welcome-logo {
  width: 88px;
  height: 88px;
  display: block;
  filter: drop-shadow(0 10px 28px rgba(0, 0, 0, 0.45));
}

.welcome-title {
  margin: 16px 0 0;
  font-size: 24px;
  font-weight: 700;
  letter-spacing: 0.02em;
  color: #fff;
}

.welcome-tagline {
  margin: 6px 0 0;
  font-size: 13px;
  color: #8f8f9a;
}

.dropzone {
  width: min(520px, 84%);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 7px;
  padding: 40px 32px;
  border: 1.5px dashed rgba(255, 255, 255, 0.22);
  border-radius: 16px;
  background: rgba(255, 255, 255, 0.03);
  color: #cfcfd8;
  cursor: pointer;
  transition: border-color 0.18s, background 0.18s, box-shadow 0.18s, transform 0.18s;
}

.dropzone:hover,
.dropzone.hot {
  border-color: #ffd479;
  background: rgba(255, 212, 121, 0.07);
  box-shadow: 0 0 0 1px rgba(255, 212, 121, 0.25), 0 12px 40px rgba(255, 212, 121, 0.08);
}

.dropzone.hot {
  transform: scale(1.015);
}

.dz-icon {
  width: 34px;
  height: 34px;
  color: #ffd479;
  margin-bottom: 4px;
}

.dz-main {
  font-size: 15px;
  font-weight: 600;
  color: #fff;
}

.dz-sub {
  font-size: 12px;
  color: #8f8f9a;
}

/* 首启向导卡片 */
.wizard-card {
  width: min(520px, 84%);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 10px;
  padding: 18px 24px;
  border-radius: 14px;
  background: rgba(255, 212, 121, 0.05);
  border: 1px solid rgba(255, 212, 121, 0.28);
}

.wizard-title {
  margin: 0;
  font-size: 14px;
  font-weight: 700;
  color: #ffd479;
}

.wizard-text {
  margin: 0;
  line-height: 1.8;
  font-size: 12px;
  color: #b9b9c4;
}

.wizard-text code {
  background: rgba(255, 255, 255, 0.12);
  padding: 1px 6px;
  border-radius: 4px;
  color: #ffe1a1;
  font-family: ui-monospace, "SF Mono", Menlo, monospace;
}

.btn {
  padding: 8px 18px;
  border-radius: 6px;
  border: none;
  background: #ffd479;
  color: #1c1508;
  font-weight: 600;
  cursor: pointer;
  font-size: 13px;
}

.btn.ghost {
  background: rgba(255, 255, 255, 0.14);
  color: #fff;
  font-weight: 400;
}

.status {
  color: #ffd479;
  min-height: 16px;
}

/* 字幕 Overlay：底部居中，黑描边白字；字号/位置由设置档位类覆盖 */
.subtitle-overlay {
  position: absolute;
  left: 50%;
  bottom: 8%;
  transform: translateX(-50%);
  max-width: 80%;
  text-align: center;
  pointer-events: none;
}

/* 字幕字号档位（原文 28 / 译文 24 为中档基准） */
.of-small .sub-src {
  font-size: 22px;
}
.of-small .sub-dst {
  font-size: 19px;
}
.of-large .sub-src {
  font-size: 36px;
}
.of-large .sub-dst {
  font-size: 31px;
}

/* 字幕位置档位 */
.op-raised {
  bottom: 16%;
}
.op-high {
  bottom: 26%;
}

.sub-src {
  font-size: 28px;
  font-weight: 700;
  color: #fff;
  text-shadow:
    0 0 4px #000, 0 0 4px #000,
    2px 2px 2px #000, -2px 2px 2px #000,
    2px -2px 2px #000, -2px -2px 2px #000;
  line-height: 1.4;
}

.sub-dst {
  margin-top: 6px;
  font-size: 24px;
  font-weight: 600;
  color: #ffd479;
  text-shadow:
    0 0 4px #000, 0 0 4px #000,
    2px 2px 2px #000, -2px 2px 2px #000,
    2px -2px 2px #000, -2px -2px 2px #000;
  line-height: 1.4;
}

.sub-dst.pending {
  font-size: 14px;
  font-weight: 400;
  color: rgba(255, 255, 255, 0.55);
}

/* 识别进度指示 */
.asr-progress {
  position: absolute;
  top: 14px;
  right: 16px;
  padding: 4px 12px;
  border-radius: 999px;
  background: rgba(0, 0, 0, 0.5);
  color: rgba(255, 255, 255, 0.75);
  font-size: 11px;
  pointer-events: none;
  font-variant-numeric: tabular-nums;
}

/* 全片导出进度：在识别进度下方，取消按钮可点 */
.export-pill {
  top: 48px;
  display: flex;
  align-items: center;
  gap: 8px;
  pointer-events: auto;
}

.asr-error {
  position: absolute;
  top: 14px;
  left: 16px;
  padding: 6px 12px;
  border-radius: 8px;
  background: rgba(120, 30, 30, 0.8);
  color: #ffd7d7;
  font-size: 12px;
  max-width: 60%;
  pointer-events: none;
}

/* 设置抽屉 */
.drawer-mask {
  position: fixed;
  inset: 0;
  z-index: 30;
  background: rgba(0, 0, 0, 0.35);
}

.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.2s;
}

.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}

.drawer {
  position: fixed;
  top: 0;
  right: 0;
  bottom: 0;
  width: 320px;
  z-index: 31;
  padding: 16px 18px;
  background: rgba(22, 22, 28, 0.97);
  border-left: 1px solid rgba(255, 255, 255, 0.12);
  box-shadow: -8px 0 24px rgba(0, 0, 0, 0.45);
  transform: translateX(105%);
  transition: transform 0.22s ease;
  overflow-y: auto;
}

.drawer.open {
  transform: translateX(0);
}

.drawer-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  font-size: 15px;
  font-weight: 700;
  margin-bottom: 10px;
}

.drawer-close {
  background: none;
  border: none;
  color: #9a9aa5;
  font-size: 14px;
  cursor: pointer;
  padding: 2px 6px;
}

.drawer-close:hover {
  color: #fff;
}

/* 设置页签 */
.set-tabs {
  display: flex;
  gap: 4px;
  margin-bottom: 10px;
  padding-bottom: 10px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}

.set-tab {
  flex: 1;
  padding: 6px 2px;
  background: none;
  border: none;
  border-radius: 6px;
  color: #9a9aa5;
  font-size: 12px;
  cursor: pointer;
}

.set-tab:hover {
  color: #fff;
}

.set-tab.on {
  background: rgba(255, 212, 121, 0.14);
  color: #ffd479;
  font-weight: 600;
}

/* 开关（boolean 设置项） */
.switch {
  position: relative;
  flex: none;
  width: 38px;
  height: 22px;
  padding: 0;
  border: none;
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.18);
  cursor: pointer;
  transition: background 0.15s;
}

.switch i {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  background: #fff;
  transition: left 0.15s;
}

.switch.on {
  background: #ffd479;
}

.switch.on i {
  left: 18px;
}

/* 带说明文字的设置行 */
.set-label {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.set-label small {
  font-size: 11px;
  color: #8f8f9a;
}

.set-value {
  color: #9a9aa5;
  font-size: 13px;
}

.set-btn {
  display: block;
  width: 100%;
  margin-top: 8px;
  text-align: center;
}

/* 项目链接（官网 / GitHub） */
.link-btn {
  background: none;
  border: none;
  padding: 0;
  color: #ffd479;
  font-size: 12px;
  cursor: pointer;
  word-break: break-all;
  text-align: right;
}

.link-btn:hover {
  text-decoration: underline;
}

.drawer-section {
  margin: 14px 0 8px;
  font-size: 11px;
  font-weight: 700;
  color: #8f8f9a;
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

.set-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  font-size: 13px;
  margin: 0 0 10px;
}

.set-row select {
  background: rgba(255, 255, 255, 0.1);
  color: #fff;
  border: 1px solid rgba(255, 255, 255, 0.2);
  border-radius: 6px;
  padding: 4px 8px;
  font-size: 12px;
}

.set-hint {
  margin: 8px 0 0;
  font-size: 11px;
  color: #8f8f9a;
  max-width: 320px;
}

.model-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 8px;
  font-size: 11px;
}

.model-desc {
  flex: 1;
  color: #b9b9c4;
}

.model-ok {
  color: #7ddb8a;
  white-space: nowrap;
}

/* 首启向导按钮组 */
.wizard-actions {
  display: flex;
  flex-direction: column;
  gap: 10px;
  width: 100%;
}

/* 通知区：错误条/引导条/更新条纵向堆叠，避免同位互相遮挡 */
.notices {
  position: absolute;
  top: 50px;
  left: 50%;
  transform: translateX(-50%);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  z-index: 15;
  pointer-events: none;
  max-width: 86%;
}

.notices > div {
  pointer-events: auto;
}

/* 全局错误条：错误曾只显示在欢迎页一行小字里，播放中完全不可见 */
.err-bar {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 12px 8px 16px;
  border-radius: 10px;
  background: rgba(110, 28, 28, 0.92);
  border: 1px solid rgba(255, 130, 130, 0.45);
  font-size: 12px;
  color: #ffd9d9;
}

.err-close {
  background: none;
  border: none;
  color: rgba(255, 217, 217, 0.7);
  cursor: pointer;
  font-size: 12px;
  padding: 2px 4px;
}

.err-close:hover {
  color: #fff;
}

/* 本地模型下载引导条 */
.cta-bar {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 16px;
  border-radius: 10px;
  background: rgba(70, 50, 12, 0.9);
  border: 1px solid rgba(255, 212, 121, 0.4);
  font-size: 12px;
  color: #ffe1a1;
  max-width: 72%;
}

/* 新版本提示条 */
.update-bar {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 8px 16px;
  border-radius: 10px;
  background: rgba(30, 60, 110, 0.9);
  border: 1px solid rgba(121, 169, 255, 0.4);
  font-size: 12px;
  color: #cfe1ff;
}

.btn.small {
  padding: 5px 12px;
  font-size: 12px;
  white-space: nowrap;
}

/* 控制栏 */
.controls {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 16px;
  background: rgba(16, 16, 20, 0.72);
}

.ctl-btn {
  border: none;
  background: transparent;
  color: #fff;
  cursor: pointer;
  padding: 4px 6px;
  display: flex;
  align-items: center;
  transition: color 0.12s;
}

/* 单色 SVG 图标（与顶栏风格一致；emoji 已弃用——跨平台渲染不一致且语义混乱） */
.ctl-btn svg {
  width: 18px;
  height: 18px;
  fill: currentColor;
  display: block;
  pointer-events: none; /* 命中整个按钮，避免 SVG 吞事件 */
}

.ctl-btn:hover {
  color: #ffd479;
}

.time {
  font-size: 12px;
  color: #cfcfd8;
  font-variant-numeric: tabular-nums;
  min-width: 52px;
  text-align: center;
}

.progress {
  flex: 1;
  position: relative;
  height: 14px;
  display: flex;
  align-items: center;
  cursor: pointer;
}

.progress::before {
  content: "";
  position: absolute;
  left: 0;
  right: 0;
  height: 5px;
  border-radius: 3px;
  background: rgba(255, 255, 255, 0.2);
  transition: height 0.12s;
}

.progress:hover::before {
  height: 8px;
}

.progress-fill {
  position: absolute;
  left: 0;
  height: 5px;
  border-radius: 3px;
  background: #ffd479;
  transition: height 0.12s;
}

.progress:hover .progress-fill {
  height: 8px;
}

.progress-thumb {
  position: absolute;
  width: 12px;
  height: 12px;
  border-radius: 50%;
  background: #fff;
  transform: translateX(-50%);
  box-shadow: 0 1px 4px rgba(0, 0, 0, 0.5);
  opacity: 0;
  transition: opacity 0.12s;
}

.progress:hover .progress-thumb {
  opacity: 1;
}

.progress-tip {
  position: absolute;
  bottom: 18px;
  transform: translateX(-50%);
  padding: 3px 8px;
  border-radius: 4px;
  background: rgba(10, 10, 14, 0.92);
  color: #fff;
  font-size: 12px;
  font-variant-numeric: tabular-nums;
  pointer-events: none;
  white-space: nowrap;
}

.osd {
  position: absolute;
  left: 50%;
  top: 18px;
  transform: translateX(-50%);
  padding: 8px 16px;
  border-radius: 6px;
  background: rgba(10, 10, 14, 0.85);
  color: #fff;
  font-size: 13px;
  pointer-events: none;
  z-index: 10;
}

/* 倍速按钮 + 弹层 */
.speed-wrap {
  position: relative;
}

.speed-btn {
  min-width: 48px;
  padding: 4px 8px;
  border-radius: 6px;
  border: 1px solid rgba(255, 255, 255, 0.2);
  background: rgba(255, 255, 255, 0.1);
  color: #fff;
  font-size: 12px;
  cursor: pointer;
}

.speed-pop {
  position: absolute;
  bottom: 36px;
  left: 50%;
  transform: translateX(-50%);
  padding: 10px;
  border-radius: 10px;
  background: rgba(24, 24, 30, 0.97);
  border: 1px solid rgba(255, 255, 255, 0.14);
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
  z-index: 35;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.speed-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 4px;
}

.speed-chip {
  padding: 5px 0;
  border: none;
  border-radius: 6px;
  background: rgba(255, 255, 255, 0.08);
  color: #e8e8ee;
  font-size: 12px;
  cursor: pointer;
  white-space: nowrap;
}

.speed-chip:hover {
  background: rgba(255, 255, 255, 0.16);
}

.speed-chip.on {
  background: #ffd479;
  color: #1c1508;
  font-weight: 600;
}

/* 右键菜单 */
.ctx-mask {
  position: fixed;
  inset: 0;
  z-index: 40;
}

.ctx-menu {
  position: fixed;
  z-index: 41;
  min-width: 210px;
  padding: 6px;
  border-radius: 10px;
  background: rgba(24, 24, 30, 0.97);
  border: 1px solid rgba(255, 255, 255, 0.14);
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.5);
  display: flex;
  flex-direction: column;
}

.ctx-item {
  text-align: left;
  padding: 7px 12px;
  border: none;
  background: none;
  color: #e8e8ee;
  font-size: 13px;
  border-radius: 6px;
  cursor: pointer;
}

.ctx-item:hover {
  background: rgba(255, 255, 255, 0.1);
}

.ctx-item.on {
  color: #ffd479;
}

.ctx-divider {
  height: 1px;
  background: rgba(255, 255, 255, 0.1);
  margin: 6px 4px;
}

.ctx-label {
  padding: 2px 12px 4px;
  font-size: 11px;
  color: #8f8f9a;
}

.ctx-speeds {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 4px;
  padding: 0 6px 6px;
}

.ctx-speed {
  padding: 5px 0;
  border: none;
  border-radius: 6px;
  background: rgba(255, 255, 255, 0.08);
  color: #e8e8ee;
  font-size: 12px;
  cursor: pointer;
}

.ctx-speed:hover {
  background: rgba(255, 255, 255, 0.16);
}

.ctx-speed.on {
  background: #ffd479;
  color: #1c1508;
  font-weight: 600;
}

/* 播放中鼠标静止：顶栏上滑、控制栏下滑、隐藏光标 */
.topbar,
.controls {
  transition: transform 0.25s ease;
}

.ui-hidden .topbar {
  transform: translateY(-100%);
}

.ui-hidden .controls {
  transform: translateY(100%);
}

.ui-hidden {
  cursor: none;
}

/* 音量：品牌琥珀滑杆（accent-color 覆盖 macOS 默认浅蓝，深色 UI 上更协调） */
.vol {
  width: 84px;
  accent-color: #ffd479;
}
</style>
