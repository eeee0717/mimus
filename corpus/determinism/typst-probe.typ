// Corpus v1 · Typst 确定性探针（docs/03-corpus-requirements.md §2.6）
//
// 这份探针不是 fixture，只服务于 `corpus determinism`：它要覆盖真实 fixture
// 会用到的产物特征——嵌入子集化字体、多行断行、CMap——否则门禁会漏掉
// 只在这些路径上出现的不确定源（子集标签就是这样被 XeTeX 暴露的）。

#set page(width: 240pt, height: 160pt, margin: 20pt)
#set text(size: 10pt)
#set par(justify: true)

Corpus v1 determinism probe. This paragraph is long enough to wrap across
several lines so that the engine exercises its line-breaking and font
subsetting paths, not just a single short run of glyphs.

#text(weight: "bold")[Second font face.] Mixed weights force a second subset.
