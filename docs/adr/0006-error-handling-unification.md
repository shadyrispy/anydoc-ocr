# ADR-0006: 错误处理统一——复用 anydoc::ConvertError

- 状态: Accepted（2026-08-15 审计跟进，详见末节）
- 日期: 2026-08-15
- 决策者: 架构 review

## 背景

ADR-0005 候选 2 落地后，brooks-review 发现一处 Critical 静默失败：损坏 PDF 在
`render_cross_doc_fn` 内被 `eprintln + continue` 跳过，batch.rs 兜底标为
`Ok(String::new())`，导致 main.rs 写出空 .md 并计入成功。已修复为 `Err`，但暴露
出更深层问题：**错误类型信息在边界处丢失**。

当前错误处理的三个问题：

1. **PDF 错误被吞**（[text_layer.rs:41-44](../../src/pdf/text_layer.rs#L41-L44)）：
   ```rust
   let items = match pdf_inspector::extract_text_with_positions(path) {
       Ok(items) => items,
       Err(_) => return Ok(None),  // ← Encrypted/NotAPdf 全被当"图片型需OCR"
   };
   ```
   加密 PDF → `Ok(None)` → batch 当图片型送 OCR → pdfium 加载失败 → 兜底标 Err。
   绕一大圈，"加密"分类丢失。

2. **anydoc 错误被降级为字符串**（[convert.rs:26](../../src/convert.rs#L26)）：
   ```rust
   DocKind::Other => anydoc::to_markdown(path).map_err(|e| anyhow::anyhow!("{e}")),
   ```
   anydoc 的 `ConvertError`（含 `code()` 稳定字符串）被 `anyhow!("{e}")` 降级，
   `downcast_ref::<ConvertError>()` 失败，无法按 `encrypted`/`malformed` 分支。

3. **OFD 错误无类型**（[ofd/mod.rs](../../src/ofd/mod.rs)）：
   全用 `anyhow::anyhow!("打开 OFD 失败: {e}")` 字符串，无分类。

上游能力调研：
- **anydoc 0.1.9** [error.rs](file:///root/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/anydoc-0.1.9/src/error.rs)
  已有完整 `ConvertError` 枚举：`Encrypted`/`Malformed { part, detail }`/
  `ResourceLimit { limit, detail }`/`MissingPart { part }`/`Unsupported(String)`/`Io`，
  且 `formats/pdf.rs:40-48` 已实现 `PdfError → ConvertError` 完整映射。
- **pdf-inspector 1.14.2** [lib.rs:6077-6088](file:///root/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/pdf-inspector-1.14.2/src/lib.rs#L6077-L6088)
  暴露 `pub enum PdfError { Io, Parse, Encrypted, InvalidStructure, NotAPdf }`。
- **ofd-core 0.3.0** 经核验**不支持**加密识别：`OfdError` 8 变体无加密相关，
  `Cargo.toml` 的 `zip` 依赖关闭了 `aes-crypto` 特性，加密 OFD 会以 `Zip`/
  `EntryNotFound`/`Structure`/`Xml` 错误失败，无加密语义。

## 决策

**复用 `anydoc::ConvertError` 作为全库统一错误类型，三类文档错误统一分类。**

### 1. 错误模型：复用 anydoc::ConvertError，不 fork 不包装

```rust
// 全库统一类型别名
pub type Result<T> = std::result::Result<T, anydoc::ConvertError>;
```

理由：
- anydoc 的 `ConvertError` 已覆盖文档错误的全部语义变体（Encrypted/Malformed/
  MissingPart/ResourceLimit/Unsupported/Io），无需自建
- `code()` 方法暴露稳定字符串，main.rs 可按 code 精准提示
- 不 fork：保持与上游 anydoc 同步升级能力，未来 anydoc 加变体不破坏我们
- 不包装：避免 `anydoc_ocr::ConvertError::Doc(anydoc::ConvertError)` 这类冗余嵌套，
  调用方直接 match `anydoc::ConvertError` 变体

### 2. 三类文档错误映射

| 文档类型 | 错误源 | 映射到 ConvertError |
|---------|--------|---------------------|
| PDF | `pdf_inspector::PdfError::Encrypted` | `Encrypted` |
| PDF | `PdfError::NotAPdf(d)` | `Malformed { part: None, detail: "not a PDF: {d}" }` |
| PDF | `PdfError::InvalidStructure` | `Malformed { part: None, detail: "invalid PDF structure" }` |
| PDF | `PdfError::Parse(d)` | `Malformed { part: None, detail }` |
| PDF | `PdfError::Io(e)` | `Io(e)` |
| OFD | `OfdError::Io` | `Io` |
| OFD | `OfdError::Zip` | `Malformed { part: None, detail: "zip 容器损坏" }` |
| OFD | `OfdError::EntryNotFound(part)` | `MissingPart { part }` |
| OFD | `OfdError::Structure(d)` | `Malformed { part: None, detail }` |
| OFD | `OfdError::Xml(d)` | `Malformed { part: None, detail }` |
| OFD | 其他 | `Malformed { part: None, detail }` |
| anydoc 格式 | `anydoc::ConvertError` 直接透传 | 原样 |

**OFD 加密识别**：ofd-core 0.3.0 不支持（已核验），OFD 加密文档会以 `Structure`/
`Xml` 错误失败，统一归 `Malformed`。不额外做加密探测——ofd-core 未暴露加密层，
自行解析 OFD.xml 的 `<EncryptInfo>` 元素成本高且 ofd-core 不解析内容文件加密，
收益不抵成本。未来 ofd-core 升级支持加密再补 `Encrypted` 映射。

### 3. 运行时错误：统一归 Malformed + detail

ORT 加载失败、pdfium 绑定失败、单页渲染失败等"非文档本身问题"的运行时错误，
统一映射到 `ConvertError::Malformed { part, detail }`：
- `part`：失败位置（如 `Some("page 3")`、`Some("ort engine")`、`None`）
- `detail`：原始错误字符串（如 `"ORT 引擎加载失败: ..."`, `"渲染页 3 失败: ..."`）

理由：
- `ConvertError` 是 anydoc 的 external type，不能加变体（不 fork 决策）
- `Unsupported` 语义为"格式不支持"，运行时错误非格式问题，`Malformed` 更接近
- 调用方看 `detail` 字符串能区分具体原因，`code()` 返 `malformed` 也能处理
- 不完美但够用：运行时错误本就罕见（ORT/pdfium 部署问题），过度优化收益低

### 4. 签名变更：全库 `Result<T> = Result<T, ConvertError>`

- [convert.rs](../../src/convert.rs) `convert_to_markdown` 签名改 `Result<String, ConvertError>`
- [pdf/mod.rs](../../src/pdf/mod.rs) `convert_pdf`/`convert_pdf_ocr` 改 `Result<.., ConvertError>`
- [ofd/mod.rs](../../src/ofd/mod.rs) `convert_ofd` 改 `Result<.., ConvertError>`
- [pipeline.rs](../../src/pipeline.rs) `PagePipeline::run` 改 `Result<.., ConvertError>`
- [pdf/render.rs](../../src/pdf/render.rs) render 闭包返 `Result<(), ConvertError>`
- [pdf/text_layer.rs](../../src/pdf/text_layer.rs) `text_layer_markdown` 改 `Result<Option<String>, ConvertError>`
- [batch.rs](../../src/batch.rs) `BatchConverter::convert_many` 返 `Vec<Result<String, ConvertError>>`
- [lib.rs](../../src/lib.rs) `Result<T>` 类型别名改 `Result<T, ConvertError>`

**anyhow 去留统一决策：全库移除 anyhow**。

理由：
- 签名统一为 `Result<T, ConvertError>` 后，`anyhow::Result<T>` 别名必须移除
  （两者类型不兼容，保留别名会导致 `?` 传播点编译失败）
- pipeline 内部错误链构造改用 `ConvertError::Malformed { part, detail }` 直接构造，
  detail 字符串承载原始错误信息（如 `format!("ORT 引擎加载失败: {e}")`），
  无需 anyhow 中转
- anyhow 的 `Context` trait 在边界处改用 `.map_err(|e| crate::error::runtime(None, format!("{e}")))`
  替代，语义等价
- 移除 anyhow 后 `Cargo.toml` 删掉 anyhow 依赖，减少一个外部 crate

**具体移除点**：
- `src/lib.rs`：删 `pub type Result<T> = anyhow::Result<T>;`，改 `pub type Result<T> = std::result::Result<T, anydoc::ConvertError>;`
- 各模块 `use anyhow::anyhow;` / `use anyhow::Context;` 删除，改用 `ConvertError` 构造
- pipeline 内部 `anyhow::Error` 改 `ConvertError`，`anyhow::bail!` 改 `return Err(crate::error::runtime(part, detail))`
- `Cargo.toml`：删 `anyhow = "..."` 依赖行

### 5. PDF 预分流：Err 不送 OCR

[batch.rs](../../src/batch.rs) 预分流阶段，`text_layer_markdown` 返 `Err` 的 PDF
（Encrypted/Malformed）直接标 `Err` 填槽，**不送 `ocr_paths`**：
- 加密 PDF 送 OCR 也读不了（pdfium 同样无法解密）
- 损坏 PDF 送 OCR 浪费资源
- 错误信息从源头保留分类（Encrypted 不变 Malformed）

### 6. force_ocr 不绕过加密检查

`pdf_force_ocr` 跳过 text_layer 文字提取，但**不跳过加密检测**：
- force_ocr 时仍调 `text_layer_markdown` 做加密预检（忽略返回的 `Ok(Some(md))` 文字层，
  只看 `Err`）
- 加密 PDF 即使 force_ocr 也报 `Encrypted`，不送 OCR
- 未加密 + force_ocr → 送 OCR pipeline

实现：batch.rs 预分流阶段，force_ocr 时调 `text_layer_markdown`：
```rust
if self.opts.pdf_force_ocr {
    match crate::pdf::text_layer_markdown(path, &self.opts) {
        Ok(_) => ocr_paths.push((i, path.clone())),  // 忽略文字层，送 OCR
        Err(e) => slots[i] = Some(Err(e)),           // 加密/损坏直接标错
    }
    continue;
}
```

### 7. main.rs 按 code() 精准提示

main.rs 对 `Err` 调 `code()`（ConvertError 直接有此方法，无需 downcast），
按 code 给用户精准提示：

```rust
match result {
    Ok(md) => { /* 写文件 */ }
    Err(e) => {
        let hint = match e.code() {
            "encrypted" => "文档已加密，需提供密码或解密后重试",
            "malformed" => "文档损坏或格式错误",
            "missingPart" => "文档结构不完整（缺必需部件）",
            "resourceLimit" => "超出安全限制（可能解压炸弹或过大）",
            "unsupported" => "格式不支持或需 OCR 但未配置",
            "io" => "文件读写错误",
            _ => "未知错误",
        };
        eprintln!("{prefix} {} 失败: {e}\n  提示: {hint}", path.display());
        fail += 1;
    }
}
```

## 理由

### 为什么复用而非自建

- anydoc 的 `ConvertError` 已是成熟设计：6 变体覆盖文档错误全语义，`code()`
  稳定字符串已用于 Node/wasm 绑定，经过上游测试验证
- 自建 `anydoc_ocr::ConvertError` 要写映射表、维护与上游同步、调用方多一层
  嵌套——收益不抵成本
- anydoc 是我们已依赖的上游（docx/xlsx/pptx 等格式靠它解析），复用其错误类型
  是自然延伸，非额外耦合

### 为什么不 fork anydoc 加变体

- fork 放弃 cargo 依赖更新，未来 anydoc 升级要手动合并
- 运行时错误（ORT/pdfium）归 `Malformed + detail` 已够用，不值得为它 fork
- 保持与上游同步能力是长期维护优势

### 为什么 OFD 加密不额外探测

- ofd-core 0.3.0 已核验不支持加密（`OfdError` 无加密变体，zip 依赖关闭 aes-crypto）
- 自行解析 `<EncryptInfo>` 需深入 ofd-core 内部层，ofd-core 未暴露该层 API
- OFD 加密文档罕见（GB/T 33190 加密 OFD 在实际交换中极少见）
- 归 `Malformed` 能正确报错（不静默），只是分类不精准——可接受

## 后果

### 实施节奏：结合 ADR-0005 已落代码分 3 步递进

ADR-0005 候选 1+2 已落地（batch.rs / pipeline.rs / pdf/render.rs / pdf/mod.rs /
ofd/mod.rs / main.rs / lib.rs / text_layer.rs）。本 ADR 在这些已落代码上做错误
处理升级：`anyhow::Error` → `ConvertError` 类型化。每步锚定 0005 的代码点，
逐步替换，每步可独立编译 + golden 验证。

**第 1 步：错误基础设施 + PDF 文字层源头分类**
（锚定 0005 候选 2 的 `text_layer.rs` 预分流入口）

- [error.rs](../../src/error.rs)：新建 `from_pdf_error` / `from_ofd_error` / `runtime`
  映射助手；`Result<T>` 别名改 `Result<T, ConvertError>`
- [lib.rs](../../src/lib.rs)：`pub use error::{ConvertError, Result}`，删旧
  `pub type Result<T> = anyhow::Result<T>`
- [text_layer.rs](../../src/pdf/text_layer.rs)：`extract_text_with_positions` 错误
  按 `PdfError` 分类返 `Err(ConvertError)`，不再吞 `Ok(None)`（ADR-0006 §1 问题点）
- **验证**：编译通过；健康 PDF golden 零变更（错误路径不影响）；`text_layer` 单元
  测试（若有 mock PdfError）补 Encrypted/Malformed 分支断言

**第 2 步：PDF 跨文档 pipeline + render 错误类型化**
（锚定 0005 候选 2 的 `pipeline.rs` 复合键 + `pdf/render.rs` 跨文档闭包 + `pdf/mod.rs`）

- [pipeline.rs](../../src/pipeline.rs)：`RenderItem` 的 `anyhow::Error` 改
  `ConvertError`；`run()` 返回 `Result<Vec<..>, ConvertError>`；内部
  `anyhow::anyhow!` 构造改 `crate::error::runtime(part, detail)`
- [pdf/render.rs](../../src/pdf/render.rs)：`render_cross_doc_fn` 返回类型改
  `Result<(), ConvertError>`；`anyhow::bail!` 改 `return Err(runtime(...))`；
  `locate_pdfium` / `render_pdf_pages` / `render_document` 同步去 anyhow
- [pdf/mod.rs](../../src/pdf/mod.rs)：`convert_pdf` / `convert_pdf_ocr` 签名靠
  lib.rs 别名自动生效；内部 `?` 传播点检查（PdfError 已有 `from_pdf_error`，
  渲染错误走 `runtime`）
- **验证**：PDF golden（OCR + 非 OCR）零变更；batch golden 零变更

**第 3 步：OFD + anydoc 格式 + batch 预分流 + main 提示 + 测试**
（锚定 0005 候选 1+2 的 `ofd/mod.rs` + `convert.rs` + `batch.rs` + `main.rs`）

- [ofd/mod.rs](../../src/ofd/mod.rs)：`OfdError → ConvertError` 映射（调
  `crate::error::from_ofd_error`），移除全部 `anyhow::anyhow!("...")`；
  render 闭包返回类型改 `Result<(), ConvertError>`
- [convert.rs](../../src/convert.rs)：`convert_to_markdown` 签名靠别名生效；
  anydoc 格式透传 `ConvertError`（删 `map_err(|e| anyhow!("{e}"))`，直接 `?`）
- [batch.rs](../../src/batch.rs)：预分流阶段 `text_layer_markdown` 返 `Err` 直接
  标错不送 `ocr_paths`（ADR-0006 §5）；force_ocr 时仍调 text_layer 做加密预检
  （ADR-0006 §6）；兜底 `Err` 从 `anyhow::anyhow!` 改 `crate::error::runtime`
- [main.rs](../../src/main.rs)：`run_batch` 的 `anyhow::Result` 改
  `Result<(), ConvertError>`；`Err` 分支按 `e.code()` 精准提示（ADR-0006 §7）；
  `write_single` / `resolve_stdin` 同步
- `Cargo.toml`：删 `anyhow` 依赖（全库无残留 `use anyhow` 后）
- 补 4 测试样本（覆盖三类）：
  - 加密 PDF（qpdf 加密生成）→ 断言 `ConvertError::Encrypted` + `code()=="encrypted"`
  - 损坏 PDF（已有 `batch_isolates_corrupt_pdf_as_err`，改断言 `Malformed`）
  - 加密 docx（OOXML EncryptionInfo）→ 断言 `Encrypted`
  - 损坏 docx（zip 截断）→ 断言 `Malformed`
- **验证**：全 golden + 4 新测试全绿；`cargo build` 无 anyhow 残留

### 风险

- **签名变更连带**：`Result<T>` 别名改 `ConvertError` 后，所有 `?` 传播点需检查
  `From` impl 是否覆盖。anydoc 已 impl `From<io::Error>`；`PdfError`/`OfdError`
  无 `From` impl，调用方需显式 `.map_err(crate::error::from_pdf_error)` /
  `from_ofd_error`（已在 error.rs 提供助手函数）。运行时错误用
  `crate::error::runtime(part, detail)` 构造（anydoc 的 `ConvertError::malformed`
  是 `pub(crate)` 我们用不了，直接构造 `Malformed { part, detail }`）
- **golden 回归**：健康文档路径不变（错误分类只影响失败路径），golden 应零变更。
  但 `convert_to_markdown` 签名变更可能影响测试代码（`tests/golden.rs` 等），
  需同步改测试的 `?` 传播
- **anyhow 全库移除的连锁影响**：`anyhow::Result<T>` 别名移除后，所有 `?` 传播点
  需 `From` impl 支持。anydoc 已 impl `From<io::Error> for ConvertError`，但
  `From<anyhow::Error>` 无 impl——移除 anyhow 后此问题消失（不再有 anyhow::Error
  需转换）。编译器会强制检查所有 `?` 点，类型不匹配即编译失败，非运行时风险
- **OFD 加密归 Malformed**：用户遇到加密 OFD 会看到"malformed"而非"encrypted"提示。
  可接受——ofd-core 不支持，且 OFD 加密罕见

### 不做项

- 不 fork anydoc（保持上游同步）
- 不自建 `anydoc_ocr::ConvertError` 包装枚举（避免冗余嵌套）
- 不为 OFD 加密额外探测（ofd-core 不支持，收益不抵成本）
- 不改 anydoc 内部错误处理（上游职责，我们只消费）
- 不做错误重试/恢复策略（本 ADR 只管分类，恢复策略另议）

## 审计跟进 (2026-08-15)

ADR-0006 三步实施完成后，`brooks-review` 复审发现 4 处执行缺口（Health 92/100，
无 Critical）。本节记录每个发现的 grilling（备选权衡 → 决策）与修复方案，
并细化执行计划。**性质：对 0006 的补全/纠正，非新架构决策**——故修订本 ADR
而非另开 0007。

### 发现清单

| 编号 | 严重度 | 风险类别 | 摘要 |
|------|--------|----------|------|
| W1 | 🟡 Warning | Change Propagation | `error_hint` 仅 batch 路径生效；stdin/单文件主路径 `?` 直传，无"下一步建议" |
| S1 | 🟢 Suggestion | Knowledge Duplication | `from_pdf_error` 镜像 anydoc 私有 `map_error`，版本升级有语义漂移风险 |
| S2 | 🟢 Suggestion | Accidental Complexity | main.rs 本地 `mod error` 重复 `anydoc_ocr::Result`（lib 已 pub 导出） |
| S3 | 🟢 Suggestion | Domain Model Distortion | `runtime()` 错误归 `Malformed`，`error_hint` 对 `malformed` 提示"文档损坏"误导环境/运行时错误 |

### 修复方案（grilling）

#### W1：error_hint 覆盖全部 CLI 入口

**Symptom**：[main.rs:71-76, 81-84](../../src/main.rs#L71-L84) 单文件/stdin 路径用
`convert_to_markdown(...)?` 直传，`main()->Result<()>` 经 `Termination` 仅打印
`ConvertError` 的 Display，不走 `error_hint`；仅 [main.rs:124-130](../../src/main.rs#L124-L130)
batch 路径有提示。CLI 最常用模式 `anydoc-ocr encrypted.pdf` 拿不到 ADR-0006 §7
承诺的精准提示。

**备选权衡**：
- (a) 抽 `fn print_error(e: &ConvertError)` 供三路共用 → 最小重复，但 `?` 优雅丢失
- (b) 每路 `?` 改 `match` + 内联 hint → 重复 3 次 hint 调用
- (c) 把 hint 推到 lib 层（`ConvertError::hint()`）→ 越界，lib 不应承载 CLI 文案
- (d) 加差异化退出码（encrypted→2, malformed→3...）→ ADR-0006 §7 只要求"精准提示"，
  退出码是脚本集成诉求，**YAGNI**，留待未来 ADR

**决策**：采 (a)。抽 `fn print_error(e: &ConvertError) -> !`（打印 `失败: {e}\n  提示: {hint}`
+ `std::process::exit(1)`），stdin/单文件/batch 三路共用。**不加差异化退出码**
（ADR-0006 未要求；脚本可解析 stderr 的 `code()` 行，未来需要再开 ADR）。

**改动点**（main.rs）：
- 新增 `fn print_error(e: &ConvertError) -> !`
- stdin 路径：`let md = convert_to_markdown(&path, &opts).map_err(|e| { print_error(&e); })?;`
  → 实为 `match convert_to_markdown(&path, &opts) { Ok(md)=>md, Err(e)=>print_error(&e) }`
- 单文件路径：同上
- batch 路径：内层 `Err(e)` 分支调 `error_hint` 改调 `print_error`（但 batch 不退出，
  继续处理下一文件——故 batch 路径保留现有 `eprintln! + fail+=1`，**不**用 `print_error`）
- 复核：`print_error` 仅用于"单文档即终止"语义；batch 是"错误隔离继续"，语义不同，
  不强求统一函数。最终：stdin/单文件用 `print_error`，batch 保留 `error_hint` 调用

**修正后的决策**：`error_hint` 仍被 batch 用；新增 `print_error` 给单文档路径。
两者共用 `error_hint` 内核。不抽 `print_error`，而是单文档路径改为：
```rust
let md = match convert_to_markdown(&input, &opts) {
    Ok(md) => md,
    Err(e) => {
        eprintln!("转换失败: {e}\n  提示: {}", error_hint(&e));
        std::process::exit(1);
    }
};
```
保持 `error_hint` 为唯一提示源，batch 与单文档都调它，只是单文档额外 `exit(1)`。

#### S1：锁定 from_pdf_error 语义契约

**Symptom**：[error.rs:14-32](../../src/error.rs#L14-L32) 与 anydoc
`formats/pdf.rs:40-48` 私有 `map_error` 逐变体同语义重复。`anydoc="=0.1.9"` 精确锁
版 + 穷尽 match（新增 `PdfError` 变体编译即断）已防护"结构漂移"，但**语义漂移**
（anydoc 把 `Parse` 改映射目标）在版本升级时静默发生。

**备选权衡**：
- (a) 单元测试断言每个 `PdfError` 变体 → 期望 `ConvertError` 变体 + `code()`
- (b) 向 anydoc 上游 PR 把 `map_error` 设 `pub`，本库直接转发 → 依赖上游接受/合入时间，
  不可控，不作为本次修复路径
- (c) 不处理，靠 `=0.1.9` 锁版 → 升版时无防护

**决策**：采 (a)。在 [error.rs](../../src/error.rs) 加 `#[cfg(test)] mod tests`，
逐变体构造 `PdfError`（`Encrypted`/`InvalidStructure`/`NotAPdf(s)`/`Parse(s)`/
`Io(io::Error)`）调 `from_pdf_error`，断言落到的 `ConvertError` 变体 + `code()`。
升 anydoc 时该测试若挂即暴露语义变更。**不依赖 (b)**，但 ADR 记录：上游若暴露
`map_error`，本库应改为转发以彻底消除重复（列为"未来优化"）。

#### S2：删除 main.rs 冗余 mod error

**Symptom**：[main.rs:10, 49-51](../../src/main.rs#L49-L51) 定义 `mod error { pub type Result<T> = ... }`
并 `use crate::error::Result;`；但 [lib.rs:24](../../src/lib.rs#L24) 已
`pub use error::{ConvertError, Result};`，`anydoc_ocr::Result` 已公开。

**备选权衡**：
- (a) 删 `mod error`，改 `use anydoc_ocr::Result;`
- (b) 保留，理由"binary 自治" → 无实际收益，徒增同名混淆

**决策**：采 (a)。删 `mod error {...}` 块与 `use crate::error::Result;`，改
`use anydoc_ocr::Result;`。`ConvertError` 已 `use anydoc_ocr::ConvertError;`，无需改。

#### S3：malformed 提示覆盖运行时/环境错误

**Symptom**：[error.rs:74-78](../../src/error.rs#L74-L78) `runtime()` 把 ORT/pdfium
失败归 `Malformed`（ADR-0006 §3 接受，因不能 fork 加变体）；
[main.rs:142](../../src/main.rs#L142) `error_hint` 对 `malformed` 返
"文档损坏或格式错误，检查文件是否完整或被截断"——对"找不到 libpdfium.so"/
"ORT 推理失败"错误归因误导。`detail` 完整错误仍打印，影响限于"提示"行。

**备选权衡**：
- (a) `error_hint` 扫 `detail` 关键词（"libpdfium"/"onnxruntime"/"OCR 推理"）给环境类提示
  → 字符串匹配脆弱，关键词漏判即退化为旧提示
- (b) 软化 `malformed` 提示文案，覆盖两类原因 + 指向 detail → 诚实承认歧义，无脆弱匹配
- (c) 接受现状，ADR 记录权衡 → detail 已暴露真因，但"提示"行仍误导
- (d) 给运行时错误可区分信号（如 `part` 固定前缀 `"runtime:"`）→ 污染 part 语义，
  part 本是"失败位置"

**决策**：采 (b)。`malformed` 提示改为
`"文档损坏或运行时错误（如 ORT/pdfium 未配置）— 详见错误详情，检查文件完整性或运行环境"`。
不区分 sub-case（detail 已让用户定位真因），但不再单押"文档损坏"。比 (a) 健壮、
比 (c) 诚实、比 (d) 干净。ADR-0006 §3 的 `runtime()→Malformed` 权衡不变。

### 细化开发计划

4 修复均为局部改动（每项 < 15 行），分 2 阶段执行，每阶段可独立编译验证。

**阶段 A：main.rs 统一提示 + 冗余清理（W1 + S2 + S3）**

三处同在 main.rs，一次编辑完成：
1. 删 `mod error {...}` 块 + `use crate::error::Result;`（S2）
2. 加 `use anydoc_ocr::Result;`
3. 改 `error_hint` 的 `malformed` 文案为 S3 决策文本
4. stdin 路径（[main.rs:72-74](../../src/main.rs#L72-L74)）：`?` 改 `match`，
   `Err(e) => { eprintln!("转换失败: {e}\n  提示: {}", error_hint(&e)); std::process::exit(1); }`
5. 单文件路径（[main.rs:82-83](../../src/main.rs#L82-L83)）：同上
6. batch 路径（[main.rs:127-128](../../src/main.rs#L127-L128)）：保留现状（错误隔离继续）

**阶段 B：error.rs 语义锁定测试（S1）**

在 [error.rs](../../src/error.rs) 末尾加：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pdf_inspector::PdfError;

    #[test]
    fn from_pdf_error_maps_each_variant() {
        assert!(matches!(from_pdf_error(PdfError::Encrypted), ConvertError::Encrypted));
        assert_eq!(from_pdf_error(PdfError::Encrypted).code(), "encrypted");
        assert!(matches!(from_pdf_error(PdfError::InvalidStructure),
                         ConvertError::Malformed { .. }));
        assert_eq!(from_pdf_error(PdfError::InvalidStructure).code(), "malformed");
        assert!(matches!(from_pdf_error(PdfError::NotAPdf("x".into())),
                         ConvertError::Malformed { .. }));
        assert!(matches!(from_pdf_error(PdfError::Parse("x".into())),
                         ConvertError::Malformed { .. }));
        assert!(matches!(from_pdf_error(PdfError::Io(std::io::ErrorKind::NotFound.into())),
                         ConvertError::Io(_)));
        assert_eq!(from_pdf_error(PdfError::Io(std::io::ErrorKind::NotFound.into())).code(), "io");
    }
}
```

**验证**：
- `cargo build`（需 ORT/pdfium 环境就绪：`ORT_LIB_PATH`/`PDFIUM_LIB_DIR`/`LD_LIBRARY_PATH`）
- `cargo test --lib from_pdf_error`（S1 单元测试，不依赖 ORT 运行时，仅 PdfError 构造）
- `cargo test --test error_classification --test batch_golden`（W1/S3 不改 code()，
  既有断言应全绿）
- 手测：`anydoc-ocr tests/samples/encrypted.pdf` 应见"提示: 文档已加密..."
  （W1 修复前无此行）；`anydoc-ocr <缺 pdfium 环境的 PDF>` 应见 S3 软化文案

**本沙箱限制**：`third_party/ort/` 缺失致 ort-sys 链接失败，无法重跑测试。
代码改动完成后需在 ORT 环境就绪的机器验证。S1 单元测试仅依赖 `pdf_inspector`
（编译期，非运行时 ORT），受影响最小。

### 审计跟进不做项

- 不加差异化退出码（YAGNI，留待脚本集成诉求出现时另开 ADR）
- 不向 anydoc 上游 PR 暴露 `map_error`（依赖外部时间，记为未来优化）
- 不改 `runtime()→Malformed` 映射（ADR-0006 §3 既定，不能 fork 加变体）
- 不做 `detail` 关键词扫描区分 sub-case（脆弱，detail 已暴露真因）

## 关联

- ADR-0005（批处理 + 跨文档流水线）：本 ADR 修复其 brooks-review 发现的
  静默失败 Critical，并补全错误隔离的"类型化"维度
- ADR-0001（不做全套 IR）：错误处理在现有类型上跑，无需 IR
- 上游 anydoc [error.rs](file:///root/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/anydoc-0.1.9/src/error.rs)
  ConvertError 定义
- 上游 pdf-inspector [lib.rs:6077](file:///root/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/pdf-inspector-1.14.2/src/lib.rs#L6077)
  PdfError 定义
- 上游 ofd-core [error.rs](file:///root/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ofd-core-0.3.0/src/error.rs)
  OfdError 定义（无加密变体）
