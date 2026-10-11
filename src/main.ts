import { createApp } from "vue";
import App from "./App.vue";
import { attachConsole, error as logError, info as logInfo } from "@tauri-apps/plugin-log";

createApp(App).mount("#app");

// 前端日志进文件：console.* 转发到 Rust 日志（终端 + 文件），打包版也能查
void attachConsole();
logInfo("[web] 前端已加载");

// 全局异常捕获：未捕获错误 / Promise 拒绝写入日志文件
window.addEventListener("error", (e) => {
  void logError(`[web] 未捕获异常: ${e.message} @ ${e.filename}:${e.lineno}:${e.colno}`);
});
window.addEventListener("unhandledrejection", (e) => {
  void logError(`[web] 未处理的 Promise 拒绝: ${e.reason}`);
});
