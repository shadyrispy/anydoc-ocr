#!/usr/bin/env python3
# 文字型→图片型 校准：以文字型解析结果为 ground truth，
# 用 --pdf-force-ocr / --ofd-force-ocr 渲染后的 OCR 结果为 hypothesis，
# 用于模型档位优选。
#
# 提供两类指标：
#  1) 序列指标（CER / recall / prec / F1）：对版面/阅读顺序敏感，
#     文字型提取与“渲染后 OCR”在目录点线、页码锚点、竖排上天然错位，
#     故该指标仅作参考，不能直接代表 OCR 质量。
#  2) 内容覆盖率（content-coverage）：顺序无关的内容短语召回，
#     才是校准/模型优选的恰当信号——衡量“OCR 是否把正文内容捞回来了”。
import re, sys, difflib

CORE_PUNCT = set('，。、；：？！（）《》〈〉"\'’‘"”·—…%￥$@#&*+=.,;:!?()[]{}<>/-')
CJK = re.compile(r'[\u4e00-\u9fff\u3400-\u4dbf]+')

def full_to_half(s: str) -> str:
    out = []
    for ch in s:
        o = ord(ch)
        if 0xFF01 <= o <= 0xFF5E:
            out.append(chr(o - 0xFEE0))
        elif ch == '\u3000':
            out.append(' ')
        else:
            out.append(ch)
    return ''.join(out)

def normalize(text: str) -> str:
    text = full_to_half(text)
    text = re.sub(r'https?://\S+', ' ', text)
    text = re.sub(r'\[([^\]]*)\]\([^)]*\)', r'\1', text)
    text = re.sub(r'[#>*`|_~]', ' ', text)
    out = []
    for ch in text:
        o = ord(ch)
        if (0x4E00 <= o <= 0x9FFF) or (0x3400 <= o <= 0x4DBF):
            out.append(ch)
        elif ch.isalnum():
            out.append(ch.lower())
        elif ch in CORE_PUNCT:
            out.append(ch)
    return ''.join(out)

def seq_metrics(gt: str, hyp: str) -> dict:
    gt, hyp = normalize(gt), normalize(hyp)
    n = max(1, len(gt))
    sm = difflib.SequenceMatcher(None, gt, hyp)
    sub = dele = ins = 0
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        l1, l2 = i2 - i1, j2 - j1
        if tag == 'equal':
            continue
        elif tag == 'replace':
            sub += min(l1, l2)
            dele += max(0, l1 - l2)
            ins += max(0, l2 - l1)
        elif tag == 'delete':
            dele += l1
        elif tag == 'insert':
            ins += l2
    dist = sub + dele + ins
    matched = n - dele - sub
    recall = matched / n
    precision = matched / max(1, len(hyp))
    f1 = 2 * recall * precision / max(1e-9, recall + precision)
    return {"gt": len(gt), "hyp": len(hyp), "cer": dist / n,
            "recall": recall, "precision": precision, "f1": f1, "ratio": sm.ratio()}

def content_terms(s: str, minlen: int = 4) -> set:
    return set(r for r in CJK.findall(normalize(s)) if len(r) >= minlen)

def content_metrics(gt: str, hyp: str, minlen: int = 4) -> dict:
    gt_terms, hyp_terms = content_terms(gt, minlen), content_terms(hyp, minlen)
    inter = gt_terms & hyp_terms
    recall = len(inter) / max(1, len(gt_terms))
    precision = len(inter) / max(1, len(hyp_terms))
    f1 = 2 * recall * precision / max(1e-9, recall + precision)
    return {"gt_terms": len(gt_terms), "hyp_terms": len(hyp_terms),
            "recall": recall, "precision": precision, "f1": f1}

def charset_recall(gt: str, hyp: str) -> float:
    """顺序无关的字符级内容恢复率：GT 字符集合有多少被 HYP 覆盖。
    对版面/阅读顺序不敏感，是校准 OCR 内容恢复能力的恰当信号。"""
    g, h = set(normalize(gt)), set(normalize(hyp))
    return len(g & h) / max(1, len(g))

def main():
    if len(sys.argv) < 3:
        print("usage: calibrate.py <gt.md> <hyp.md> [label]", file=sys.stderr)
        sys.exit(1)
    gt = open(sys.argv[1], encoding="utf-8").read()
    hyp = open(sys.argv[2], encoding="utf-8").read()
    label = sys.argv[3] if len(sys.argv) > 3 else sys.argv[2]
    s = seq_metrics(gt, hyp)
    c = content_metrics(gt, hyp)
    chr_recall = charset_recall(gt, hyp)
    print(f"{label}\t"
          f"[内容恢复率·字符集] {chr_recall*100:5.2f}%\t"
          f"[序列·参考] CER={s['cer']*100:5.2f}% F1={s['f1']*100:5.2f}%\t"
          f"[短语覆盖 minlen=4·参考] 召回={c['recall']*100:5.2f}% "
          f"(GT短语={c['gt_terms']} HYP短语={c['hyp_terms']})")

if __name__ == "__main__":
    main()
