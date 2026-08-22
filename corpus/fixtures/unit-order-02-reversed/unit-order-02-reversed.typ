// unit-order-02-reversed — 整体倒序：先右栏自下而上，再左栏自下而上
//
// Corpus v1 fixture 源文件。几何在 manifest.toml 里由 poppler 与 mutool 一致
// 裁定（docs/03-corpus-requirements.md §2.1 例外条款）；结构化期望先于本文件
// 手写，本文件只负责按那份期望把内容画出来。

#set page(width: 420pt, height: 220pt, margin: 30pt)
// hyphenate: false —— 断行处不引入连字符，两个解析器在这一点上的分词差异
// 不该混进阅读顺序这个被观察量里（单变量原则，§2.4）。
#set text(size: 9pt, hyphenate: false)
#set par(justify: true, leading: 4pt)

// 六段正文。三份顺序变体共用同一组文本与同一组坐标，只换 `#place` 的先后。
#let P1 = [Layout preservation is the whole point of this tool. A translated page must land its text in the same columns, in the same reading order, as the source page.]
#let P2 = [The model supplies the reading order. Everything downstream consumes that order rather than the order in which glyphs happen to be drawn.]
#let P3 = [Where the model is silent, geometry takes over. A fallback sort by column and baseline keeps the pipeline honest when detection misses a block.]
#let P4 = [Content stream order is not reading order. Engines emit footnotes before body text, and some tools interleave columns line by line.]
#let P5 = [This fixture exists to make that distinction observable. Two independent parsers report the same page from two different angles.]
#let P6 = [Nothing here is adjusted to match what the tools produced. The structure was written down first.]

// 左栏 x=30pt，右栏 x=225pt；栏宽 165pt，栏间距 30pt。
// 纵向槽位刻意**不排成网格**：左栏 0/54/108（四行段），右栏 0/44/88（三行段），
// 只有首行对齐。三个槽位若在两栏间逐行对齐，poppler 的版面分析会把整页判成
// 行优先（实测记录见 manifest 的 [[adjudication]]），那是网格/表格的读法而不是
// 双栏正文的读法——本 fixture 的被观察量是绘制次序，不该混进这一条。
#let slot(dx, dy, body) = place(top + left, dx: dx, dy: dy, box(width: 165pt, body))

// 绘制顺序：整体倒序：先右栏自下而上，再左栏自下而上
#slot(195pt, 88pt, P6)
#slot(195pt, 44pt, P5)
#slot(195pt, 0pt, P4)
#slot(0pt, 108pt, P3)
#slot(0pt, 54pt, P2)
#slot(0pt, 0pt, P1)
