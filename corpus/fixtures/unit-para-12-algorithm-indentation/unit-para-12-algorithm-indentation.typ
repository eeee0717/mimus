// PARA-12: raw text preserves four leading-space indentation levels.
#set page(width: 300pt, height: 160pt, margin: 30pt)
#set text(size: 10pt)

#place(top + left)[#raw(block: true, lang: "text", "root\n  child\n    grandchild\n      leaf")]
