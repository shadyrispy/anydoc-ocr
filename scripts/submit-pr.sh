#!/bin/bash
# 向上游 greatv/oar-ocr 提 PR：first_input_name 输入名自动探测
#
# 当前状态（2026-10-04）：分支已推好在 fork 上，只剩「开 PR」这一步。
#   fork   : shadyrispy/oar-ocr
#   分支   : fix/derive-onnx-input-name @ 510516c
#   基底   : upstream/main @ f755256（= 0.10.0）
#   补丁原文: scripts/pr-first-input-name.patch（+30/-3，2 文件）
#
# 为什么需要本机跑：沙箱内的 token 是 GitHub App integration，能往 fork 推分支，
# 但对第三方仓库的 pulls 端点返回 403 "Resource not accessible by integration"，
# 只能开 issue 不能开 PR。issue 会打扰上游，所以留这一步给你。
#
# 前提：本机 `gh auth login`（需有 fork 仓库的写权限 + 对上游开 PR 的权限）
#
# 用法：
#   bash scripts/submit-pr.sh          # 只开 PR（默认，分支已在 fork 上）
#   bash scripts/submit-pr.sh --force  # 重建并重推分支后再开 PR
set -euo pipefail

UPSTREAM_URL=https://github.com/greatv/oar-ocr.git
BRANCH=fix/derive-onnx-input-name
EXPECTED_COMMIT=510516c6e6a03d5af1145b5afbfb39135da93979
WORKDIR=$(mktemp -d)
HERE=$(cd "$(dirname "$0")" && pwd)
PATCH="$HERE/pr-first-input-name.patch"
BODY_FILE="$HERE/pr-first-input-name-body.md"
BODY=$(cat "$BODY_FILE")
TITLE='fix(inference): derive the ONNX input name from the session instead of hard-coding "x"'

command -v gh >/dev/null || { echo "需要 gh：brew install gh && gh auth login"; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "请先 gh auth login"; exit 1; }
FORK=$(gh api user -q .login)
echo "当前账号: $FORK"

if [ "${1:-}" = "--force" ]; then
  echo "==> 1/5 [force] 重建分支（基底 = upstream/main）"
  [ -f "$PATCH" ] || { echo "缺少 $PATCH"; exit 1; }
  git clone -q "https://github.com/$FORK/oar-ocr.git" "$WORKDIR/repo"
  cd "$WORKDIR/repo"
  git remote add upstream "$UPSTREAM_URL" 2>/dev/null || true
  git fetch -q upstream main
  git checkout -q -B "$BRANCH" upstream/main
  patch -p1 --dry-run < "$PATCH" >/dev/null || { echo "补丁对 upstream/main 不干净，中止"; exit 1; }
  patch -p1 < "$PATCH"
  git add -A
  git -c user.name="$(gh api user -q '.name // empty' 2>/dev/null || echo github)" \
      -c user.email="$(gh api user -q '.email // empty' 2>/dev/null || echo "${FORK}@users.noreply.github.com")" \
      commit -q -m "$TITLE"
  git push -q -f -u origin "$BRANCH"
  echo "    已重推: $(git rev-parse --short HEAD)"
else
  echo "==> 1/5 核对 fork 分支已在"
  SHA=$(gh api "repos/$FORK/oar-ocr/commits/$BRANCH" -q .sha 2>/dev/null || true)
  [ -n "$SHA" ] || { echo "fork 上没有 $BRANCH，用 --force 重建"; exit 1; }
  echo "    $BRANCH @ ${SHA:0:7}"
  [ "$SHA" = "$EXPECTED_COMMIT" ] || echo "    注意：与预期 $EXPECTED_COMMIT 不同，若非有意请用 --force"
fi

echo "==> 2/2 校验远端补丁内容"
# 确认远端分支上确实带上了 first_input_name，且已无硬编码的 Some("x")
R1=$(gh api "repos/$FORK/oar-ocr/contents/oar-ocr-core/src/core/inference/ort_infer_builders.rs?ref=$BRANCH" -q .content | base64 -d | grep -c "first_input_name" || true)
R2=$(gh api "repos/$FORK/oar-ocr/contents/oar-ocr-core/src/models/detection/db.rs?ref=$BRANCH" -q .content | base64 -d | grep -c 'Some("x")' || true)
echo "    first_input_name 出现 $R1 次（期望 4）；db.rs 残留 Some(\"x\") $R2 次（期望 0）"
[ "$R1" -ge 4 ] && [ "$R2" -eq 0 ] || { echo "远端内容不符，中止开 PR"; exit 1; }

echo "==> 3/3 开 PR（上游 greatv/oar-ocr）"
if gh pr view --repo "$UPSTREAM_URL" "$BRANCH" >/dev/null 2>&1; then
  echo "    PR 已存在：$(gh pr view --repo "$UPSTREAM_URL" "$BRANCH" --json url -q .url)"
  exit 0
fi
gh pr create \
  --repo "$UPSTREAM_URL" \
  --head "$FORK:$BRANCH" \
  --base main \
  --title "$TITLE" \
  --body "$BODY"

echo
echo "完成。补丁原文保留在 $PATCH"
echo "若要改 PR 正文，编辑 $BODY_FILE 后走："
echo "  gh pr edit --repo $UPSTREAM_URL $BRANCH --body-file $BODY_FILE"
