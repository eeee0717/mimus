#set page(width: 420pt, height: 220pt, margin: 30pt)
#set text(size: 9pt, hyphenate: false)
#set par(justify: true, leading: 4pt)

#let L1 = [The abstract opens with enough text to establish the left column as a continuous reading region before the boundary case appears.]
#let L2 = [A second left-column block keeps the staggered geometry distinct from a table or a row-aligned grid.]
#let LEFT = [Its final left-column sentence reaches the foot of the column and]
#let RIGHT = [continues at the head of the right column without a semantic break.]
#let R2 = [The right column then resumes ordinary abstract prose after the continuation has completed.]
#let R3 = [A final block closes the abstract and remains inside the same model-owned region.]
#let slot(dx, dy, body) = place(top + left, dx: dx, dy: dy, box(width: 165pt, body))

#slot(0pt, 0pt, L1)
#slot(0pt, 54pt, L2)
#slot(0pt, 108pt, LEFT)
#slot(195pt, 0pt, RIGHT)
#slot(195pt, 44pt, R2)
#slot(195pt, 88pt, R3)
