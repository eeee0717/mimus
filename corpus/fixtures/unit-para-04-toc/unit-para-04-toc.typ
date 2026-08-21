// unit-para-04-toc —— 六种 leader 的目录
//
// PARA-04 的切段判据是「连续 20 个点」——硬阈值。这一页给出六种 leader：
// 40 个点（过阈）、8 个点（不过阈）、U+22EF 重复、纯右对齐无 leader、
// U+00B7 重复、以及第二个纯右对齐条目。目录条目与页码是否被切开、页码是否
// 与下一条目的标题粘连，都在这一页上可观察。
//
// leader 用 `#("." * n)` 这样的字符串乘法写出，不用 markup 里的连续点号：
// Typst 的 markup 会把 `...` 归并成 U+2026，那样就测不到「连续 20 个点」了。

#set page(width: 340pt, height: 160pt, margin: 25pt)
#set text(size: 9pt, hyphenate: false)
#set par(leading: 4pt, spacing: 14pt)

1 Introduction#("." * 40)3 \
1.1 Background#("." * 8)7 \
2 Method#("\u{22EF}" * 8)12 \
2.1 Setup#h(1fr)18 \
3 Results#("\u{00B7}" * 12)24 \
4 Conclusion#h(1fr)31
