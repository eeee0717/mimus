// FORM-10: bold is preserved; italic, size-only, and script changes are not.
#set page(width: 420pt, height: 120pt, margin: 30pt)
#set text(size: 10pt, hyphenate: false)

#block[Regular #text(weight: "bold")[strong] #text(style: "italic")[emphasis] #text(size: 7.5pt)[(small note)] x#super[2] end.]
