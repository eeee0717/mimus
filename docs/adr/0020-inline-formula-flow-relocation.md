# ADR-0020 · 已翻译多行段内的 inline formula 行流重定位

- 状态：已接受（2026-08-27）
- 决策层级：难逆（公式保真边界、写回字节合同和几何验收建立其上）
- 修订：2026-08-29 补充 StylesAndFormulas 的公式单元边界合同与 Typeset 阅读连续性

## 背景

ADR-0013 将写回钉死为 text-show 操作数区间替换，并据此规定公式 span 不进入译文
替换集。#95 为混排段增加固定公式槽后消除了静默叠印，但 L5-2 的 11 个多行
inline-formula 段全部因固定槽被公式墨迹切碎而 `typeset_overflow`，共损失 1,245 Han。
#87 T1/T2 证明扩大外层块只能恢复三个非公式块，保留率仍为 80.84%。

继续固定公式几何无法达到 95% 质量门槛；把公式转成输出字体文字又会破坏源操作数、
字体身份和公式字形保真。

## 决策

### 1. 启用边界

公式重定位只在以下条件全部成立时启用：

1. 段已通过 placeholder validator，并存在 `RestoredTranslation`；
2. 段包含 inline formula，且源段跨两条或更多 baseline；
3. #95 的固定公式槽计划已经失败；
4. 公式 text-show span 要么只包含该 inline-formula 单元，要么只与当前段完整拥有的可翻
   文本共享操作数；共享 span 不得含其它 passthrough 类字符，且每个公式 glyph 必须能
   唯一溯源到源编码字节、源字体和 show 前 text matrix；
5. 公式字符可定位、可见、直立，且单个 placeholder 对应一个有限、连续的 inline
   formula 单元；单元内部可以有多个相对 baseline，但必须能作为刚体整体平移。

任一条件不满足，保持现有段级 `typeset_overflow`/`typeset_protocol` 降级。单行
TYPE-02 固定槽合同不变。`display_formula`、段外公式、非直立公式和未翻译段不动。

### 2. 行流与几何

译文 text token 与公式单元共同进入一个多行流。公式单元的占位宽高取源 metric/visual
box 的并集，单元内部字符、多个 show span 及其相对几何不变。行流可在 8 pt 下使用
ADR-0020 段落容器内的正常行槽；候选的译文墨迹与重定位公式墨迹必须同时满足：

- 位于 CropBox 内；
- 不与段外保留墨迹或非 owner layout region 相交；
- 不同输出行、译文片段和公式单元之间互不相交。

装不下时整段 `typeset_overflow`，不回退到部分替换或静默重叠。

### 3. 字节与字体身份

公式仍在自己的源 text-show 操作数 span 内被替换。替换程序先用空操作数中和原 show，
再在 `q/Q` 内以临时 CTM 平移重发：

- 独占 span 的原 text-show 操作数字节逐字嵌入，不解码后重编码；
- 与可翻文本共享的 span 只逐 glyph 重发公式的原编码字节，并逐 glyph 恢复源字体、字号
  与 show 前 text matrix；原 operand 的可翻文本字节不得重发；
- 使用该源位置已经生效的原字体、字号、字符间距、rise、render mode 与线性变换；
- 只增加把源 baseline 移到目标行流 baseline 的平移；
- 重发后恢复原 text matrix advance，使后续原 content program 状态不变。

因此公式的源字体对象号、子集标签、公式 glyph 编码字节、Unicode/code 和单元内部相对
几何不变；独占 span 还保持整个 operand 的 lexical bytes，共享 span 不作这一保证。只有
已翻译段内 inline formula 的绝对 baseline/box 可以改变。输出 round-trip validator
把这些被修改 span 的公式字符按新 baseline 列入 expected typeset characters，validator
容差不放宽。

### 4. 对 ADR-0013 和验收哈希的修订

ADR-0013 的“公式 span 不替换”收窄为：display formula、段外公式、未翻译段公式，及不
满足 §1 的 inline formula span 不替换。满足 §1 的段内 inline formula span 可以为了原字节
重发而进入替换集；这不是用译文替换公式。

原“公式 canonical hash 含几何跨 stage 不变”拆为两条：

- 字节/身份 hash：独占 span 的 operand 字节或共享 span 的公式 glyph 编码字节，加上
  Unicode/code、字体对象号与子集标签，StylesAndFormulas 至 Write 必须不变；
- 几何 hash：display formula 和未重定位 inline formula 仍跨 stage 不变；已重定位单元只
  允许整体平移，单元内部相对几何必须不变。

IL schema 保持 v1；CLI schema 保持 v2。本决策不增加配置开关。

### 5. 公式单元边界

PP-DocLayoutV3 的 `inline_formula` label 是公式**存在性**的唯一权威。fallback 数学形状
启发式不得据此新建 model 公式；StylesAndFormulas 也不得收缩模型已经标出的字符。

模型框可能漏掉同一数学单元的下标、上标或末尾定界符。已有 model 公式锚时，可以把
相邻 `text/translate` 字符提升为 `inline_formula/passthrough`，但必须同时满足：

- 与锚在同一段和同一视觉行，水平间距按相邻字号的 em 有界，且没有显式或推导出的
  词间边界；
- 至少有一项 typed 证据：相对锚发生有界脚本基线偏移、补全未配平定界符、与锚使用
  同一数学字体的连续字母数字 run，或属于紧连的数学后缀。其中“数学字体”必须由该
  字体内已有 model 锚的 Unicode Mathematical Alphanumeric Symbols 字符证明；普通
  ASCII 公式锚与正文共用字体时，不得据此吞入相邻散文；
- 只改变 layout label/policy；Unicode/code、源 operand 引用与编码字节、字体引用、
  字号、baseline、metric box 和 visual box 均不改变。

每个公式锚和证据类别发一条 informational `formula_boundary_expanded`，字段包含页、段、
model reading order 和扩入字符数。事件受现有逐 ID 有界诊断预算约束，超额仍由
`dropped_diagnostics` 按 ID 记账。扩展后的完整连续字符区间才对应一个 `{vN}`，并整体
适用本 ADR 的字节、字体和单元内部相对几何合同。

### 6. fixed-slot 与 relocation 共用阅读连续性 oracle

几何不相交只是成功计划的必要条件。任何含 inline formula 的 fixed-slot 或 relocation
计划还必须通过同一个阅读连续性 oracle；两条路径不得拥有各自阈值。

连续性上界从源段事实推导：

```text
max(2 * median(source inline word spacing), 1.5 * median(source font size))
```

`source inline word spacing` 只取两类正有限样本：源空白字符的度量宽度，以及两个
`text/translate` 字符间由 `implicit_space_before` 标出的同行水平间距。model formula
任一端的间距不得进入样本，否则待检测的远端固定槽会反向抬高自己的合法上界。没有
词间距样本时，仍由源字号中位数给出 `1.5em` 下界；中位数避免单个合法宽空格或度量
离群值支配阈值。

oracle 按 `RestoredTranslation` 的 text/formula segment 顺序检查：

- 同行公式与前后语义邻居的间距不得超过推导上界；
- 语义邻居换行时，新行首项到所属行槽左边界的距离不得超过同一上界，防止 fixed
  formula 独自留在源行中部；
- 相邻项的阅读序与公式单元顺序不得逆转；
- 与公式邻接的标点不得拆行。relocation 放置器在装箱前把公式与相邻标点视为不可拆
  组合，oracle 再作最终验证。

门禁顺序固定为：fixed-slot 几何成功但连续性失败 → 在 ADR 本节之前已允许的范围内尝试
relocation → relocation 仍失败或不具备重定位资格 → `typeset_overflow` typed 段级保留。
oracle 拒绝后不得通过继续缩字号、扩大碰撞容差、删除公式字符或重绘公式来换取成功；
既有字号搜索只服务于进入 oracle 之前的几何装箱。

## 后果

- L5-2 离线回放实际恢复 11 个多行混排溢出中的 9 个，使双提取器 Han 保留率从
  80.84% 提升到 96.82%，超过 95% 门槛；所有 50 个独占 operand span、84 个公式字符和
  30 个公式单元均保持 operand 字节与源字体引用身份，且双提取器可见。
- 2026-08-28 的 #102 修订为共享 operand 增加上述逐 glyph 分割口径；L5-3 的 `(4,9)`
  （69 Han）与 `(8,79)`（138 Han）可在不重发源文本的前提下恢复。无法证明完整段所有权、
  唯一 glyph 溯源或不含其它 passthrough 类字符时仍以 `typeset_protocol` 整段 fail closed。
- 写回会修改更多 operand span，因此 retained-character 账目必须把重定位公式从“保留字符”
  转入“带新 baseline 的预期字符”。
- 含段外字符或其它 passthrough 类字符的共享 operand、跨行公式或奇异 content transform
  继续 fail closed；后续真实语料若证明这些形状重要，须另立 ADR 扩大边界。
- 1706 离线回放中，边界合同在 17 个段扩入 65 个字符；`(1,11)`、`(4,19)`、
  `(4,21)`、`(7,50)` 分别恢复完整序列、`d_model` 与 `epsilon_ls` 单元。受影响段的
  prepared request 和 cache key 随之改变，这是内容修复的必要结果。
- `unit-type-14-formula-continuity` 钉死短译文造成的 fixed-slot 超界空洞：固定计划虽无
  碰撞仍必须失败，随后以相同字号重定位源公式 operand；标点拆行和多公式逆序由共享
  oracle 单测覆盖。
