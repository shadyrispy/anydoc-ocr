# anydoc-ocr

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/rust-1.95%2B-orange.svg)
![platform](https://img.shields.io/badge/platform-Linux%20x86__64%2Faarch64-lightgrey.svg)

把 PDF / OFD（含图片型、扫描件）转成干净 GitHub-Flavored Markdown 的 Rust CLI 与库。文字型从版式还原阅读顺序，图片型自动走 OCR，表格结构化重建——一种输入、一致输出，面向办公公文与资料归档场景。

## Highlights

- **四类通道自动分流**：按文件魔数识别 PDF/OFD 的文字型 vs 图片型，各走最优路径（细则见下节）。
- **图片型 OCR 回退**：扫描件/图片型 PDF、OFD 自动渲染后跑「版面检测 → 文本检测 → 文本识别 → 表格结构识别」管线，模型档三档可按精度/速度切换。
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

依赖预编译原生库（ONNX Runtime 1.20.1、PDFium）与 Rust ≥ 1.95，先放库再构建：

```bash
# 1) 把预编译库放到 third_party/ 对应架构目录（见「构建」节）

# 2) x86_64 本机构建（脚本内含全部环境变量）
./scripts/build-x64.sh build --release

# 3) 首次 OCR 会从 ModelScope 自动下载模型
export OAR_HOME=~/.oar

# 4) 转换
target/release/anydoc-ocr 公文.ofd -o out.md
target/release/anydoc-ocr 扫描件.pdf --ocr-tier small --threads 4
```

## 使用（CLI）

```text
anydoc-ocr <输入文件或目录> [选项]
```

- `<input>`：`-` 表示 stdin（图片型会先落临时文件）；目录则递归批量转所有受支持文档。
- 单文件省略 `-o` 写 stdout；目录输入必须 `-o` 指定输出目录（保持相对结构，`.pdf/.ofd` → `.md`）。

| 参数 | 默认 | 含义 |
|------|------|------|
| `-o, --output <path>` | stdout | 输出文件；目录输入时为输出目录 |
| `--ocr-tier <tiny\|small\|medium>` | `tiny` | OCR 模型档（见「模型档与精度」） |
| `--ocr-layout <doc\|table>` | `doc` | 版面模型：`doc` 默认文档结构 / `table` 表格专用（检出 Table 才跑 SLANet，无表页零额外开销） |
| `--threads <n>` | `0` | OCR 推理页级并行度。`0` = 自动取可用并行度；进程级 ORT `intra=max(1,核心数/n)`，总线程≈核心数。内存受限环境（cgroup<8GB）可调小 |
| `--dpi <f32>` | `100` | 图片型渲染分辨率。印刷体公文 `100` 零精度损失且比 `200` 快 33%；`80` 起脚注/小字开始漏检 |
| `--quality-route <auto\|off>` | `off` | 质量路由：`auto` 用 tiny 首跑首页 OCR，平均置信低于阈值则升级 `small` 全篇重跑（污染件更稳）；`off` 用显式 `--ocr-tier` |
| `--ofd-force-ocr` | off | 文字型 OFD 也强制走 OCR（重建表格结构） |
| `--pdf-force-ocr` | off | 文字型 PDF 当图片渲染后 OCR（图片型校准用） |

示例：

```bash
anydoc-ocr 公文.pdf                       # 自动分流，写 stdout
anydoc-ocr 扫描件.pdf --ocr-tier small --threads 4 --dpi 100
anydoc-ocr 公文.ofd --ofd-force-ocr       # 强制 OCR，重建表格
cat 公文.pdf | anydoc-ocr - -o out.md     # stdin
anydoc-ocr 资料目录/ -o out_md/           # 目录批处理
anydoc-ocr /tmp/doc.pdf --quality-route auto
```

## 模型档与精度

三档模型，`--ocr-tier` 切换，无需重编译。全部模型从 ModelScope 自动下载，按 `$OAR_HOME` 缓存（sha256 匹配则复用）。

| 档 | 版面 | 文本检测（det） | 识别（rec） | 适用 |
|----|------|----------------|------------|------|
| `tiny`（默认） | PP-DocLayout-S | PP-OCRv6 tiny 1.7MB | 4.3MB | 常见中文公文，极速 |
| `small` | PP-DocLayout-M | PP-OCRv6 small 9.4MB | 20.2MB | 中文覆盖最全，均衡 |
| `medium` | PP-DocLayoutV3 | PP-OCRv6 medium 59MB | 73MB | 复杂版式高精度（**ARM CPU 较慢**，慎批量） |

各档通用：表格结构 `slanet_plus` + `pp-lcnet` 分类 + 中文表格词典；文档方向矫正 `pp-lcnet doc_ori`（0°/90°/180°/270° 自动转正）。

## 处理速度

性能受机型（CPU 核数/指令集）、`--threads`、`--dpi` 与模型档影响；以下为本项目实测参考（Linux x86_64，静态编译）：

| 场景 | 配置 | 耗时 |
|------|------|------|
| 24 页图片型 PDF（扫描公文） | `tiny` / 3 线程 / 100dpi | **≈ 18s**（≈ 0.75s/页，含渲染+OCR） |
| 印刷体公文 DPI 对比 | 52 页，100 vs 200 | 恢复率均 99.83%，100 比 200 快 **33%** |

要点：分辨率比全局去噪更能保精度——`100dpi` 是印刷体甜点（快且零损失），DPI≤80 起小字/脚注开始漏检；线程模型把页级并行与进程级 ORT 池配平到总线程≈核心数，多核利用率高、不超额订阅。

CLI 加 `ANYDOC_TIMINGS=1` 可在 stderr 输出分阶段计时（render/ocr/gfm...）。

## 模型缓存与内存

- 模型按 `(tier, layout)` 为键常驻缓存，跨文档/跨调用复用；库模式 `OcrEngine::clear_cache()` 可释放。
- 需更多档位/更大吞吐时，实测 `small` 行批 4/8/16/32 与 `tiny` 16/32/64 全部持平（intra 满核后批大小只改矩阵形状不改 FLOPs），故行批旋钮仅留给飞腾架机构复核，无默认超配。

## 库用法

核心 API：`convert_to_markdown(path, &ConvertRequest, &ForceFlags) -> Result<String>`，外加 `OcrEngine` 单例（`build`/`predict`/`clear_cache`）、`batch::BatchConverter`、类型 `DocKind`/`OcrTier`/`OcrLayout`/`QualityRoute`/`ConvertError`。

```rust
use anydoc_ocr::{convert_to_markdown, ConvertRequest, ForceFlags, OcrTier};

let opts = ConvertRequest {
    render: RenderConfig { dpi: 100.0 },
    ocr: OcrConfig { tier: OcrTier::Small, layout: OcrLayout::Doc },
    parallel: ParallelConfig { page_parallel: 4, ort_intra: 0 },
    quality_route: QualityRoute::Off,
};
let force = ForceFlags::default();
let md = convert_to_markdown(std::path::Path::new("公文.ofd"), &opts, force)?;
```

> 库模式 `OcrEngine::predict` 页序契约被破坏时返回 `Err` 而非 panic，宿主进程不会被打翻。

## 环境变量

| 变量 | 说明 |
|------|------|
| `OAR_HOME` | oar-ocr 模型缓存/下载根目录（首用自动从 ModelScope 下载） |
| `ANYDOC_MODEL_DIR` | 本地 ONNX 模型目录（绝对路径）。设置后从该目录**直载**，不走 `$OAR_HOME` 缓存/下载，用于离线/内网；缺某模型回退裸名下载。注意不能把自备模型放 `$OAR_HOME` 用裸名（会命中缓存分支被 size/hash 不符静默重下覆盖） |
| `ANYDOC_ORT_INTRA_THREADS` | 强制覆盖进程级 ORT intra-op 线程数（调试用）。必须在任何 ONNX session 创建前生效 |
| `ANYDOC_REC_BATCH` | 覆盖 rec 行批大小（上游默认 tiny=16 / small+medium=4；默认不启用） |
| `ANYDOC_TIMINGS` | 存在即输出分阶段计时到 stderr |
| `ANYDOC_DEBUG_GFM` | 存在即启用 GFM 适配器调试输出 |
| `ORT_LIB_LOCATION` / `ORT_INCLUDE_LOCATION` | ORT 预编译库路径（构建期） |
| `ORT_PREFER_DYNAMIC_LINK` | 置 `1` 走 ORT 动态链接（构建期，**必须**：ort 2.0 rc 对静态校验严格） |
| `PDFIUM_LIB_DIR` | PDFium 库路径（构建期与运行期） |
| `ANYDOC_GOLDEN_OCR` / `ANYDOC_GOLDEN_UPDATE` | golden 测试开关（见「测试」） |

## 构建

依赖预编译原生库（ORT 1.20.1、PDFium），先放到 `third_party/` 对应架构目录，再用环境变量指明位置。

### x86_64 本机构建

```bash
./scripts/build-x64.sh build --release
```

等效手动方式：

```bash
export ORT_LIB_LOCATION=$PWD/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/lib
export ORT_INCLUDE_LOCATION=$PWD/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/include
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

`./scripts/package-single.sh [auto|aarch64|x86_64] [tiny|small]` 产出 1 个自解压安装器，目标机自动检测架构、首次运行一键部署（装 CJK 字体 + `~/.local/bin/anydoc` 命令）。第二参数选内嵌模型档：`tiny`（默认，~78M）极简版 / `small`（~114M）正常版——small 包的启动器会在未显式传 `--ocr-tier` 时自动默认 `small`，开箱即用对应档位。medium 不内嵌，用 `ANYDOC_MODEL_DIR` 外置。

CI 已自动化：`.github/workflows/release.yml` 在 push `v*` tag 时构建 Linux x86_64/aarch64 × tiny/small 共 4 个 `.run` 并发布到 GitHub Release（手动触发则进 artifact）。

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
third_party/
  oar-ocr-core/      vendored（升级时 rebase；含 SIMD resize 加速本地 patch）
tests/               golden.rs + golden/snapshots + samples/
scripts/             build-x64 / build-aarch64 / package-single / install-font
.cargo/config.toml   aarch64 交叉编译 rustflags（cortex-a72+neon + rpath）
```

## 依赖

| 包 | 版本 | 用途 |
|----|------|------|
| `oar-ocr` | 0.9.2（锁定） | 版面/OCR/表格结构推理（ONNX Runtime，PaddleOCR 系模型） |
| `anydoc` | 0.2.3（锁定） | 其他格式兜底（docx 等） |
| `ort` | 2.0.0-rc.13（锁定） | 进程级 ORT 线程池 API（与 oar-ocr-core 同版镜像） |
| `ofd-core` | 0.3.0 | OFD 文本提取与渲染 |
| `pdf-inspector` | 1.14 | 文字型 PDF 文本提取 |
| `pdfium-render` | 0.9.3 | PDFium 渲染 |

> 版本策略：深耦合/行为镜像/RC/0.x 演进期锁 `=`，纯 Rust 工具库用 `^`。`oar-ocr-core` 为本仓 vendored（`[patch.crates-io]`），内含 NEON SIMD 的 resize 加速 patch，升级时需 rebase。

## 已知限制

- **DPI ≤80 起漏检**：脚注/小字开始丢；印刷体公文 `100` 为甜点（快 33% 且零损失）。
- **`medium` 档在 ARM CPU 上慢**（det 59MB / rec 73MB），慎批量。
- **ORT 全局线程池仅首次生效**：宿主已先初始化 ORT 时 `init_runtime` 配置被忽略（幂等）。
- **模型加载失败不自动重试下载**：`ANYDOC_MODEL_DIR` 缺文件时该模型回退裸名下载；自备模型不能放 `$OAR_HOME`。
- **图片型是先渲染（或直提）成整页光栅再整页 OCR**，不做 unpaper 式全局去噪/去歪斜——靠 100dpi 分辨率 + 文档方向矫正保障精度（与 MinerU 同思路）。

## 许可

[MIT](LICENSE)