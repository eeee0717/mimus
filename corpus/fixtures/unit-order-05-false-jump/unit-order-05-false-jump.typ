// unit-order-05-false-jump —— 单栏，但正文起点被人为抬高
//
// 全页只有一栏，三个块的 x 跨度完全相同。B1 贴在页顶（上边距 12pt 而非 30pt），
// B2 之前插入 46pt 的纯纵向空白。只看 y 跳变、不看 x 归属的跨栏判据会在
// B1→B2 之间误判出一次栏切换——这正是 ORDER-02 判据的假阳性形状。

#set page(width: 420pt, height: 230pt, margin: (x: 30pt, top: 12pt, bottom: 30pt))
#set text(size: 9pt, hyphenate: false)
#set par(justify: true, leading: 4pt, spacing: 14pt)

A single column runs down this page from top to bottom.

#v(46pt)

The gap above this paragraph is far larger than a paragraph gap. A criterion that decides column switches by looking only at how far the vertical position jumped will fire here, and it will be wrong, because there is no second column on this page to switch into.

This paragraph follows the previous one at an ordinary distance. Nothing about the pair is unusual, which is the point: the only anomaly on the page is the gap above, and it is purely vertical.
