#!/usr/bin/env bash
# LiveSub-Player macOS 一键安装脚本
#
# 原理：curl/wget 命令行下载的文件不带 com.apple.quarantine 隔离标记，
# 安装后双击即开，不会被 Gatekeeper 拦截（无需右键打开 / xattr）。
#
# 用法：
#   一键安装（最新版，含 prerelease）：
#     curl -fsSL https://raw.githubusercontent.com/wu529778790/livesub-player/main/install.sh | bash
#   指定版本：
#     curl -fsSL https://raw.githubusercontent.com/wu529778790/livesub-player/main/install.sh | bash -s -- v1.0.0
#   或下载后执行：
#     ./install.sh [tag]

set -euo pipefail

REPO="wu529778790/livesub-player"
DEST="/Applications"
LEGACY_APP="DualSub.app"   # 项目改名前发布包内的旧 app 名，装新版时顺手清理

info() { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m警告:\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31m错误:\033[0m %s\n' "$*" >&2; exit 1; }

# ---------- 1. 架构检查（当前 Release 仅 Apple Silicon） ----------
case "$(uname -m)" in
  arm64) ;;
  x86_64) die "当前 Release 仅提供 Apple Silicon（arm64）构建，Intel Mac 暂不支持。" ;;
  *) die "不支持的架构：$(uname -m)" ;;
esac

# ---------- 2. 解析下载地址 ----------
# 注意：GitHub 的 releases/latest 接口不含 prerelease，因此用列表接口
# 取「第一个带 macOS zip 资产的 Release」（列表按时间倒序，即最新）。
TAG="${1:-}"
API="https://api.github.com/repos/$REPO/releases?per_page=20"
if [[ -n "$TAG" ]]; then
  API="https://api.github.com/repos/$REPO/releases/tags/$TAG"
fi

info "查询最新 Release …"
JSON="$(curl -fsSL "$API")" || die "无法访问 GitHub API（网络受限可先 export HTTPS_PROXY）"

URL="$(printf '%s' "$JSON" \
  | grep -oE '"browser_download_url": *"[^"]+"' \
  | sed -E 's/.*"([^"]+)"$/\1/' \
  | grep -iE 'macos.*\.zip$' \
  | head -1 || true)"
[[ -n "$URL" ]] || die "未在 Release 中找到 macOS 安装包"

# URL 形如 …/releases/download/<TAG>/<file>，从地址里取真实 TAG（比 basename 整段可靠）
TAG_SHOW="$(basename "${URL%/*}")"
info "目标版本：$TAG_SHOW"
info "下载地址：$URL"

# ---------- 3. 下载并解压 ----------
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

info "下载中 …"
curl -fL --progress-bar -o "$TMP/app.zip" "$URL" || die "下载失败，请检查网络后重试"

# ---------- 3.5 SHA256 校验（Release 附带 SHA256SUMS.txt，防损坏/篡改） ----------
SUMS_URL="${URL%/*}/SHA256SUMS.txt"
if curl -fsSL -o "$TMP/SHA256SUMS.txt" "$SUMS_URL" 2>/dev/null; then
  info "校验 SHA256 …"
  WANT="$(grep " $(basename "$URL")\$" "$TMP/SHA256SUMS.txt" | awk '{print $1}' | head -1)"
  GOT="$(shasum -a 256 "$TMP/app.zip" | awk '{print $1}')"
  [[ -n "$WANT" ]] || die "校验文件里找不到 $(basename "$URL") 的 SHA256"
  [[ "$WANT" == "$GOT" ]] || die "SHA256 校验失败（下载不完整或被篡改），请重试"
  info "SHA256 校验通过"
else
  warn "该 Release 未提供 SHA256SUMS.txt，跳过校验"
fi

EXTRACT="$TMP/extract"
mkdir -p "$EXTRACT"
ditto -x -k "$TMP/app.zip" "$EXTRACT" || die "解压失败，安装包可能已损坏"

APP="$(find "$EXTRACT" -maxdepth 1 -name '*.app' -print -quit)"
[[ -n "$APP" ]] || die "压缩包中未找到 .app"
APP_NAME="$(basename "$APP")"

# ---------- 4. 安装到 /Applications ----------
info "安装 $APP_NAME 到 $DEST …"
if [[ -d "$DEST/$APP_NAME" ]]; then
  rm -rf "$DEST/$APP_NAME" 2>/dev/null \
    || die "无法删除旧版本（权限不足）。请重新运行：sudo bash $0 ${TAG:-}"
fi
if ! ditto "$APP" "$DEST/$APP_NAME"; then
  die "复制到 $DEST 失败（权限不足）。请重新运行：sudo bash $0 ${TAG:-}"
fi

# 项目改名：清理旧 DualSub.app（非致命）
if [[ "$APP_NAME" != "$LEGACY_APP" && -d "$DEST/$LEGACY_APP" ]]; then
  if rm -rf "$DEST/$LEGACY_APP" 2>/dev/null; then
    info "已移除旧版 $LEGACY_APP"
  else
    warn "旧版 $LEGACY_APP 删除失败（权限不足），可手动移除"
  fi
fi

# 双保险：命令行下载本就不带 quarantine，这里再清一次确保万无一失
xattr -dr com.apple.quarantine "$DEST/$APP_NAME" 2>/dev/null || true

if codesign --verify "$DEST/$APP_NAME" 2>/dev/null; then
  info "签名校验通过"
fi

info "安装完成：$DEST/$APP_NAME"
info "双击打开即可使用——命令行下载不带隔离标记，无 Gatekeeper 拦截。"
