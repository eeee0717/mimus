// unit-form-01-display —— 独立居中公式
//
// form-01 / -02 / -03 是同一条公式 $E = m c^2$ 的三种空间关系，字形完全相同
// （实测宽度均为 34.74pt），唯一的变量是它与正文的位置关系。

#set page(width: 420pt, height: 170pt, margin: 30pt)
#set text(size: 9pt, hyphenate: false)
#set par(justify: true, leading: 4pt, spacing: 14pt)

The equation below is set on a line of its own, centred, with clear space above it and below it. Nothing else shares its vertical band.

#v(10pt)
#align(center, $E = m c^2$)
#v(10pt)

Body text resumes after the equation. A model that reports a display formula here has nothing to disambiguate, because no text block overlaps it.
