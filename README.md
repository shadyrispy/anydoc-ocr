# anydoc-ocr

Rust 2024 编写的 PDF/OFD → Markdown 转换 CLI 与库，含**图片型 PDF/OFD 的 OCR 回退**。OCR 基于 `oar-ocr`（ONNX Runtime，PaddleOCR 系模型，自动下载）。

## 功能特性

- **四类通道自动分流**（按文件魔数检测，见 `src/detect.rs`）：
  | 输入 | 处理方式 | 输出 |
  |------|----------|------|
  | 文字型 PDF | `pdf-inspector` 提取文本 + 自建阅读顺序还原；含表格页回退 OCR | 纯文本 GFM |
  | 图片型 PDF | PDFium 渲染 → OCR 管线 | 结构化 GFM |
  | 文字型 OFD | `ofd-core` 文本提取（按坐标排序）+ 表格网格重建 | 纯文本 GFM |
  | 图片型 OFD | `ofd-core` 渲染 → OCR 管线 | 结构化 GFM |
  | 其他格式（docx 等） | 回退 `anydoc::to_markdown` | Markdown |

- **OCR 管线**：版面检测（layout）→ 文本检测（det）→ 文本识别（rec）→ 表格结构重建（SLANet+）。模型档三档：
  - `tiny`（默认，极速）：PP-OCRv6 tiny（det 1.7MB / rec 4.3MB）+ PP-DocLayout-S
  - `small`（均衡）：PP-OCRv6 small（det 9.4MB / rec 20.2MB）+ PP-DocLayout-M
  - `medium`（高精度）：PP-OCRv6 medium（det 59MB / rec 73MB）+ PP-DocLayoutV3（复杂版式，ARM CPU 慢）

- **坏字体防护**：PDF/OFD 文字层检测到乱码（GID 坏字体、U+FFFD/私有区/控制字符占比 ≥20%）自动回退整页 OCR；PDF 用浅检 + 深检两级兜底。
- **页级健壮性**：坏页（装载失败等）跳过并告警，不整体失败。
- **线程模型**：`--threads` 控制页级并行（rayon）；进程级 ORT 全局线程池按 `intra = max(1, 核心数 / threads)` 提交，使总线程≈核心数，消除 rayon × ORT 的线程超额订阅（详见下文环境变量）。
- **渲染 DPI 可调**：默认 100；印刷体公文 100 相比 200 快 33% 且零精度损失（上海公报 52 页实测恢复率均 99.83%），80 起脚注/小字开始漏检。
- **模型缓存常驻**：`(tier, layout)` 为键缓存引擎，跨文档/跨调用复用；`OcrEngine::clear_cache()` 可释放。

## 构建

依赖预编译原生库（ORT 1.20.1、PDFium），需先放到 `third_party/` 下对应架构目录，再以环境变量指明位置。

### x86_64 本机构建

直接使用仓库内的脚本（等效于手动 export 下述变量后 `cargo build --release`）：

```bash
./scripts/build-x64.sh build --release
```

等价的手动写法：

```bash
export ORT_LIB_LOCATION=$PWD/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/lib
export ORT_INCLUDE_LOCATION=$PWD/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/include
export ORT_PREFER_DYNAMIC_LINK=1
export PDFIUM_LIB_DIR=$PWD/third_party/pdfium/x64/lib
cargo build --release
```

`ORT_PREFER_DYNAMIC_LINK=1` 必须：ort 2.0 rc 对静态链接校验严格，须用动态链接（运行时加载 `.so`）。

### 交叉编译到飞腾 aarch64

`.cargo/config.toml` 已为 `aarch64-unknown-linux-gnu` 配置编译期优化：

```toml
[target.aarch64-unknown-linux-gnu]
rustflags = [
  "-C", "target-cpu=cortex-a72",
  "-C", "target-feature=+neon",
  "-C", "link-arg=-Wl,-rpath,$ORIGIN/lib",
]
```

- 飞腾 D2000（FTC663）≈ Cortex-A72 微架构，`cortex-a72` 改善调度/指令选择。
- 飞腾是 **ARMv8.0-A**，NEON 默认开启；**勿加 `+dotprod`/`+fp16`**（v8.2 才支持，会崩/错码）。
- rpath（`$ORIGIN/lib`）一并放 config：若改由 `RUSTFLAGS` 环境变量传会**整体覆盖** config 的 rustflags（实测确认），导致 `cortex-a72+neon` 静默丢失。构建脚本不设 `RUSTFLAGS`。

完整交叉构建 + 离线打包走 `scripts/build-aarch64.sh`（含链接器、rpath、打包逻辑）：

```bash
./scripts/build-aarch64.sh
```

（rpath 已并入 `.cargo/config.toml`，CPU 调优与 rpath 同时生效，不再有 RUSTFLAGS 覆盖问题。）

### 运行环境

```bash
export OAR_HOME=~/.oar          # 模型下载/缓存根目录（首次 OCR 自动从 ModelScope 下载）
export LD_LIBRARY_PATH="$ORT_LIB_LOCATION:$PDFIUM_LIB_DIR"   # 仅开发期需要；打包产物自带 rpath
```

OFD 中文渲染需要 CJK 字体，运行 `scripts/install-font.sh`（安装 `fonts/NotoSansCJK-Regular.ttc`，fontdb 直接扫描字体目录、无需 fc-cache 亦生效）。

## 使用（CLI）

```bash
anydoc-ocr <input> [选项]
```

- `<input>`：输入文件；`-` 表示 stdin（图片型 PDF/OFD 会先落临时文件）。
- 省略 `-o` 时输出写 stdout。

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `-o, --output <path>` | stdout | 输出文件 |
| `--ocr-tier <tiny\|small\|medium>` | `tiny` | OCR 模型档 |
| `--ocr-layout <doc\|table>` | `doc` | 版面模型：`doc` 默认文档结构 / `table` 表格专用（检出 Table 才跑 SLANet，无表页零开销） |
| `--threads <n>` | `0` | OCR 推理线程数（页级并行）。`0` = 自动取 `available_parallelism`（飞腾 D2000 8 核→8），配合进程级 ORT `intra=1` 全核利用；内存受限环境（cgroup<8GB）显式调小 |
| `--dpi <f32>` | `100.0` | 图片型走 OCR 时的渲染分辨率 |
| `--pdf-force-ocr` | 关闭 | 文字型 PDF 当图片渲染后 OCR（图片型校准用） |
| `--ofd-force-ocr` | 关闭 | OFD 强制走 OCR（重建表格结构） |

示例：

```bash
anydoc-ocr 公文.pdf                 # 自动分流，输出到 stdout
anydoc-ocr 公文.ofd -o out.md       # OFD → out.md
anydoc-ocr 扫描件.pdf --ocr-tier small --threads 4 --dpi 100
anydoc-ocr 公文.ofd --ofd-force-ocr # 文字型 OFD 也强制 OCR，重建表格
cat 公文.pdf | anydoc-ocr -         # stdin 输入
```

## 库用法

核心 API：`convert_to_markdown(path, &ConvertOptions) -> Result<String>`，外加 `OcrEngine` 单例（`build`/`predict`/`clear_cache`）、`ocr_images`/`ocr_pdf_pages` 便捷函数、`DocKind`、`OcrTier`。

```rust
use anydoc_ocr::{convert_to_markdown, ConvertOptions, OcrTier, OcrLayout};

let opts = ConvertOptions {
    ocr_tier: OcrTier::Small,
    ocr_layout: OcrLayout::Doc,
    dpi: 100.0,      // 必须显式设置：Default 的 dpi=0 会使 OCR 渲染失效
    threads: 4,
    ..Default::default()
};
let md = convert_to_markdown(std::path::Path::new("公文.ofd"), &opts)?;
```

注意：库模式 `OcrEngine::predict` 页序契约被破坏时返回 `Err` 而非 panic，宿主进程不会被打翻。

## 环境变量

| 变量 | 说明 |
|------|------|
| `OAR_HOME` | oar-ocr 模型缓存/下载根目录（首次使用自动从 ModelScope 下载，sha256 匹配则复用） |
| `ANYDOC_MODEL_DIR` | 本地 ONNX 模型目录（绝对路径）。设置后从该目录加载模型，**不走 `$OAR_HOME` 缓存/下载**，用于离线/内网部署。目录内缺某模型时回退裸名走正常下载。注意：**不能**把自备模型放进 `$OAR_HOME` 用裸名加载——会命中缓存目录分支，size/hash 不符即被静默重下载覆盖 |
| `ANYDOC_ORT_INTRA_THREADS` | 强制覆盖 ORT 全局线程池 intra-op 线程数（调试用）。默认 `max(1, 核心数 / 页级并行度)`，令总线程≈核心数。必须发生在任何 ONNX session 创建前（`init_runtime` 在引擎构建前调用），否则被 ORT 忽略（幂等） |
| `ANYDOC_TIMINGS` | 存在即输出分阶段计时（render/ocr/gfm…）到 stderr |
| `ANYDOC_DEBUG_GFM` | 存在即启用 GFM 适配器调试输出 |
| `ORT_LIB_LOCATION` / `ORT_INCLUDE_LOCATION` | ORT 预编译库路径（构建期） |
| `ORT_PREFER_DYNAMIC_LINK` | 置 `1` 走 ORT 动态链接（构建期，必须） |
| `PDFIUM_LIB_DIR` | PDFium 库路径（构建期与运行期） |
| `ANYDOC_GOLDEN_OCR` / `ANYDOC_GOLDEN_UPDATE` | golden 测试开关，见下节 |

## 测试

- **Golden 回归**（`tests/golden.rs`）：对 22 个样本（`tests/samples` 生成样本 + `tests/real_samples` 真实公文）跑 `convert_to_markdown`，输出 SHA-256 与 `tests/golden/snapshots/*.sha256` 比对，保护重构行为不变。
  ```bash
  cargo test --test golden                          # 非 OCR 样本（不触发模型下载）
  ANYDOC_GOLDEN_OCR=1 cargo test --test golden      # 追加 OCR 样本（image.* 与真实样本）
  ANYDOC_GOLDEN_UPDATE=1 cargo test --test golden   # 重生成快照基线（仅行为变更 ticket 才用）
  ```
  真实样本（`tests/real_samples/`）被 gitignore，CI 缺文件自动跳过；样本存在但快照缺失时强制显式 `UPDATE=1` 建基线。
- 校准与 DPI 扫描：`tests/bench.sh`、`tests/calibrate.py`（字符集内容恢复率）、`tests/dpi_sweep.sh`。

## 目录结构

```
src/
  main.rs           CLI 入口（clap 参数）
  lib.rs            库入口，重导出 convert_to_markdown / ConvertOptions / OcrTier 等
  convert.rs        格式分流（detect → pdf/ofd/anydoc）
  detect.rs         PDF/OFD/Other 魔数检测
  pdf/              文字层提取 + 阅读顺序、渲染（render.rs）、OCR 回退
  ofd/              ofd-core 提取 + 表格重建 + OCR 回退
  ocr_engine.rs     OcrEngine 单例缓存、进程级 ORT 线程池（init_runtime）、ANYDOC_MODEL_DIR
  models.rs         OcrTier/OcrLayout 定义与模型规格
  gfm_adapter.rs    OCR StructureResult → GFM
  reading_order.rs  阅读顺序还原（PDF 文字层与 OCR 通路共用）
  table_grid.rs     文字层表格网格重建 + 跨页合并
  emitter.rs / region.rs / text_health.rs / timing.rs / error.rs
tests/
  golden.rs + golden/snapshots/   22 个 SHA-256 快照
  samples/         生成的确定性子样本
  real_samples/    gitignored 真实公文样本
scripts/            build-x64 / build-aarch64 / package-aarch64 / install-font
.cargo/config.toml aarch64 交叉编译 rustflags
```

## 已知限制

- **DPI ≤80** 起脚注/小字开始漏检（印刷体公文 100 为甜点：快 33% 且零精度损失）。
- **`medium` 档在 ARM CPU 上慢**（det 59MB / rec 73MB），慎用于大批量。
- **`ConvertOptions` 的 `Default` 实现 `dpi=0`**，库调用方必须显式设 `dpi`，否则 OCR 渲染失效（golden 测试内同样显式设 100）。
- **ORT 全局线程池仅首次生效**：若宿主已先初始化 ORT，`init_runtime` 的配置被忽略（幂等）；配置失败时告警并回落 ORT 默认线程池（可能线程超额订阅）。
- **模型加载失败不自动重试下载**：`ANYDOC_MODEL_DIR` 下缺文件时该模型回退裸名下载；但自备模型不能放 `$OAR_HOME`（见环境变量表）。
- **OFD 中文渲染依赖 CJK 字体**：目标机需安装 `fonts/NotoSansCJK-Regular.ttc`（`scripts/install-font.sh`）。

## 许可

[MIT](LICENSE)
