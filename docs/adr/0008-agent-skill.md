# ADR-0008 · Agent 集成：CLI 为唯一执行接口，通过 skills CLI 安装薄 Agent Skill

- 状态：已接受（2026-08-21）
- 决策层级：难逆（机器调用协议、发布包结构与后续 agent 集成都建立其上）

## 背景

ADR-0001 将 V1 交付形态定为 CLI，最初只设计了面向人的输出；`CONTEXT.md` 还把 `--json` 推到后续版本。产品现在新增一个明确形态：仓库还要提供 Agent Skill，让支持 skill 的 agent 能把 `mimus` 当作本地工具完成翻译、资产准备与诊断。

可选方案有三种：在 skill 内复制业务流程、为 agent 另建 MCP/常驻服务，或让 skill 只编排稳定的 CLI。复制流程会产生两个行为源；MCP 会扩大 V1 的协议、生命周期与分发范围。CLI 已经是公开入口，因此最小且可长期维护的方案是薄 skill + 机器可读 CLI。

## 决策

1. **CLI 是唯一执行接口与行为真源。** 人、脚本和 Agent Skill 最终都调用同一个 `mimus` 二进制；skill 不实现 PDF、模型、翻译或缓存逻辑。
2. **仓库提供一个通用 `mimus` Agent Skill。** 以 `skills/mimus/` 维护，至少包含 `SKILL.md` 与 agent 元数据；用户通过 `npx skills add eeee0717/mimus` 安装。skill 覆盖 `translate`、`inspect`、`assets pull` 三条既有工作流，不新增同义命令。
3. **机器调用协议进入 V1。** 所有子命令支持 `--json`；该模式在 stdout 输出带 `schema_version` 与事件类型的 NDJSON，禁止 spinner、颜色和交互提示，最终必须有且仅有一个 result/error 终结事件。人类进度与诊断走 stderr，分类退出码保持为脚本的第一层判断依据。
4. **skill 不接触密钥值。** 它只检查所需配置是否存在，API key 仍仅来自环境变量或配置文件，不进入参数、prompt、日志或结构化输出。
5. **Skill 安装不承担运行时安装。** `npx skills add` 只安装指令包；`mimus` 二进制仍从 GitHub Release 安装，模型与字体仍由 CLI 的资产机制管理。skill 必须检查 CLI 是否存在及版本是否满足其声明的兼容范围，缺失或不兼容时给出明确安装指引。
6. **分阶段交付。** M1 随首条端到端 CLI 固化机器协议并建立契约测试；M2 让真实翻译完整覆盖该协议；M4 编写、校验、通过 `npx skills add` 发布安装路径并以干净环境前向测试 Agent Skill。
7. **V1 不提供 MCP、常驻 daemon 或 vendor-specific plugin。** 将来如需这些形态，仍以 CLI/`mimus-core` 为内核另行决策。

## 后果

- agent 集成保持很薄，CLI 修复一次即可同时惠及人、脚本和不同 agent。
- `--json` 不再是后续增强，而是 V1 的兼容性接口；事件 schema 需要版本字段、契约测试和变更纪律。
- 二进制与 skill 有两条明确的安装路径；`npx skills add` 不会让 Node.js 成为 `mimus` 的运行时依赖。
- skill 从仓库安装，可能与已安装的 CLI release 发生版本漂移，因此兼容范围与跨版本前向测试是发布门槛。
- skill 必须简洁并按需引用 CLI 帮助，不复制一份容易过期的完整参数手册。
- M4 增加 skill 结构校验与真实 agent 前向测试，但不把模型生成结果当作确定性 CI oracle。
