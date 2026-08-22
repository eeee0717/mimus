// unit-form-04-superscript —— 上下标双阈值的四种邻近情形
//
// FORM-04 的判据是「字号 < 前一个字符的 0.79 倍则进入角标态，≥ 1.1 倍才退出」。
// 四段分别是：(a) 真上下标；(b) small caps 标题（约 0.8 倍，恰在阈值边上）；
// (c) 首字下沉（首字 3 倍，第二个字符相对它只有 1/3，必然触发进入）；
// (d) 正文中插入的小字号括注。只有 (a) 该被判为公式。

#set page(width: 380pt, height: 220pt, margin: 25pt)
#set text(size: 9pt, hyphenate: false)
#set par(justify: true, leading: 4pt, spacing: 16pt)

True superscript and subscript: $x^2 + y_1$

#smallcaps[Small Caps Heading]

#text(size: 27pt)[D]rop caps open this paragraph with a letter three times the body size, after which every remaining character is back at the body size and must stay ordinary text.

A parenthetical set two points smaller #text(size: 7pt)[(like this one)] sits inside an otherwise ordinary sentence and must not turn the sentence into a formula.
