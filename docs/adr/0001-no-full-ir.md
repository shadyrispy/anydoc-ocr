# ADR-0001: 不引入全套 IR 中间层

- 状态: Accepted
- 日期: 2026-08-14
- 决策者: 架构 review

## 背景

参考项目 anydoc 与 MinerU 均有 IR 中间层：
- MinerU `middle_json`：10 段流水线 stage 间解耦，支持 stage 重跑、多格式序列化、断点续算。
- anydoc `Document` 模型：10+ 格式解析器共用一套 block/inline/table 类型，统一序列化。

评估是否在 anydoc-ocr 引入类似 IR。

## 决策

**不做全套 IR。**

## 理由

1. 单输出格式（Markdown only）—— IR 的多格式序列化收益为 0。
2. 单趟流水（extract→emit，无 stage 重跑）—— IR 的断点续算收益为 0。
3. 2 路径非 10 段 DAG（文字层 / OCR）—— IR 的 stage 复用收益为 0。
4. 散装 IR 已存在并覆盖共享需求：
   - `Region`（文本块，PDF/OFD 文字层共用）
   - `TableGrid`（表格，文字层网格 + OCR Image 重建共用）
   - `StructureResult`（OCR 结果，oar-ocr 借来）
   - `DocumentEmitter`（装配，跨页表挂起/续接，三通路共用）
5. 全套 IR = 大重构 + 22 个 golden 全 re-baseline + 行为漂移风险，ROI 低。
6. IR 不解锁吞吐：render↔OCR 流水线（ADR-0002）在现有 `StructureResult`/`Region` 上即可跑。

## 后果

- 不引入 `PageBlock` 统一枚举或完整 Document 模型。
- 共享逻辑仍由 `reading_order` / `table_grid` / `text_health` / `emitter` 承担。
- 若未来加 JSON 输出或需要 per-stage golden，再重新评估窄 IR（仅测试基建，非运行时）。

## 关联

- ADR-0002（流水线）：在现有类型上跑，无需 IR。
