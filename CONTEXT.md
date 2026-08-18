# Context

## Glossary

| Term | Meaning | Notes |
|---|---|---|
| DocIR | 版面级中间表示：pages(page_no, regions, source) | 区别于 anydoc 的内容级 Document（无版面信息）；自有，不外借 |
| Region | 版面区域单元：kind + bbox + confidence + content | 演进自 src/region.rs 既有类型 |
| PageSource | 页数据来源：TextLayerPdf / TextLayerOfd / Ocr | DocIR 内溯源用 |
| FallbackSignal | 文字层→OCR 回退的判定信号（空层/浅检/深检/图片占比/字符数/置信度） | P1.6 引入；信号收集在通路，决策集中在 fallback::decide |
| Route | 回退决策结果：TextLayer / OcrPage / OcrDoc | decide() 输出；OcrPage 页级、OcrDoc 整文档 |
| golden 基线 | tests/golden/snapshots/*.sha256 共 22 个端到端快照 | 重构行为守护网；分级策略见 designs/anydoc-ocr-arch-refactor.md |
| 回退决策 | FallbackSignal→Route 的纯函数决策 | 不要写成"OCR 判定"，统一叫回退决策 |
| OCR tier | tiny/small/medium 三档模型规格 | 沿用既有术语，见 models.rs |
| 兜底通路 | DocKind::Other → anydoc::to_markdown | 唯一使用 anydoc 的位置 |
