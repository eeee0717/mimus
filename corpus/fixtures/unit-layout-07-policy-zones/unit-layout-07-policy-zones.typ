// unit-layout-07-policy-zones —— 一页上的五个政策区
//
// 页眉、页脚、参考条目、印章各自都是**不该被翻译**的区域，两段正文是唯一
// 该翻译的内容。BabelDOC 的文本白名单把 header/footer/seal 全放了进去、
// 却把 reference 注释掉了，因此四个区里有三个会被误翻。

#set page(width: 380pt, height: 260pt, margin: 25pt)
#set text(size: 9pt, hyphenate: false)
#set par(justify: true, leading: 4pt, spacing: 14pt)

#place(top + left, dx: 0pt, dy: 0pt, box(width: 330pt)[#text(size: 7pt, style: "italic")[Journal of Reproducible Layout, Vol. 3]])
#place(top + left, dx: 0pt, dy: 24pt, box(width: 330pt)[The first body paragraph is the only kind of text on this page that a translator should ever see. Everything around it belongs to a policy zone.])
#place(top + left, dx: 0pt, dy: 78pt, box(width: 330pt)[The second body paragraph is likewise ordinary prose. Between them the page carries a running head, a folio, a reference entry and a seal.])
#place(top + left, dx: 0pt, dy: 132pt, box(width: 330pt)[#text(size: 7pt)[[1] Smith et al. Layout preservation in machine translation. 2024.]])
// 印章：红色圆环 + 环内文字，右下角。
#place(top + left, dx: 250pt, dy: 155pt,
  circle(radius: 26pt, stroke: 1.4pt + rgb("#c02020")))
#place(top + left, dx: 258pt, dy: 176pt,
  box(width: 60pt)[#align(center)[#text(size: 7pt, fill: rgb("#c02020"))[APPROVED]]])
#place(top + left, dx: 0pt, dy: 195pt, box(width: 330pt)[#align(center)[#text(size: 7pt)[17]]])
