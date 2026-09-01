// PARA-11: ten visual lines per column omit source space glyphs at returns.
#set page(width: 340pt, height: 180pt, margin: 20pt)
#set text(size: 10pt, hyphenate: false)
#set par(justify: false, leading: 2pt)

#let col1 = [Alpha words#linebreak()cross narrow#linebreak()lines and each#linebreak()return must#linebreak()preserve a#linebreak()separating space#linebreak()before the next#linebreak()ordinary word begins#linebreak()in compact#linebreak()English prose.]
#let col2 = [Bravo words#linebreak()wrap through#linebreak()the same narrow#linebreak()measure so every#linebreak()visual line#linebreak()boundary remains#linebreak()an ordinary#linebreak()word boundary#linebreak()in compact#linebreak()English prose.]
#let col3 = [Charlie words#linebreak()finish the#linebreak()three column#linebreak()control and every#linebreak()wrapped return#linebreak()keeps the request#linebreak()readable without#linebreak()joining nearby#linebreak()words in the#linebreak()third column.]

#place(top + left, dx: 0pt)[#box(width: 90pt, col1)]
#place(top + left, dx: 105pt)[#box(width: 90pt, col2)]
#place(top + left, dx: 210pt)[#box(width: 90pt, col3)]
