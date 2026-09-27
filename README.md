# anydoc-ocr

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/rust-1.95%2B-orange.svg)
![platform](https://img.shields.io/badge/platform-Linux%20x86__64%2Faarch64-lightgrey.svg)

把 PDF / OFD（含图片型、扫描件）转成干净 GitHub-Flavored Markdown 的 Rust CLI 与库。文字型从版式还原阅读顺序，图片型自动走 OCR，表格结构化重建——一种输入、一致输出，面向办公公文与资料归档场景。

## Highlights

- **四类通道自动分流**：按文件魔数识别 PDF/OFD 的文字型 vs 图片型，各走最优路径（细则见下节）。
- **图片型 OCR 回退**：扫描件/图片型 PDF、OFD 自动渲染后跑「版面检测 → 文本检测 → 文本识别 → 表格结构识别」管线，**默认档即 MinerU 4.0 basic 同款流程与模型**，另可按精度/速度降到小模型档。
- **坏字体自动回退整页 OCR**：文字层检测到乱码（GID 坏字体、U+FFFD/私有区占比 ≥20%）自动整页重检，PDF 另有浅检+深检两级兜底。
- **表格结构化重建 + 跨页合并**：文字层表格网格重建、图像型 SLANet+ 识别，统一输出 `<table>` HTML 并合并跨页重复表头。
- **自研阅读顺序还原**：单/双/多列、竖排、标题层级、列表项——同一算法通用于文字层与 OCR 通路。
- **页级健壮性**：坏页跳过并告警，不整篇失败；单文档错误类型化（`ConvertError`）给精准提示。
- **线程模型自控**：`--threads` 控页级并行，进程级 ORT 线程池 `intra = max(1, 核心数/threads)`，总线程≈核心数，消除 rayon×ORT 超额订阅。
- **离线可用**：模型 `$OAR_HOME` 缓存常驻、可 `ANYDOC_MODEL_DIR` 直载本地 ONNX；可打包单文件自解压安装器一键部署。

## 处理流程

按文件魔数自动分流（[`detect.rs`](src/detect.rs)）：

| 输入 | 处理方式 | 输出 |
|------|----------|------|
| 文字型 PDF | `pdf-inspector` 提取文本 + 自研阅读顺序还原；含表格页回退 OCR | 纯文本 GFM |
| 图片型 PDF | PDFium 渲染（或「单图满页」直提内嵌像素，ADR-0008）→ OCR 管线 | 结构化 GFM |
| 文字型 OFD | `ofd-core` 文本提取（坐标排序）+ 表格网格重建 | 纯文本 GFM |
| 图片型 OFD | `ofd-core` 渲染 → OCR 管线 | 结构化 GFM |
| 其他（docx 等） | 回退 `anydoc` | Markdown |

OCR 管线：版面（layout）→ 文本检测（det）→ 文本识别（rec）→ 表格结构（SLANet+）。渲染与 OCR 默认共享一个跨文档 pipeline，批处理跨文档不停顿。

## 快速开始

依赖预编译原生库（ONNX Runtime 1.28.2、PDFium）与 Rust ≥ 1.95，先放库再构建：

```bash
# 1) 把预编译库放到 third_party/ 对应架构目录（见「构建」节）

# 2) x86_64 本机构建（脚本内含全部环境变量）
./scripts/build-x64.sh build --release

# 3) 首次 OCR 会从 ModelScope 自动下载模型
export OAR_HOME=~/.oar

# 4) 转换
target/release/anydoc-ocr 公文.ofd -o out.md
target/release/anydoc-ocr 扫描件.pdf -o out.md
```

## 使用（CLI）

```text
anydoc-ocr <输入文件或目录> [选项]
```

- `<input>`：`-` 表示 stdin（图片型会先落临时文件）；目录则递归批量转所有受支持文档。
- 单文件省略 `-o` 写 stdout；目录输入必须 `-o` 指定输出目录（保持相对结构，`.pdf/.ofd/.png/.jpg/…` → `.md`）。
- 位图输入（#12，对齐 MinerU 的 8 个扩展名）：`png/jpg/jpeg/webp/gif/bmp/tiff/jp2`。gif/tiff 只取**第一帧**；`jp2` 能识别但 `image` crate 无 JPEG2000 解码器，显式报 `unsupported` 而非静默走别的通道；长边 > `ANYDOC_RENDER_EDGE_CAP`（默认 3500px）在**解码前**显式报 `resourceLimit`，不做静默降采样（与 PDF 渲染路径的自动降 scale 不同——那是我方重排，这是用户原图）。EXIF 方向自动摆正后再 OCR。

| 参数 | 默认 | 含义 |
|------|------|------|
| `-o, --output <path>` | stdout | 输出文件；目录输入时为输出目录 |
| `--ocr-tier <mineru-basic\|tiny\|small\|medium>` | `mineru-basic` | OCR 引擎档。**默认即 MinerU 4.0 basic 档同款流程与模型**，日常无需传（见「模型档与精度」） |
| `--threads <n>` | `0` | OCR 推理页级并行度。`0` = 自动取可用并行度；进程级 ORT `intra=max(1,核心数/n)`，总线程≈核心数。内存受限环境（cgroup<8GB）可调小 |
| `--dpi <f32>` | `100` | 图片型渲染分辨率，允许区间 `[50, 400]`（越界/NaN 直接报错）。印刷体公文 `100` 零精度损失且比 `200` 快 33%；`80` 起脚注/小字开始漏检 |
| `--pages <expr>` | 全部 | 页码选择（**仅 PDF**，语法对齐 MinerU/docvortex）：1 基含端点、逗号分隔，如 `1-5,8`；`rN` 从末页倒数（`r3-r1` = 末三页）；`all` = 全部。排序去重、越界裁剪；倒序区间 / 与文档无交集 / 非法语法立即报错。所选外的页不抽取、不渲染、不进输出；非 PDF 或目录输入显式给页直接拒绝 |
| `--ofd-force-ocr` | off | 文字型 OFD 也强制走 OCR（重建表格结构） |
| `--pdf-force-ocr` | off | 文字型 PDF 当图片渲染后 OCR（图片型校准用） |
| `--text-only` | off | **绝不加载任何模型**的文字层直出（#13，MinerU `--ocr-mode txt` 的**更严**版）：PDF/OFD 只抽内嵌文字层，缺文字的页在 stderr 列页码警告（不静默）；纯扫描件/图片输入直接报 `needsOcr`（图片没有文字层可抽）；与 `--pdf-force-ocr`/`--ofd-force-ocr` 互斥（立即 `unsupported`）。用途：无网机器上的应急抽取与文字层调试 |

> 参数面已按"默认即 MinerU"收敛：`--ocr-layout`（换版面模型会把 PP-DocLayoutV2 整个替掉，与默认流程互斥）与 `--quality-route`（其语义是 tiny→small 升档，两端都不是默认档）从 CLI 撤下；两者在库 API 仍可用（`OcrConfig{layout}` / `ConvertRequest{quality_route}`，且 MinerU 档下路由自动让位，见 `quality::routing_applies`）。

示例：

```bash
anydoc-ocr 公文.pdf                       # 自动分流，写 stdout
anydoc-ocr 扫描件.pdf -o out.md           # 默认即 MinerU 档，无需任何参数
anydoc-ocr 长文档.pdf --pages 1-3,r1       # 只转前 3 页 + 末页
anydoc-ocr 公文.ofd --ofd-force-ocr       # 强制 OCR，重建表格
anydoc-ocr 低内存机.pdf --ocr-tier tiny    # 内存吃紧/离线时降档
cat 公文.pdf | anydoc-ocr - -o out.md     # stdin
anydoc-ocr 资料目录/ -o out_md/           # 目录批处理
```

## 模型档与精度

**默认档 `mineru-basic`** 与 MinerU 4.0 `basic` 档逐模型对齐（`--ocr-tier` 可切换，无需重编译）。注册表内的模型从 ModelScope 自动下载，按 `$OAR_HOME` 缓存（sha256 匹配则复用）。

| 档 | 版面 | 文本检测（det） | 识别（rec） | 公式 | 适用 |
|----|------|----------------|------------|------|------|
| `mineru-basic`（默认） | PP-DocLayoutV2 | PP-OCRv6 tiny 1.7MB | small 20.2MB | PP-FormulaNet_plus-M | MinerU 同款流程与后处理（score 0.45、IoU 去重、header/footer 重标） |
| `tiny` | PP-DocLayout-S | PP-OCRv6 tiny 1.7MB | 4.3MB | — | 内存受限、完全离线的目标机，极速 |
| `small` | PP-DocLayout-M | PP-OCRv6 small 9.4MB | 20.2MB | — | 中文覆盖最全，均衡 |
| `medium` | PP-DocLayoutV3 | PP-OCRv6 medium 59MB | rec 73MB | — | 复杂版式高精度（**ARM CPU 较慢**，慎批量） |

默认档的资产分两层，行为差别很大，这是 `mineru-basic` 唯一需要理解的配置点：

- **必需 7 件**（版面/det/rec/词典/表格三件）都在 ModelScope 注册表内 → 首跑自动联网下载（合计约 240MB，其中 PP-DocLayoutV2 占 214MB），CLI 会在下载前打一行 stderr 点名待下模型与体积，之后 `$OAR_HOME` 缓存常驻、离线复用。
- **公式 2 件**（`formula_m.onnx` 591MB + `ppformulanet_tokenizer.json`）**不注册、永不自动下载**，唯一来路是 `ANYDOC_MODEL_DIR`。缺这两件**不影响起跑**：版面/文本/表格全部照常，只是公式块不输出 LaTeX，并打一次性提示。

小模型三档（`tiny`/`small`/`medium`）通用：表格结构 `slanet_plus` + `pp-lcnet` 分类 + 中文表格词典；文档方向矫正 `pp-lcnet doc_ori`（0°/90°/180°/270° 自动转正）。`mineru-basic` 与 MinerU 口径一致，**不做**页面方向矫正（MinerU basic 无此环节）。

## 处理速度

性能受机型（CPU 核数/指令集）、`--threads`、`--dpi` 与模型档影响；以下为本项目实测参考（Linux x86_64，静态编译）：

| 场景 | 配置 | 耗时 |
|------|------|------|
| 24 页图片型 PDF（扫描公文） | `tiny` / 3 线程 / 100dpi | **≈ 18s**（≈ 0.75s/页，含渲染+OCR） |
| 印刷体公文 DPI 对比 | 52 页，100 vs 200 | 恢复率均 99.83%，100 比 200 快 **33%** |
| 单页扫描件 OCR | `mineru-basic`（默认） vs `tiny`，4 核 | ≈ 2.9s vs ≈ 0.6s（默认档换精度与 MinerU 一致性，非速度） |

要点：默认档比小模型档慢且重（峰值 RSS ≈1.9GB vs tiny ≈0.5GB，本机实测），这是"与 MinerU 对齐"的代价——内存受限或批量吞吐优先时用 `--ocr-tier tiny` 降档。分辨率仍比全局去噪更保精度：`100dpi` 是印刷体甜点，DPI≤80 起小字/脚注开始漏检；线程模型把页级并行与进程级 ORT 池配平到总线程≈核心数。

CLI 加 `ANYDOC_TIMINGS=1` 可在 stderr 输出分阶段计时（render/ocr/gfm...）。

## 模型缓存与内存

- 模型按 `(tier, layout)` 为键常驻缓存，跨文档/跨调用复用；库模式 `OcrEngine::clear_cache()` 可释放。
- 需更多档位/更大吞吐时，实测 `small` 行批 4/8/16/32 与 `tiny` 16/32/64 全部持平（intra 满核后批大小只改矩阵形状不改 FLOPs），故行批旋钮仅留给飞腾架机构复核，无默认超配。

## 库用法

核心 API：`convert_to_markdown(path, &ConvertRequest, &ForceFlags) -> Result<String>`，外加 `OcrEngine` 单例（`build`/`predict`/`clear_cache`）、`batch::BatchConverter`、类型 `DocKind`/`OcrTier`/`OcrLayout`/`QualityRoute`/`ConvertError`。

```rust
use anydoc_ocr::{convert_to_markdown, ConvertRequest, ForceFlags, OcrConfig, OcrTier, OcrLayout, ParallelConfig, RenderConfig};

let opts = ConvertRequest {
    render: RenderConfig { dpi: 100.0 },
    // 不写 ocr 字段即默认 mineru-basic（与 CLI 同默认档）
    ocr: OcrConfig { tier: OcrTier::Small, layout: OcrLayout::Doc },
    parallel: ParallelConfig { page_parallel: 4, ort_intra: 0 },
    ..Default::default()
};
let force = ForceFlags::default();
let md = convert_to_markdown(std::path::Path::new("公文.ofd"), &opts, force)?;
```

> 库模式 `OcrEngine::predict` 页序契约被破坏时返回 `Err` 而非 panic，宿主进程不会被打翻。

## 环境变量

| 变量 | 说明 |
|------|------|
| `OAR_HOME` | oar-ocr 模型缓存/下载根目录（首用自动从 ModelScope 下载） |
| `ANYDOC_MODEL_DIR` | 本地 ONNX 模型目录（绝对路径）。设置后从该目录**直载**，不走 `$OAR_HOME` 缓存/下载，用于离线/内网；缺某模型回退裸名下载。注意不能把自备模型放 `$OAR_HOME` 用裸名（会命中缓存分支被 size/hash 不符静默重下覆盖）。**默认档的公式识别两件（`formula_m.onnx` + `ppformulanet_tokenizer.json`）只能由此提供**——不在 ModelScope 注册表，缺则自动跳过公式识别（正文/表格不受影响） |
| `ANYDOC_ORT_INTRA_THREADS` | 强制覆盖进程级 ORT intra-op 线程数（调试用）。必须在任何 ONNX session 创建前生效。未设置时自动：池=1 → 全核；池>1 → `核心数/池`（防并发 run 互抢） |
| `ANYDOC_ORT_SESSION_POOL` | A1：每模型加载 N 份 ORT session（1–8，默认 1=上游行为）。>1 时引擎放开页级并发推理（pipeline 多消费者 + 轮转分池）；4 核实测（2×24 页批，t=4）wall −20~30%，峰值 RSS +约 0.8GB（每 session 独立 arena），输出逐字节确定一致。CPU-only 专用（CUDA/TensorRT 恒回落 1） |
| `ANYDOC_NO_HYBRID` | 存在即关闭 PDF 混合路由（B）：有文字层的 PDF 不再对缺页自动补 OCR，回到旧行为（文字层直出，扫描件页可能缺失），用于 A/B 回滚 |
| `ANYDOC_NO_RAW_EXTRACT` | 存在即禁用 ADR-0008 单图满页直提，扫描件强制走 PDFium 整页光栅化（MinerU 回归对齐用：低分辨率内嵌图也被放大到目标 DPI 网格） |
| `ANYDOC_RENDER_EDGE_CAP` | 整页渲染长边上限（px，>0 生效；**默认 3500**）。超则整体降 scale，对齐 docvortex/MinerU 3500px 像素网格（PDF 逐页、OFD 按页物理框等价换算 dpi）；A4×100dpi≈1169px 常规公文不触发。非法值回落 3500 |
| `ANYDOC_MAX_INPUT_BYTES` | 输入字节上限（默认 200 MiB，对齐 MinerU 上传档）。stdin 有界读、文件入口（含 HTML/CSV 等 anydoc 通道）预检，超限显式 `resourceLimit` 拒绝、不截断；非法值（不可解析/≤0）回落默认 |
| `ANYDOC_NATIVE_TEXT_CHARS` | 单页原生文字层字符上限（默认 65535，对齐 MinerU `MAX_NATIVE_TEXT_CHARS_PER_PAGE`）。超限页放弃文字层抽取直判 OCR 缺页，防超大文字层拖垮抽取；非法值回落默认 |
| `ANYDOC_MAX_PAGES` | 单文档页数上限（默认 1000，对齐 MinerU `max_pages_per_file`）。PDF 在 classify 元数据阶段、OFD 在逐页判定循环内拦截，超限显式 `resourceLimit` 报错，发生在渲染/OCR 之前；非法值回落默认 |
| `ANYDOC_RICH_TEXT` | 存在即开启 PDF 文字层**行内样式**注入（借鉴 MinerU `prepare/apply_text_evidence`）：消费 pdf-inspector 的 bold/italic/underline/strikeout 证据，产出 `**粗**`/`*斜*`/`<u>下划线</u>`/`<s>删除线</s>`；标题启发式在剥标记后的判定视图上跑，前缀与样式共存（`## **一、总则**`）。**默认关闭**（守护现网字节一致），精度增益按语料自行 A/B——`is_bold` 部分来自字体名启发，中文公文加粗小标题已由标题前缀承担 |
| `ANYDOC_HEADINGS_LAYOUT` | 存在即开启**布局驱动标题分级**（仅 OCR 通路，借鉴上游 `infer_paragraph_title_levels`）：默认路径只有编号语义一条信号，无编号标题（"总则""适用范围"）一律抹平为 `##`；开启后追加行高、缩进两条 k-means 布局信号做三信号加权投票（语义 2 > 行高 1 = 缩进 1），把同级标题按字号/缩进拉开。编号命中的标题级别不变，故开关只影响原本回落 `##` 的那批。**默认关闭**（字节一致） |
| `ANYDOC_NO_SEAL_OCR` | 存在即关闭**印章文字识别**（**#10b 行为变更**：此前为 `ANYDOC_SEAL_OCR` 存在即开启、默认关闭；现**默认开启**，对齐 MinerU basic=medium 档默认跑 seal OCR）。链路不变：版面模型的 `Seal` 元素 → 页图裁剪 → 印章专用 DB 检测（`pp-ocrv4_mobile_seal_det`/`seal_ppocrv4_det`，auto-download 或 `ANYDOC_MODEL_DIR`）→ 行框摆正 → tier 同款 rec → 输出 `【印章】…` 行。**代价可控**：页面无 `Seal` 元素时 `seal_pass` 早退、一个额外模型都不加载，成本只落在真含章的页上。**限制**：环排（弧形）公司名当前显式跳过（弧行摆正只会产出残缺字，取证见 `BACKLOG.md` #5a），章内直排文字（"专用章"等）可识别 |
| `ANYDOC_SEAL_OCR` | （遗留别名）#10b 前的开关名，现**恒为等价默认值的 no-op**——设不设都是开，绝不把默认翻转成关（老脚本 `ANYDOC_SEAL_OCR=1` 行为不变）。与 `ANYDOC_NO_SEAL_OCR` 同时设置时以关闭为准并 stderr 提示一次 |
| `ANYDOC_TABLE_FILL` | 存在即开启**空单元格 OCR 回捞**（对齐 MinerU flash 表填充）：表格网格中文字为空的格单独裁剪 → 重跑一遍行检测（`box_thresh 0.5`/`unclip 1.6`，比整页尺度更宽松）→ rec → 文本写回并按 `structure_tokens` 的 (row,col) 网格重建 HTML（含 colspan/rowspan 落位）。无空格时对输出逐字节无影响；无结构 token 时保守不改写 HTML。默认关闭 |
| `ANYDOC_WIRELESS_CELLS` | **A/B 用，默认关闭**（#7 结论）：存在即给 `mineru-basic` 档接无线表"单元格检测 → cells→HTML"通路（额外加载 `rt-detr-l_wireless_table_cell_det.onnx`，129MB，走 auto-download）。实测**不支持把它设为默认**：入库件上与现状持平但多包一层 `<tbody>`，行高更紧的版式上把 colspan 撑成整行并丢格，而现状（slanet_plus 通用兜底）两种版式都正确——即它多付确定的 129MB + 每页一次检测，换来的是对版面几何更敏感的结构。要按自家语料复核时开它对照（`tests/wireless_table.rs` 钉住了现状的 span 基线与"有线表不受该开关影响"）。MinerU basic 用的是 UNet 分割方案，本仓走的是另一条技术路线，**对齐的是能力不是实现** |
| `ANYDOC_RENDER_TRACE` | 存在即逐页打印渲染路径（直提 / 回退整页渲染）到 stderr，排查 ADR-0008 直提命中用 |
| `ANYDOC_DUMP_DIR` | 目录路径：存在即逐页落 StructureResult + 页像素尺寸 JSON，供 MinerU 框级 IoU / 阅读顺序对比 |
| `ANYDOC_REC_BATCH` | 覆盖 rec 行批大小（上游默认 tiny=16 / small+medium=4；默认不启用） |
| `ANYDOC_TIMINGS` | 存在即输出分阶段计时到 stderr |
| `ANYDOC_DEBUG_GFM` | 存在即启用 GFM 适配器调试输出 |
| `ORT_LIB_LOCATION` / `ORT_INCLUDE_LOCATION` | ORT 预编译库路径（构建期） |
| `ORT_PREFER_DYNAMIC_LINK` | 置 `1` 走 ORT 动态链接（构建期，**必须**：ort 2.0 rc 对静态校验严格） |
| `PDFIUM_LIB_DIR` | PDFium 库路径（构建期与运行期） |
| `ANYDOC_GOLDEN_OCR` / `ANYDOC_GOLDEN_UPDATE` | golden 测试开关（见「测试」） |

## 构建

依赖预编译原生库（ORT 1.28.2、PDFium），先放到 `third_party/` 对应架构目录，再用环境变量指明位置。

### x86_64 本机构建

```bash
./scripts/build-x64.sh build --release
```

等效手动方式：

```bash
export ORT_LIB_LOCATION=$PWD/third_party/ort/x64/onnxruntime-linux-x64-1.28.2/lib
export ORT_INCLUDE_LOCATION=$PWD/third_party/ort/x64/onnxruntime-linux-x64-1.28.2/include
export ORT_PREFER_DYNAMIC_LINK=1
export PDFIUM_LIB_DIR=$PWD/third_party/pdfium/x64/lib
cargo build --release
```

### 交叉编译到飞腾 aarch64

`.cargo/config.toml` 已为 `aarch64-unknown-linux-gnu` 配 `cortex-a72+neon` 与 `$ORIGIN/lib` rpath（**勿**用 `RUSTFLAGS` 环境变量，会整体覆盖 config 的 rustflags）：

```bash
./scripts/build-aarch64.sh
```

### 单文件分发（自解压安装器）

`./scripts/package-single.sh [auto|aarch64|x86_64] [tiny|small]` 产出 1 个自解压安装器，目标机自动检测架构、首次运行一键部署（装 CJK 字体 + `~/.local/bin/anydoc` 命令）。第二参数选内嵌模型档：`small`（~153M）正常版——**同时内嵌 tiny 全套**（表格/方向件共用去重）；`tiny`（~73M）极简版仅在需要最小体积时单独构建。

> CLI 默认档已是 `mineru-basic`，而包内只嵌小模型，所以**两种包的启动器都会**在用户未显式传 `--ocr-tier` 时注入包内档位（否则 `anydoc 扫描件.pdf` 会去联网下载包里根本没有的 MinerU 模型）。medium 与 mineru-basic 均不内嵌，用 `ANYDOC_MODEL_DIR` 外置。

CI 已自动化：`.github/workflows/release.yml` 在 push `v*` tag 时构建 Linux x86_64/aarch64 × small 共 2 个 `.run`（单包双档）并发布到 GitHub Release（手动触发则进 artifact）。

```bash
./anydoc-ocr-linux-x86_64-small.run 公文.pdf -o out.md   # 目标机首次运行即部署
anydoc 公文.ofd -o out.md                                 # 之后新终端直接用 anydoc 命令
```

### 运行环境

```bash
export OAR_HOME=~/.oar
export LD_LIBRARY_PATH="$ORT_LIB_LOCATION:$PDFIUM_LIB_DIR"   # 仅开发期；打包产物自带 rpath
```

OFD 中文渲染需 CJK 字体：`./scripts/install-font.sh`（装 `fonts/NotoSansCJK-Regular.ttc`，fontdb 免 fc-cache 生效）。

## 测试

- **Golden 回归**（`tests/golden.rs`）：对样本跑 `convert_to_markdown`，输出 SHA-256 与 `tests/golden/snapshots/*.sha256` 比对，守护行为不变。
  ```bash
  cargo test --test golden                          # 非 OCR 样本（不触发模型下载）
  ANYDOC_GOLDEN_OCR=1 cargo test --test golden      # 追加 OCR 样本
  ANYDOC_GOLDEN_UPDATE=1 cargo test --test golden   # 重生成基线（仅行为变更 ticket 用）
  ```
- 校准与 DPI 扫描：`tests/bench.sh`、`tests/calibrate.py`（字符集内容恢复率）、`tests/dpi_sweep.sh`。

## 目录结构

```
src/
  main.rs            CLI 入口（clap 参数、batch 目录递归）
  lib.rs             库入口：convert_to_markdown / ConvertRequest / OcrTier / OcrEngine 等
  convert.rs         格式分流（detect → pdf/ofd/anydoc）
  detect.rs          PDF/OFD/Other 魔数检测
  pdf/               PDF 文字层提取 + 阅读顺序、渲染（render.rs，含 ADR-0008 直提）、OCR 回退
  ofd/               OFD 提取 + 表格重建 + OCR 回退
  ocr_engine.rs      OcrEngine 单例缓存、进程级 ORT 线程池（init_runtime）
  models.rs          OcrTier/OcrLayout 定义与模型规格
  reading_order.rs   阅读顺序还原（PDF 文字层与 OCR 通路共用）
  table_grid.rs      文字层表格网格重建 + 跨页合并
  gfm_adapter.rs     OCR StructureResult → GFM
  pipeline.rs        跨文档渲染→OCR 管线（ADR-0005 候选 2）
  batch.rs           目录批量转换
  orientation.rs     文字层朝向分组/投票（旋转表块与正文各自重建）
  heading_levels.rs  布局驱动标题分级（行高/缩进 k-means + 编号语义投票，ANYDOC_HEADINGS_LAYOUT）
  seal.rs            印章内核（嵌套框去重、弧行判定、行文本规整，默认开/ANYDOC_NO_SEAL_OCR 关）
  ocr_post.rs        OCR 后处理层（印章文字识别默认开 #10b / 空单元格回捞默认关闭）
third_party/
  oar-ocr-core/      vendored（升级时 rebase；含 SIMD resize 加速本地 patch）
tests/               golden.rs + golden/snapshots + samples/
BACKLOG.md           已取证未实现项（弧排印章矫正 #5a、窗口化结论 #1）
scripts/             build-x64 / build-aarch64 / package-single / install-font
.cargo/config.toml   aarch64 交叉编译 rustflags（cortex-a72+neon + rpath）
```

## 依赖

| 包 | 版本 | 用途 |
|----|------|------|
| `oar-ocr` | 0.9.2（锁定） | 版面/OCR/表格结构推理（ONNX Runtime，PaddleOCR 系模型） |
| `anydoc` | 0.2.4（锁定） | 其他格式兜底（docx 等） |
| `ort` | 2.0.0-rc.13（锁定） | 进程级 ORT 线程池 API（与 oar-ocr-core 同版镜像） |
| `ofd-core` | 0.3.0 | OFD 文本提取与渲染 |
| `pdf-inspector` | 1.24 | 文字型 PDF 文本提取 |
| `pdfium-render` | 0.9.4 | PDFium 渲染 |

> 版本策略：深耦合/行为镜像/RC/0.x 演进期锁 `=`，纯 Rust 工具库用 `^`。`oar-ocr-core` 为本仓 vendored（`[patch.crates-io]`），内含 NEON SIMD 的 resize 加速 patch，升级时需 rebase。

## 已知限制

- **DPI ≤80 起漏检**：脚注/小字开始丢；印刷体公文 `100` 为甜点（快 33% 且零损失）。
- **默认档 `mineru-basic` 重且慢**：首跑需联网拉必需 7 件（合计约 240MB，其中 PP-DocLayoutV2 占 214MB），运行峰值 RSS ≈1.9GB、单页 ≈2.9s（tiny 档分别 ≈0.5GB / ≈0.6s）。离线目标机、内存受限或批量吞吐优先时显式 `--ocr-tier tiny|small`；公式件（不在注册表、永不自动下载）缺失时只不出 LaTeX，主链路照常。
- **`medium` 档在 ARM CPU 上慢**（det 59MB / rec 73MB），慎批量。
- **ORT 全局线程池仅首次生效**：宿主已先初始化 ORT 时 `init_runtime` 配置被忽略（幂等）。
- **模型加载失败不自动重试下载**：`ANYDOC_MODEL_DIR` 缺文件时该模型回退裸名下载；自备模型不能放 `$OAR_HOME`。
- **图片型是先渲染（或直提）成整页光栅再整页 OCR**，不做 unpaper 式全局去噪/去歪斜——靠 100dpi 分辨率 + 文档方向矫正保障精度（与 MinerU 同思路）。
- **印章识别只覆盖直排行**（默认开启，#10b）：章顶环排（弧形）公司名当前**显式跳过**——弧行摆正只会产出残缺字（实测"北京测试科技有限公司"→"时技有限"），残缺字比缺失更坏。弧行矫正方案的取证与下一步设计见 `BACKLOG.md` #5a。
- **印章默认开对离线包的影响**（#10b 行为变更）：tiny/small 离线包**不内置**印章检测模型（4.8MB，注册表资产）。含章文档在离线机上会触发一次 auto-download 尝试，失败只告警一次并跳过，主链路结果不受影响；确定不需要印章的环境可 `ANYDOC_NO_SEAL_OCR=1` 彻底关掉这次尝试。
- **后处理层（印章默认开 / `ANYDOC_TABLE_FILL`）不改 vendored 管线**：模型缺失或单框推理失败只告警一次并跳过该项填充，主链路结果原样返回；两者都进引擎缓存键（开关切换重建引擎，防串会话）。不含 `Seal` 版面元素的页在 `seal_pass` 早退，印章默认开对无章文档零额外推理。
- **安全闸（口径对齐 MinerU，超限显式报错、绝不静默截断/降质）**：输入 200 MiB（`ANYDOC_MAX_INPUT_BYTES`，stdin 有界读 + 文件入口预检）；单文档 1000 页（`ANYDOC_MAX_PAGES`，PDF 在 classify 元数据阶段拦、OFD 在逐页判定循环拦，均先于渲染/OCR）；`--dpi` 限 50–400；整页渲染长边 3500px（超则整体降 scale，`ANYDOC_RENDER_EDGE_CAP`）；单页原生文字 >65535 字符放弃文字层直判 OCR（`ANYDOC_NATIVE_TEXT_CHARS`）。
- **`--pages` 仅 PDF 通道**：每个 PDF 调度时多付一次 classify 元数据读取（~10–50ms，不渲图）用于页数闸与选页求值，输出不变。行内样式注入（`ANYDOC_RICH_TEXT`）默认关闭、仅作用于 PDF 文字层通路，开启后 OCR/网格表格通路不受影响。

## 许可

[MIT](LICENSE)