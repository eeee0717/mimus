// unit-base-02-two-column —— 真实双栏流的正文基线
//
// 与 unit-order-01/02/03 用的是同一组正文、同一套栏宽（165pt）与栏间距（30pt），
// 唯一的区别是这里由 Typst 的 `columns` **自然分栏**，而那三份用 `place` 绝对
// 定位以便换绘制次序。因此这份是「真实排版引擎在双栏下会怎么发射 content
// stream」的参照：绘制次序与阅读次序在这里天然一致。
//
// 页高 220pt 是让六段恰好三三分栏的取值（200–230pt 都是 3/3，取中间值留裕度）。

#set page(width: 420pt, height: 220pt, margin: 30pt)
#set text(size: 9pt, hyphenate: false)
#set par(justify: true, leading: 4pt, spacing: 14pt)

#columns(2, gutter: 30pt)[
Layout preservation is the whole point of this tool. A translated page must land its text in the same columns, in the same reading order, as the source page.

The model supplies the reading order. Everything downstream consumes that order rather than the order in which glyphs happen to be drawn.

Where the model is silent, geometry takes over. A fallback sort by column and baseline keeps the pipeline honest when detection misses a block.

Content stream order is not reading order. Engines emit footnotes before body text, and some tools interleave columns line by line.

This fixture exists to make that distinction observable. Two independent parsers report the same page from two different angles.

Nothing here is adjusted to match what the tools produced. The structure was written down first.
]
