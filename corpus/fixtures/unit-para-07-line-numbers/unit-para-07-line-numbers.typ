// unit-para-07-line-numbers —— 左边距每 5 行一个行号
//
// 行号是独立的文本，插在字符流里；把带行号的稿件按 content stream 顺序读下来，
// 一段正文会被行号切成碎片。这一页把行号单独放在左边距的一列（x 25..43pt），
// 正文在 x 55..255pt，共 20 行。
//
// 行号的 dy 取 (n-1) x 9.92pt：9pt 字号、leading 4pt 下的行进实测为 9.92pt
// （四行块墨迹高 40.02pt 减三行块 30.10pt）。这是排版参数，不是期望值。

#set page(width: 280pt, height: 250pt, margin: 25pt)
#set text(size: 9pt, hyphenate: false)
#set par(justify: true, leading: 4pt)

#place(top + left, dx: 30pt, dy: 0pt, box(width: 200pt)[This paragraph runs for twenty lines so that a line numbering column has something to number. The numbers sit in the left margin, one every five lines, and they are not part of the sentence they stand beside. A pass that merges alternating line numbers into the body must remove them from the text it sends onward, and must put them back untouched on the way out. The trap in the original implementation is that its predicate returns true for empty text, so any empty paragraph is treated as a line number and merged away. Nothing on this page is empty, which is the point: this fixture isolates the numbering itself and introduces nothing else alongside it. Manuscripts that carry line numbers are common enough to matter: statutes, court filings, and papers circulated for review all use them, and in every one of those documents the numbers are apparatus rather than prose. They must survive the round trip in place, at the same coordinates, in the same size, and they must never reach a translation backend. That is the whole of what this page is for.])
#place(top + left, dx: 0pt, dy: 39.68pt, box(width: 18pt)[#align(right)[5]])
#place(top + left, dx: 0pt, dy: 89.28pt, box(width: 18pt)[#align(right)[10]])
#place(top + left, dx: 0pt, dy: 138.88pt, box(width: 18pt)[#align(right)[15]])
#place(top + left, dx: 0pt, dy: 188.48pt, box(width: 18pt)[#align(right)[20]])
