// FORM-10: exceed BabelDOC's historical 40-rich-placeholder cutoff.
#set page(width: 800pt, height: 150pt, margin: 30pt)
#set text(size: 10pt, hyphenate: false)

#let spans = range(45).map(_ => [r#text(weight: "bold")[B]])
#block[#spans.join()]
