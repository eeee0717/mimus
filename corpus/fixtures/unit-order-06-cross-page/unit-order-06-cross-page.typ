// unit-order-06-cross-page —— 跨页续段与跨页假阳性各一处
//
// 三页。p1 末段在页边界处被句子中间截断（不以句号结尾），p2 首段续接同一句；
// p2 末段以句号完整结束，p3 首段是新章节。ORDER-03 的判据无条件合并「上页末段
// + 下页首段」，因此这份 fixture 同时含它的真阳性与假阳性。

#set page(width: 300pt, height: 150pt, margin: 25pt)
#set text(size: 9pt, hyphenate: false)
#set par(justify: true, leading: 4pt, spacing: 14pt)

A first page opens with an ordinary paragraph. It begins and ends on this page, and nothing about it reaches across the page boundary.

The last paragraph of the first page is cut off by the page boundary in the middle of a sentence, which means it does not end with a
#pagebreak()
full stop and therefore continues at the top of the second page, where the remainder of the same sentence finally arrives.

The last paragraph of the second page, by contrast, ends properly with a full stop. It is complete, and joining it to whatever follows would be wrong.
#pagebreak()
#text(weight: "bold")[Chapter Two]

The third page opens a new chapter. Its first paragraph has nothing to do with the last paragraph of the previous page, and a rule that always joins them will always be wrong here.
