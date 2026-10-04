#!/bin/bash
# 向上游 greatv/oar-ocr 提 PR：first_input_name 输入名自动探测
#
# 前提：本机需 `gh auth login` 或配置好 git 凭证（沙箱内的 token 是 GitHub App
# integration，无 fork / 跨仓写权限，403 "Resource not accessible by integration"，
# 故 fork 与 push 必须在本地完成）。
#
# 用法：
#   bash scripts/submit-pr.sh
set -euo pipefail

UPSTREAM_URL=https://github.com/greatv/oar-ocr.git
BRANCH=fix/derive-onnx-input-name
WORKDIR=$(mktemp -d)
HERE=$(cd "$(dirname "$0")" && pwd)
PATCH="$HERE/pr-first-input-name.patch"
BODY=$(cat "$HERE/pr-first-input-name-body.md")
TITLE='fix(inference): derive the ONNX input name from the session instead of hard-coding "x"'

command -v gh >/dev/null || { echo "需要 gh：brew install gh && gh auth login"; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "请先 gh auth login"; exit 1; }
[ -f "$PATCH" ] || { echo "缺少 $PATCH"; exit 1; }

echo "==> 1/5 fork 上游到当前账号"
# 已 fork 则 gh 会报错，忽略
gh repo fork "$UPSTREAM_URL" --clone=false --remote=false 2>/dev/null || true
FORK=$(gh api user -q .login)
echo "    fork = $FORK/oar-ocr"
gh repo view "$FORK/oar-ocr" >/dev/null 2>&1 || {
  echo "    fork 不存在。请在浏览器打开 $UPSTREAM_URL 点 Fork，或给 token 加 fork 权限后重跑"; exit 1; }

echo "==> 2/5 克隆 fork 到 $WORKDIR"
git clone -q "https://github.com/$FORK/oar-ocr.git" "$WORKDIR/repo"
cd "$WORKDIR/repo"
git remote add upstream "$UPSTREAM_URL" 2>/dev/null || true
git fetch -q upstream main
git checkout -q -B "$BRANCH" upstream/main
echo "    基底 = upstream/main @ $(git rev-parse --short HEAD)"

echo "==> 3/5 应用补丁"
patch -p1 --dry-run < "$PATCH" >/dev/null   # 先干跑，失败即中止
patch -p1 < "$PATCH"
git add -A
git -c user.name="$(gh api user -q .name || echo github)" \
    -c user.email="$(gh api user -q .email || echo $(gh api user -q .id)@users.noreply.github.com)" \
    commit -q -m "$TITLE" -m "Co-Authored-By: none"
echo "    commit = $(git rev-parse --short HEAD)"

echo "==> 4/5 推送分支"
git push -q -u origin "$BRANCH"
echo "    https://github.com/$FORK/oar-ocr/tree/$BRANCH"

echo "==> 5/5 开 PR"
gh pr create \
  --repo "$UPSTREAM_URL" \
  --head "$FORK:$BRANCH" \
  --base main \
  --title "$TITLE" \
  --body "$BODY" \
  --repo "$FORK/oar-ocr" \
  || gh pr create --repo "$UPSTREAM_URL" --head "$FORK:$BRANCH" --base main \
       --title "$TITLE" --body "$BODY"

echo
echo "完成。补丁原文保留在 $PATCH"
echo "（若要改 PR 正文，编辑 $HERE/pr-first-input-name-body.md 后重跑 body 段）"
