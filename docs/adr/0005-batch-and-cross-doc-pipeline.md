# ADR-0005: 批处理入口 + 跨文档流水线 + 目录递归遍历

- 状态: Accepted
- 日期: 2026-08-15
- 决策者: 架构 review
- 后续: ADR-0006（错误处理统一）补充本 ADR 错误隔离的类型化维度

## 背景

当前 CLI（[main.rs](../../src/main.rs)）只接受单文件 `input: String`。处理 N 个文档 =
N 次进程启动 = N 次模型冷加载（实测 ~0.5s/进程，含 ORT 线程池初始化）+
N 次 ORT 全局线程池提交。`OcrEngine` 的 `static CACHE`（[ocr_engine.rs](../../src/ocr_engine.rs)）本就是
为跨文档复用设计的，但单文件 CLI 让它永远只命中一次就随进程退出。

同时 `PagePipeline`（[pipeline.rs](../../src/pipeline.rs)）是 per-document：每个 doc 自己
spawn render 线程 + rayon scope + channel。处理多个小 PDF（1-3 页）时：

- 文档边界处 OCR 池空转（doc1 OCR 跑完 → pipeline 结束 → doc2 重新 spawn）
- per-doc setup 开销（thread spawn、channel 创建、rayon scope 建立）反复付

MinerU 工程实践（master 分支核验）对齐：
- `mineru -p dir` 接受目录，单进程跑整批
- `AtomModelSingleton` 跨文档复用模型（按 `(模型名, 设备, 语言)` 缓存）
- `doc_analyze_streaming` 跨文档 64 页处理窗口——文档边界只是 idx 跳变，pipeline 不停
- 预分流：只把需 OCR 的页送进处理窗口，文字型走快速路径

## 决策

**三项一起做，按层次落地**：

1. **目录递归遍历**：CLI 接受文件或目录，目录递归展开为文件列表
2. **批处理入口（候选 1）**：`BatchConverter` 持有 `OcrEngine` 句柄，跨文档复用
3. **跨文档流水线（候选 2）**：`PagePipeline` 升档为跨文档 render↔OCR

## 理由

### 1. 目录递归遍历

现状：CLI 单文件输入。用户处理目录只能 shell 循环 `for f in dir/*.pdf; do anydoc-ocr "$f"; done`，
每个文档独立进程，模型反复加载。

决策：CLI `input` 改为接受文件或目录。目录递归遍历（含子目录）收集所有受支持文件
（PDF/OFD/anydoc 支持的办公格式），过滤掉临时文件和隐藏文件。

理由：
- 对齐 MinerU `-p dir` 的目录批处理入口
- 目录递归而非仅顶层——真实场景文档按目录树组织（如 `2025/01/`、`2025/02/`）
- 递归遍历复杂度小（`walkdir` 或手写 `fs::read_dir` 递归），集中在 CLI 层

文件过滤规则：
- 扩展名匹配：`.pdf` `.ofd` `.docx` `.doc` `.xlsx` `.xls` `.pptx` `.ppt`（大小写不敏感）
- 跳过隐藏文件和隐藏目录（`.` 开头）
- 跳过 `.tmp`/`.crdownload` 等临时文件

### 2. 批处理入口（候选 1）：`BatchConverter` 新模块

现状：`convert_to_markdown(&Path, &ConvertOptions) -> Result<String>` 是单文件 API。
每个 doc 内部自己 `OcrEngine::build`（[pdf/mod.rs:49](../../src/pdf/mod.rs#L49)、
[ofd/mod.rs:171](../../src/ofd/mod.rs#L171)），靠 `static CACHE` 隐式命中。

决策：新增 `src/batch.rs`，定义 `BatchConverter` 深模块：

```rust
pub struct BatchConverter {
    engine: Option<Arc<OcrEngine>>,  // 懒构建：首个图片型 doc 触发
    opts: ConvertOptions,
}

impl BatchConverter {
    pub fn new(opts: ConvertOptions) -> Self;
    pub fn convert_many(&mut self, paths: &[PathBuf]) -> Vec<Result<String>>;
}
```

设计要点：
- `engine: Option`——文字型文档批处理不触发 build，零开销。首个图片型 doc 触发
  `OcrEngine::build`，后续复用同一实例
- `convert_many` 内部对每个 path 调 `convert_to_markdown`（完整分流：detect → pdf/ofd →
  text_layer 或 OCR）。**候选 1 不改分流逻辑，OCR 路径代码一行不动**
- 错误隔离：单个 doc 失败（损坏的 PDF、渲染失败）不炸整批，`Vec<Result<String>>`
  每个文档独立 Result
- `convert_to_markdown(&Path)` 保留为公开单文件 API，委托 `convert_many(&[path])`

理由：
- `BatchConverter` 是深模块：engine 生命周期 + 调度集中。删除测试——删掉后 engine build
  散到 N 次调用，N×0.5s 冷加载复杂度重现 → earning its keep
- 候选 2 的跨文档 pipeline 需要一个持有 engine 句柄的宿主，`BatchConverter` 正是这个位置
- 零精度风险：不改 OCR 路径，golden 不变

收益：100 个图片型文档省 99×0.5s = ~50s 模型冷加载。收益与文档的图片型占比成正比。

### 3. 跨文档流水线（候选 2）：`PagePipeline` 升档

现状：`RenderItem = Result<(usize, RgbImage), (usize, anyhow::Error)>`（idx 是单文档页号），
`run()` 返回 `Vec<(usize, StructureResult)>`。render 闭包在专属线程 open **一个** doc +
逐页渲染。

决策：`PagePipeline` 升档为跨文档，idx 改复合键：

```rust
pub(crate) type RenderItem = Result<((usize, usize), RgbImage), ((usize, usize), anyhow::Error)>;

pub fn run(self) -> Result<Vec<((usize, usize), StructureResult)>>;
```

设计要点：
- **idx 编址**：复合键 `(doc_idx, page_idx)`。语义清晰，类型层面显式"哪个文档哪一页"。
  改 `RenderItem` 签名，PDF/OFD 两调用方的 render 闭包同步改
- **预分流**：`BatchConverter::convert_many` 内部先 rayon 并行对每个 path 做 detect +
  text_layer 判定。文字型 doc 直接 `text_layer_markdown`（rayon 并行），图片型 doc 收集到
  `ocr_paths` 进跨文档 `PagePipeline`。pipeline 只处理图片型，职责单一
- **跨文档 render 闭包**：接受 `Vec<PathBuf>`，内部逐 doc open + 逐页渲染，产出
  `((doc_idx, page_idx), img)`。render 仍单线程（PdfDocument/OfdReader 非 Send，
  ADR-0002 模式不变），OCR 池跨文档消费
- **GFM 边界**：跨文档 pipeline 产出按 `doc_idx` 分组，每个 doc 自己
  `structure_results_to_gfm`。`DocumentEmitter` 非 Send + 跨页表不跨文档，
  ADR-0003 双段模式不变——跨文档的只有 render↔OCR 段，GFM 收尾仍是 per-doc
- **OFD 第一遍页型判定**：跨文档时每个 doc 各自的 `OcrPendingImage` 列表合并传入 render 闭包

理由：
- 文档边界 OCR 池空转消除 + 小文档 setup 摊薄，对齐 MinerU `doc_analyze_streaming` 的
  跨文档处理窗口
- 复合键而非全局连续页号——避免 global_idx 映射表的隐式状态，语义在类型层面显式
- 预分流而非混合 pipeline——文字型 doc 0.03s，进 pipeline 反而引入 idx 编址复杂度；
  pipeline 职责单一（只管图片型 doc 的 render↔OCR）

收益：与文档平均页数成反比——页数越少（1-3 页小文档），per-doc setup 占比越高，候选 2
收益越大。8 页文档 setup 占比已低，候选 2 收益小；100 个 1 页文档 setup 占比高，收益显著。

## 后果

### CLI 改动

[main.rs](../../src/main.rs) `input: String` 改为接受文件或目录：

```rust
input: PathBuf,  // 文件或目录
```

目录递归遍历收集文件列表，单文件直接 `[path]`。stdin `-` 仍兼容（单元素临时文件）。
对齐 MinerU `-p` 接受文件或目录。

### 实施节奏：分步

1. **候选 1 + 目录遍历先做**——`batch.rs` + `BatchConverter` + CLI 目录输入。拿 engine
   复用确定收益，零精度风险，golden 不变
2. **候选 2 后做**——升级 `PagePipeline` 跨文档，改 idx 编址为复合键，PDF/OFD render
   闭包同步改。需加 batch golden（2-3 个小文档一起跑验证 idx 分组正确性）

### 风险

- 候选 1：零精度风险（不改 OCR 路径）
- 候选 2：`RenderItem` 签名变更，PDF/OFD 两调用方闭包同步改；OCR 路径页序契约不变
  （复合键保序），但需 batch golden 验证跨文档 idx 分组
- 目录遍历：递归深度无限制可能触发符号链接环（用 `walkdir` 或限制深度兜底）
- 混合目录（文字型 + 图片型文档）：预分流处理，两类各自走快速路径/pipeline，不互相阻塞
- **错误隔离的类型化**（brooks-review Critical 修复 + ADR-0006）：候选 2 落地后
  发现 batch.rs 兜底分支曾把"doc 打开失败被 pipeline 跳过"标为 `Ok(String::new())`，
  导致 main.rs 写出空 .md 并计入成功（静默数据丢失）。已修复为 `Err`，但暴露
  错误类型信息在边界处丢失的问题。ADR-0006 决策全库统一用 `anydoc::ConvertError`
  替代 `anyhow::Error`，把错误分类（Encrypted/Malformed/MissingPart 等）从源头
  保留到 main.rs，按 `code()` 给用户精准提示。本 ADR 的"错误隔离"从"每文档独立
  Result"升档为"每文档独立 **类型化** Result"。

### 不做项

- 不改 GFM 收尾为流式（ADR-0003 已延后，实测 GFM 占比 <1% 验证决策正确）
- 不做跨文档结果缓存/断点续算（ADR-0001 已决策不做全套 IR）
- 不内置多进程/多 GPU 调度（MinerU 也无内置，靠外部多进程）

## 关联

- ADR-0001（不做全套 IR）：批处理在现有类型上跑，无需 IR
- ADR-0002（流水线）：候选 2 是该 ADR 的跨文档扩展，render 专属线程 + OCR rayon 池
  模式不变，仅 idx 编址升级为复合键
- ADR-0003（双段先做）：候选 2 不触碰 GFM 段，跨文档的只有 render↔OCR 段
- ADR-0004（不加 wired adapter）：与批处理无关
- ADR-0006（错误处理统一）：补充本 ADR 错误隔离的类型化维度。brooks-review 发现
  候选 2 的静默失败 Critical（`Ok(空串)` 伪装成功）后，ADR-0006 决策全库统一用
  `anydoc::ConvertError` 替代 `anyhow::Error`，从源头保留错误分类
