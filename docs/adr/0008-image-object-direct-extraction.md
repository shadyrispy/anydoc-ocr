# ADR-0008：图片型 PDF/OFD 图像对象直提（替代整页渲染）

- 状态：拟议
- 日期：2026-08-15
- 决策者：anydoc-ocr 维护者
- 上下文：ADR-0005（批处理/跨文档流水线）、ADR-0006（错误处理统一）、ADR-0007（质量路由）之后，实测 nuaa4.pdf（大幅面扫描件）发现整页渲染存在根本性架构问题。

## 背景

nuaa4.pdf 实测发现：

| 维度 | 值 |
|------|-----|
| 物理尺寸 | A4（210×297mm） |
| 扫描分辨率 | ~460dpi |
| MediaBox | 3600×2540 pt（**异常**：扫描仪把图像像素值当 pt 写入） |
| 图像像素 | 3609×2540 |
| 80dpi 整页渲染 | 4000×2822 px/页 × 10 页并发 → OOM（cgroup 4GB） |
| 60dpi 整页渲染 | 3000×2122 px/页，单线程勉强通过 |

**根因**：图片型 PDF/OFD 每页本质上就是一张扫描图像（嵌入为 image object）。当前 [render.rs](../../src/pdf/render.rs) 用 `page.render(w, h, None)` **整页光栅化**——把已解码的图像再渲染一次。对图片型文档，这是"重复光栅化"：

1. pdfium 先解压 image object 得到原始像素（3609×2540）
2. 再按 `MediaBox × DPI/72` 创建画布（80dpi → 4000×2822）
3. 把图像绘制到画布上
4. 输出画布

步骤 2-4 是冗余的——原始像素已经在步骤 1 拿到了。MediaBox 异常时，步骤 2 还会放大像素导致 OOM。

## 决策

**图片型 PDF/OFD 直接提取 image object 的原始像素，跳过整页渲染。**

### PDF 路径

pdfium-render 提供 `PdfPageImageObject::image_data() -> PdfBitmap`（context7 确认）：

```rust
for obj in page.objects() {
    if let Some(img_obj) = obj.as_image_object() {
        let bitmap = img_obj.image_data();  // 原始像素，非渲染
        let img = bitmap.as_image()?.to_rgb8();
    }
}
```

### OFD 路径

ofd-core 的 `ImageObject.resource_id` → `Res.multi_medias()` 找 `CtMultiMedia` → `media_file` → `package.read()` 拿原始字节 → `image::load_from_memory` 解码。路径解析（参考 ofd-core render.rs `load_res_into`）：

```rust
let res_path = resolve_path(&doc.base, &res_loc);
let res_dir = parent_dir(&res_path);
let data_base = resolve_path(&res_dir, &res.base_loc);
let media_path = resolve_path(&data_base, &mm.media_file);
let bytes = reader.package_mut().read(&media_path)?;
let img = image::load_from_memory(&bytes)?.to_rgb8();
```

### 降级条件

直提仅适用于"单图满页"（图片型扫描件的典型形态）。以下情况回退整页渲染：

| 条件 | 原因 |
|------|------|
| PDF 页含多个 image object | 多图块拼接，直提会丢失页面整体性 |
| PDF 页含 Path/Shading 等可见矢量 object | 真正的混合页，需渲染合成 |
| OFD 页含多个 ImageObject | 同上 |
| OFD 页含 TextObject/PathObject | 混合页 |
| 直提失败（解码错误等） | 容错回退 |

**关于隐藏文字层**：图片型扫描件常带 OCR 生成的不可见文字层（大量 Text object，
如 nuaa4 每页 141~964 个）。这些 Text object 不可见，不影响图像内容——**允许
"1 image + 仅 Text object"直提**，仅当存在 Path/Shading 等可见矢量对象时才回退。
否则 nuaa4 这类带隐藏文字层的图片型 PDF 无法直提，MediaBox 异常（2539×3609pt）
下渲染 3526×5012px/页 × 19 页 → OOM。

### 降采样

扫描件原始像素常远超 OCR 所需（460dpi 扫描 → 3609×2540），多页并发 OCR 会撑爆
cgroup 内存（4GB 限制下 nuaa 37 页、nuaa4 19 页均 OOM）。

直提后按 `dpi` 对应的 A4 长边（~12in × dpi）限制最长边，超过则 Lanczos 降采样：
- 100dpi → 最长边 1200px（nuaa4 3609×2540 → 844×1200）
- 低于上限原样返回（nuaa 150dpi 扫描约 1240×1754，100dpi 下不降采样）

这与渲染路径的 DPI 语义一致，且对齐 PP-OCR 训练分布（ADR-0007 实测 100dpi 最优、
200dpi 反退化）。`downscale_to_dpi` 函数 PDF/OFD 共用（`pdf::render` 模块 pub(crate)）。

### DPI 语义

直提路径的 `--dpi` 用于**降采样上限**（最长边 ≤ 12×dpi px），而非 MediaBox×DPI/72 的渲染缩放。
渲染路径（混合页降级）的 DPI 语义不变。

## 影响

### 性能（实测，tiny/100dpi，4GB cgroup/3 核）

| 样本 | 整页渲染 | 直提+降采样 |
|------|---------|------------|
| nuaa4.pdf (19p, 460dpi, 带隐藏文字层) | OOM（MediaBox 2539×3609pt 异常） | 85s，无 OOM |
| nuaa.pdf (37p, 150dpi) | 132s | 93s（-29%） |
| nuaa3.pdf (1p, 150dpi) | 76s | 46s（-39%） |

### 内存

- 整页渲染：峰值 = N × 渲染画布（MediaBox×DPI/72 决定，MediaBox 异常时放大 → OOM）
- 直提+降采样：峰值 = ~2 × 降采样后图像（channel 背压限制），最长边 ≤ 12×dpi px

nuaa4 单页：渲染 3526×5012×3=53MB × 19 页 → OOM；
直提降采样后 844×1200×3=3MB × 2（背压）→ 6MB。

### 与 ADR-0007 的关系

ADR-0007 的 MediaBox 归一化前置步骤**取消**——直提路径不依赖 MediaBox。质量路由改为：
1. text_layer 检测（TEXT 档）
2. 无文字层 → 直提 image object → 看像素尺寸 + Laplacian 方差 → HIGH/MEDIUM/LOW 档

## 实施步骤

1. **PDF render.rs**：新增 `extract_page_image(doc, page_idx) -> Option<RgbImage>`，在 `render_cross_doc_fn` 内优先调用，失败回退 `page.render`
2. **OFD mod.rs**：新增 `extract_ofd_page_image(reader, doc, page_idx) -> Option<RgbImage>`，在 render_fn 闭包内优先调用，失败回退 `render_page_to_image`
3. **测试**：nuaa/nuaa3/nuaa4 三个图片型样本验证不 OOM + 质量不退化
4. **golden 测试**：直提改变像素来源（原始 vs 渲染），golden 快照需重新基线

## 风险

- **pdfium `image_data()` 返回格式**：可能含 alpha 通道或非 RGB 颜色空间，需 `to_rgb8()` 转换
- **OFD media 格式**：OFD 支持多种图像格式（JPEG/PNG/TIFF/BMP），`image::load_from_memory` 需支持。已依赖 `image` crate 默认开启常见格式
- **多图块页**：某些扫描件把一页拆成多个 image object（如四象限），直提会丢失整体性。降级条件已覆盖
- **golden 重基线**：直提与渲染的像素可能有细微差异（颜色空间转换），golden 快照需更新
