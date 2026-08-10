# anydoc-ocr

办公文档转 Markdown，含**图片型 PDF/OFD 的 OCR 回退**。基于 `anydoc`、`oar-ocr`（PaddleOCR 系）、`ofd-core`、`pdfium-render`。

支持四类通道，真实政府公文实测：

| 通道 | 处理方式 | 输出 |
|------|----------|------|
| 文字型 PDF | 字节级文本提取 | 纯文本 GFM |
| 图片型 PDF | PDFium 渲染 → OCR 管线 | 结构化 GFM |
| 文字型 OFD | ofd-core 文本提取（按坐标排序） | 纯文本 GFM |
| 图片型 OFD | ofd-core 渲染 → OCR 管线 | 结构化 GFM |

## 特性

- **OCR 管线**：版面检测(layout) → 文本检测(det) → 文本识别(rec/CRNN) → 表格结构重建(SLANet)
- **模型三档**：`tiny`（极速，默认）/ `small`（均衡）/ `medium`（高精度），ModelScope 自动下载
- **渲染 DPI 可调**：默认 100（印刷体公文零精度损失，比 200 快 33%），`--dpi` 覆盖
- **线程可调**：`--threads` 默认 0 = 自动（按可用核心数决定页级并行度），大文档实测快 ~10%；内存受限环境用 1
- **四类通道字节级/结构化双路径**，页级异常跳过不整体失败

## 构建

依赖预编译库（x64）：

```bash
export ORT_LIB_LOCATION=$PWD/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/lib
export ORT_INCLUDE_LOCATION=$PWD/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/include
export ORT_PREFER_DYNAMIC_LINK=1
export PDFIUM_LIB_DIR=$PWD/third_party/pdfium/x64/lib
cargo build --release
```

运行前：

```bash
export LD_LIBRARY_PATH="$ORT_LIB_LOCATION:$PDFIUM_LIB_DIR"
export OAR_HOME=~/.oar
```

## 使用

```bash
# 文字型/图片型自动识别
anydoc-ocr 公文.pdf
anydoc-ocr 公文.ofd -o out.md

# 图片型强制走 OCR（重建表格结构）
anydoc-ocr 公文.ofd --ofd-force-ocr
anydoc-ocr 公文.pdf --pdf-force-ocr   # 文字型当图片渲染 OCR，用于校准

# 模型/参数（--threads 默认 0 = 自动，按核心数定页级并行度）
anydoc-ocr 扫描件.pdf --ocr-tier small --threads 4 --dpi 100
```

stdin 支持：`cat 公文.pdf | anydoc-ocr -`

## 环境变量

| 变量 | 说明 |
|------|------|
| `OAR_HOME` | 模型缓存/下载根目录 |
| `ANYDOC_MODEL_DIR` | 本地 ONNX 模型目录（绝对路径）。设置后从该目录加载模型文件，不走 `$OAR_HOME` 缓存/下载；用于离线/内网部署 |
| `ANYDOC_ORT_INTRA_THREADS` | 强制覆盖 ONNX Runtime 全局线程池的 intra-op 线程数（调试用）。默认自动取 `max(1, 可用核心数 / 页级并行度)`，消除 rayon 页级并行 × ORT intra 线程的超额订阅（8 核机器上默认可达 4×8=32 线程） |

## 性能参考

上海公报 52 页（threads=1，单进程，8GB cgroup）：

| DPI | WALL | 内容恢复率 |
|-----|------|-----------|
| 200 | 148.5s | 99.83% |
| 150 | 122.4s | 99.83% |
| 100 | 100.0s | 99.83% |
| 80  | 92.9s | 99.59% |

DPI 100 为甜点：快 33% 且零精度损失；80 起小字脚注开始漏检。

## 测试

```bash
# 校准：文字型 GT vs 图片型 OCR（字符集内容恢复率）
bash tests/bench.sh
python3 tests/calibrate.py <gt.md> <hyp.md>
# DPI 扫描
bash tests/dpi_sweep.sh <pdf> [dpi...]
```

## 许可

[MIT](LICENSE)

## 致谢

- [firecrawl/anydoc](https://github.com/firecrawl/anydoc)
- [GreatV/oar-ocr](https://github.com/GreatV/oar-ocr)（PaddleOCR ONNX）
- [ofd-core](https://crates.io/crates/ofd-core)
- [pdfium-render](https://crates.io/crates/pdfium-render)
