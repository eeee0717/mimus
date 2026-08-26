// FORM-01: math-shaped text remains tagged as ordinary text by the fallback
// layout detector. The final line is a prose-shaped negative control.

#set page(width: 500pt, height: 220pt, margin: 25pt)
#set text(size: 10pt, hyphenate: false)
#set par(leading: 8pt)

#block[#text("Attention(Q,K,V) = softmax(QK^T / sqrt(dk))V (1)")]
#v(12pt)
#block[#text("Q")]
#v(12pt)
#block[#text("dmodel×dk")]
#v(12pt)
#block[#text("MultiHead(Q,K,V) = Concat(head1,...,headh)WO")]
#v(12pt)
#block[#text("This method improves translation quality across documents.")]
