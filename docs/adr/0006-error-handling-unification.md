# ADR-0006: 错误处理统一——复用 anydoc::ConvertError

- 状态: Accepted
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
- anyhow 的 `Context` trait 在边界处改用 `map_err(|e| ConvertError::malformed(format!(...)))`
  替代，语义等价
- 移除 anyhow 后 `Cargo.toml` 删掉 anyhow 依赖，减少一个外部 crate

**具体移除点**：
- `src/lib.rs`：删 `pub type Result<T> = anyhow::Result<T>;`，改 `pub type Result<T> = std::result::Result<T, anydoc::ConvertError>;`
- 各模块 `use anyhow::anyhow;` / `use anyhow::Context;` 删除，改用 `ConvertError` 构造
- pipeline 内部 `anyhow::Error` 改 `ConvertError`，`anyhow::bail!` 改 `return Err(ConvertError::malformed(...))`
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

### 实施节奏：分 3 步递进

**第 1 步：PDF 路径**（源头分类 + batch 预分流过滤）
- [text_layer.rs](../../src/pdf/text_layer.rs) `extract_text_with_positions` 错误按
  `PdfError` 分类返 `ConvertError`，不再吞 `Ok(None)`
- [pdf/mod.rs](../../src/pdf/mod.rs) `convert_pdf`/`convert_pdf_ocr` 签名改
  `Result<.., ConvertError>`
- [batch.rs](../../src/batch.rs) 预分流阶段 `Err` 直接标错不送 OCR；force_ocr 时
  仍调 text_layer 做加密预检
- 验证：现有 `batch_isolates_corrupt_pdf_as_err` 测试改断言 `ConvertError::Malformed`；
  golden 不变（健康文档路径不变）

**第 2 步：OFD + anydoc 格式映射**（边界统一）
- [ofd/mod.rs](../../src/ofd/mod.rs) `OfdError → ConvertError` 映射（按上方映射表），
  移除 `anyhow::anyhow!("...")` 构造，改用 `ConvertError::malformed(...)` /
  `ConvertError::MissingPart { .. }` 等
- [convert.rs](../../src/convert.rs) `convert_to_markdown` 签名改
  `Result<String, ConvertError>`，anydoc 格式透传 `ConvertError`（移除
  `map_err(|e| anyhow!("{e}"))`）
- [lib.rs](../../src/lib.rs) `Result<T>` 别名改 `Result<T, ConvertError>`，
  删 `pub type Result<T> = anyhow::Result<T>;`
- [pipeline.rs](../../src/pipeline.rs) / [pdf/render.rs](../../src/pdf/render.rs)
  内部 `anyhow::Error` 全改 `ConvertError`，`anyhow::bail!` 改
  `return Err(ConvertError::malformed(...))`，移除 `use anyhow::*`
- `Cargo.toml` 删 `anyhow` 依赖
- 验证：OFD golden 不变；补 OFD 损坏样本测试

**第 3 步：main.rs 提示 + 测试样本**
- [main.rs](../../src/main.rs) 按 `code()` 精准提示（上方代码）
- 补 4 测试样本（覆盖三类）：
  - 加密 PDF（qpdf 加密生成）
  - 损坏 PDF（已有 `batch_isolates_corrupt_pdf_as_err`，改断言类型）
  - 加密 docx（OOXML EncryptionInfo）
  - 损坏 docx（zip 截断）
- 验证：每样本断言对应 `ConvertError` 变体 + `code()` 字符串

### 风险

- **签名变更连带**：`Result<T>` 别名改 `ConvertError` 后，所有 `?` 传播点需检查
  `From` impl 是否覆盖。anydoc 已 impl `From<io::Error>`，PDF 路径需加
  `From<PdfError>`（复用 anydoc 私有 map_error 逻辑，提到 anydoc-ocr 本地 impl）
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
