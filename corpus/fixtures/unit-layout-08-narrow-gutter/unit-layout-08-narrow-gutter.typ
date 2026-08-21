// unit-layout-08-narrow-gutter —— 栏间距 8pt 的双栏
//
// 栏宽 240pt、字号 10pt、**栏间距 8pt**（约 0.8 字宽）。LAYOUT-08 的行聚类半径
// 是 3.5 字宽，因此左右栏同一水平线上的字符会被连成一行。这份 fixture 的作用
// 就是让这件事可被观察，而不是让它不发生。

#set page(width: 548pt, height: 176pt, margin: 30pt)
#set text(size: 10pt, hyphenate: false)
#set par(justify: true, leading: 4pt, spacing: 12pt)

#columns(2, gutter: 8pt)[
The left column carries three paragraphs of ordinary prose, set at ten point with a two hundred and forty point measure.

Its gutter is eight points wide, which at this size is well under one character width and far under the three and a half character widths that a line clustering pass uses as its radius.

A pass that clusters characters into lines by horizontal distance will therefore join the left column and the right column into single wide lines.

The right column carries three more paragraphs, and it is set exactly like the left one so that the only variable on this page is the gutter.

Nothing else distinguishes the two columns. The measure, the size, the leading and the justification are identical.

If the two columns come back as one, the cause is the gutter and nothing else.]
