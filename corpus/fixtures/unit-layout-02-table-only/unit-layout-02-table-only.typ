// unit-layout-02-table-only —— 整页只有一个有线表格
//
// 页面上没有任何正文段落。BabelDOC 的 fallback 聚类会把表格内文字升格为可翻译
// 正文（优先级 idx 64 高于 table 的 65）；mimus 的政策是表格不翻，因此这一页的
// 正确行为是**零条翻译请求**。

#set page(width: 340pt, height: 160pt, margin: 25pt)
#set text(size: 9pt, hyphenate: false)
#set par(justify: true, leading: 4pt, spacing: 14pt)

#table(
  columns: (80pt, 100pt, 80pt),
  rows: 30pt,
  align: horizon,
  stroke: 0.6pt,
    [Run], [Throughput], [Latency],
    [first], [1204 ops], [8.1 ms],
    [second], [1198 ops], [8.3 ms],
)
