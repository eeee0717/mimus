// unit-form-02-display-enclosed —— 同一公式，被正文块吃进去约 60%
//
// form-01 / -02 / -03 是同一条公式 $E = m c^2$ 的三种空间关系，字形完全相同
// （实测宽度均为 34.74pt），唯一的变量是它与正文的位置关系。

#set page(width: 420pt, height: 170pt, margin: 30pt)
#set text(size: 9pt, hyphenate: false)
#set par(justify: true, leading: 4pt, spacing: 14pt)

// 正文用绝对定位以便把最后一行强制截短（`\` 换行），公式落在这一行右侧的空白
// 区里，纵向上骑在正文块底边上：约 60% 的公式度量盒落在正文块的 y 跨度内。
// 两个解析器都因此把公式并进正文块——这正是 LAYOUT-03 的 IoU 重标要处理的形状。

#place(top + left, dx: 0pt, dy: 0pt, box(width: 360pt)[A body paragraph runs the full width of the text block. Its final line is forced short so that the space to the right of it stays empty, which is where the same equation goes. \ It stops here.])
#place(top + left, dx: 170pt, dy: 24pt, $E = m c^2$)
