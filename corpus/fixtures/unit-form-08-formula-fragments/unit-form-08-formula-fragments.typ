// unit-form-08-formula-fragments —— 一行带上下标的公式
//
// FORM-08 的四组合并条件是为了把被切碎的角标缝回公式。这一行 $x_i^2 + y_j^3 = z$
// 里，每个变量都同时带上标与下标，正是最容易被切成多个公式单元的形状。
//
// 实测：两个独立解析器给出的**字形次序不同**——mutool 把上标、基线、下标分成
// 三行（"𝑥2" / "𝑖+ 𝑦3" / "𝑗= 𝑧"），poppler 的词切分给出 "𝑥 2𝑖 + 𝑦 𝑗 3 = 𝑧"。
// 因此本 fixture 不做块级次序断言，详见 manifest 的 [[adjudication]]。

#set page(width: 300pt, height: 120pt, margin: 25pt)
#set text(size: 9pt, hyphenate: false)
#set par(leading: 4pt)

$x_i^2 + y_j^3 = z$
