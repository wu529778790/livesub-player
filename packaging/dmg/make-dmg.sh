#!/usr/bin/env bash
# 制作带引导背景的安装 DMG（拖入「应用程序」窗口）
#
# 依赖：brew install create-dmg
# 用法：make-dmg.sh <stage-dir> <out.dmg>
#   stage-dir 内放 LiveSub-Player.app（Applications 软链由 create-dmg 自动创建并摆放）
#
# 布局对应 packaging/dmg/dmg-bg.png（1320×840，即窗口 660×420 的 2x）：
#   标题 y≈92（顶部）｜图标槽位中心 y≈222（从顶部算）｜说明文字 y≈318｜左下角公证提示
#   create-dmg 的 Y 轴从窗口底部起算，数值经本机 DMG 截图校准（见 git blame）。
set -euo pipefail

STAGE="${1:?用法: make-dmg.sh <stage-dir> <out.dmg>}"
OUT="${2:?用法: make-dmg.sh <stage-dir> <out.dmg>}"
BG="$(cd "$(dirname "$0")" && pwd)/dmg-bg.png"

[[ -d "$STAGE" ]] || { echo "stage 目录不存在: $STAGE" >&2; exit 1; }
[[ -f "$BG" ]] || { echo "背景图不存在: $BG" >&2; exit 1; }

# 幂等：残留的同名输出会让 create-dmg 交互式询问是否覆盖
rm -f "$OUT"

create-dmg \
  --volname "LiveSub-Player" \
  --background "$BG" \
  --window-pos 240 150 \
  --window-size 660 420 \
  --icon-size 110 \
  --text-size 13 \
  --icon "LiveSub-Player.app" 170 208 \
  --app-drop-link 490 208 \
  "$OUT" "$STAGE"
