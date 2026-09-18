# Agent Note: 原生插件 crate 拥有面向 agent 的编写指引

Status: implemented

[English](2026-09-18-native-plugin-crate-guidance.md) | 中文

## 问题

`seekdeep` 二进制附带的每个插件都是一个 Rust crate，然而 agent 能找到的编写指引描述的仍是 TypeScript 时代：「添加包」实操手册搭建的是 `packages/<group>/<pkg>/src/index.ts`，`packages/AGENTS.md` 谈的是 `src/types.ts`，`crates/` 之下没有任何子树指令。crate 约定只以示例形式散落在两百多个 crate 中：`NAME`、`INJECT` 和 `plugin()` 导出，面向 Loader 的配置校验，启动名单中的 catalog 注册，bundle 行，README 三元组，组合测试与 oracle 测试，生成的 catalog，以及 parity manifest（元数据清单）。照着可见文档做的 agent 会产出一个没有运行时的包壳，并错过会拒绝它的门禁。

## 决定

`crates/AGENTS.md` 承载插件 crate 的子树指令：运行时位于何处、crate 的形态、测试与证据，以及接线、文档与门禁。`crates/CLAUDE.md` 是它的符号链接，`docs/AGENTS.md` 把该子树列入指令层级，预算 manifest 为该文件设定上限。`seekdeep-native-plugin` skill（技能）规定工作顺序：依据运行时放置规则在原生 Rust 与 TypeScript 文件插件之间做出选择，从同类角色的已发布 crate 起步，实现、接线、测试、重新生成、记录并验证，然后汇报。该 skill 链接契约而不是复述它们；`packages/AGENTS.md` 仍是与语言无关的行为规则的归属。

## 考虑过的替代方案

**在 `docs/cookbook/` 下放一个配对的实操手册页面。** 这是面向人的正确归属，但它的每次编辑都要付出中文对应稿、站点导航和投影检查的成本，而眼下的缺口是面向 agent 的。推迟；该 skill 与 `crates/AGENTS.md` 正是它将要概括的来源。

**就地扩展 TypeScript 实操手册。** 否决：该页面记述的是插件的 manifest 与 README 那一半以及受保留的文件插件表面；把 crate 机制并入其中会模糊哪一半才是运行时。

**一个脚手架命令。** 暂时否决：生成器会固化单一的 crate 形态，而移植的 crate 仍随角色而异；复制一个已发布的 crate 已经能给 agent 一个可运行的起点。

## 后果

- 添加插件 crate 的 agent 有一条有序路径，以及在宣称检查通过之前必须运行的门禁。
- `crates/AGENTS.md` 与其他子树指令一样受预算约束且仅有英文；该 skill 与所有 skill 一样不配对。
- 在配对的原生实操手册页面出现之前，TypeScript 实操手册仍是文件插件与包 manifest 的权威来源。
