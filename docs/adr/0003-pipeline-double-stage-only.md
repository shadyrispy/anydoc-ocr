# ADR-0003: P3 先做 render↔OCR 双段，GFM 流式延后

- 状态: Accepted
- 日期: 2026-08-14
- 决策者: 架构 review

## 背景

P3 流水线尾段有两个范围：
- **双段**：只流水渲染与 OCR，GFM 仍批量收尾（`structure_results_to_gfm` 不动）。
- **三段**：render↔OCR↔GFM 全流式，GFM per-page 推送 + 跨页表延迟 flush。

## 决策

**先做双段。GFM 流式（P5）延后，待 P7 per-page 计时实测 GFM 尾段占比再定。**

## 理由

1. 双段先拿主吞吐收益（渲染延迟掩盖），复杂度可控。
2. 三段需改 `gfm_adapter` 签名（batch→stream）+ `DocumentEmitter` 线程安全，风险高。
3. GFM 尾段占比未知——P7 per-page 直方图给出数据后增量决策，避免盲目扩大 P3 范围。
4. 双段落地后 GFM 仍是 OCR 后批量串行，若实测占比低则 P5 可不做。

## 后果

- `structure_results_to_gfm` 签名与行为不变。
- `DocumentEmitter` 保持非线程安全（单线程消费 OCR 结果）。
- P5 作为后续增量项，依赖 P7 数据。

## 关联

- ADR-0002（流水线）：双段是该 ADR 的范围限定。
