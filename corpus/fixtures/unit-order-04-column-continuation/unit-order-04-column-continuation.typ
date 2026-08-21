// unit-order-04-column-continuation —— 左栏末句在栏底截断、右栏首段续接
//
// 用 Typst 的真实分栏流：L1 与 L2 恰好填满左栏，L2 因此落在栏底；R1 承接同一句
// 话，落在右栏顶端。两者的 y 跨度相差接近整个栏高，正是 ORDER-02 的跨栏合并
// 判据（y2 差 > 20pt）会触发的形状——而这里它该触发，是真阳性。

#set page(width: 420pt, height: 180pt, margin: 30pt)
#set text(size: 9pt, hyphenate: false)
#set par(justify: true, leading: 4pt, spacing: 14pt)

#columns(2, gutter: 30pt)[
A long paragraph opens the left column and keeps going for several lines, because the point of this fixture is that the column has to run out of room. Reading order must survive that boundary without inventing a paragraph break where the typesetter merely ran out of vertical space.

The sentence that matters starts down here, at the foot of the left column, and it does not finish

before the column ends, so it picks up again at the top of the right column and completes itself there.

A final paragraph follows in the right column. It is a separate paragraph, not a continuation of anything, and merging it into the sentence above would be exactly the failure this fixture is built to catch.
]
