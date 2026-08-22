// unit-layout-01-nested-boxes —— 嵌套框归属
//
// 一个有边框的表格区（x 100..400，y 30..230）里放一行说明文字；左边另有一段
// 正文，其右缘距表格框左缘 2pt。LAYOUT-01 的 68 项优先级表会让内层这行归到
// `plain text` 而不是 `table`，哪怕它 100% 落在表格框里、只被正文框擦到 2pt。

#set page(width: 430pt, height: 260pt, margin: 15pt)
#set text(size: 9pt, hyphenate: false)
#set par(justify: true, leading: 4pt, spacing: 14pt)

// 表格框：只画边框，不画格线——被观察的是框的归属而不是表格结构。
#place(top + left, dx: 85pt, dy: 15pt,
  rect(width: 300pt, height: 200pt, stroke: 0.6pt))
#place(top + left, dx: 95pt, dy: 105pt, box(width: 280pt)[Table 1. Throughput measured over ten runs.])
// 正文块：右缘 x=87pt，比表格框左缘 x=85pt 多出 2pt，刚好擦到。
#place(top + left, dx: 0pt, dy: 100pt, box(width: 72pt)[Body text ends here, two points shy of the frame.])
