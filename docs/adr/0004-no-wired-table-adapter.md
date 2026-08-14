# ADR-0004: 不加 wired 表 adapter

- 状态: Accepted
- 日期: 2026-08-14
- 决策者: 架构 review

## 背景

oar-ocr `OARStructureBuilder` 支持 `with_table_structure_recognition(model, "wired")`。当前 `build_analyzer` 只配 `wireless`，wired 表靠通用适配器兜底。MinerU 区分 slanet(wireless)/unet(wired)。

## 决策

**不加 wired adapter。**

## 理由

1. 公文场景带框表格罕见，多为无线表，SLANet wireless 已覆盖主流。
2. wired adapter + unet 模型体积 +1，ROI 低。
3. 现有通用适配器对 wired 表分类已兜底（避免 config_error 整页失败）。

## 后果

- `build_analyzer` 保持单 `wireless` adapter。
- 带框表格结构识别精度不提升（已知限制）。
- 若未来公文样本统计显示带框表占比显著，重新评估。

## 关联

- 无。
