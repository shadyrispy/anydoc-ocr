# T01 端到端 golden 基线（前置安全网）

**type**: infra / test
**blockers**: 无
**blocks**: T02 T03 T04 T06 T07 T08 T09 T10 T11 T12

## 目标
把现有真实样本（GJB1452A/GJB9001C/公报等 13/14 可用）+ 合成跨页表固化为**输出快照比对**测试，作为后续所有重构的回归护栏。

## 变更
- `tests/golden/`：快照 + runner
- 复用 `bench.sh` 样本集；固定 opts（tier/layout/threads/dpi）防漂移
- 大样本 gitignored（GJB9001C 37p）→ 仓库内用小子集 + 合成样本；CI 外跑

## 验收
- `cargo test --test golden` 绿
- 样本变更必须显式 `--update` 快照，diff 可读
- 任一重构后 golden 必须 bit-identical（除显式声明行为变化的 ticket）

## 风险/权衡
- 快照体积：中文输出大 → 存 hash + 人工抽检全文，或存全文到 git-lfs/外置
- 这是 T09/T10 等高回归风险的**唯一护栏**，必须最先落地
