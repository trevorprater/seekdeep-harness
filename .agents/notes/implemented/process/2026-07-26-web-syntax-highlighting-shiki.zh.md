# Agent Note: web client 的语法高亮——同步细粒度的 shiki

Status: implemented

[English](2026-07-26-web-syntax-highlighting-shiki.md) | 中文

> 范围：web client 唯一的一套语法高亮体系——依赖裁决、单例形态、token 表约定与各消费表面。本篇是 Code Mode UI 堆叠 PR（Pull Request）链的第五个 PR；[chat 子调用行 Agent Note](../feature/2026-07-26-code-mode-chat-subcall-rows.md)交付了 `run_code` 程序正文，而本体系存在的意义正是让它可读。样式的基本规则由 [Web 样式体系裁决](2026-07-19-web-styling-system.md)规定。

## 问题

client 过去把每一处代码表面——assistant 正文里的 markdown 围栏代码块、`run_code` 程序正文、details 面板的参数——一律渲染成不带高亮的等宽纯文本。本堆叠 PR 链的主要载荷是模型撰写的 TypeScript；未经高亮的程序扫读起来明显更吃力，而仓库已经在自家 VitePress 站点上交付经 shiki 高亮的代码，于是 web 应用成了唯一不带语法高亮的代码渲染表面。

## 决策

**采用同步细粒度形态的 shiki，作为 `ui-primitives` 里的一个单例，主题化完全经由 CSS 自定义属性完成。**

- **依赖**：`shiki/core` + `@shikijs/langs`，经 `createHighlighterCoreSync` 搭配 `createJavaScriptRegexEngine({ forgiving: true })` 组装——不带 oniguruma WASM、没有异步 boot 初始化、对 bundle 友好。boot 集合为 `typescript`（内嵌 JS）、`shellscript` 与 `json`；23 种读取卡片语法置于封闭的 lazy 表之后。落在这份组合白名单之外的内容一律回退到几何完全一致的纯文本块，绝不报错。先例：VitePress 站点已经通过 Shiki 渲染全部文档代码；而在 TypeScript（正是此处要紧的载荷）上，TextMate 语法实质性优于正则高亮器。
- **单例与所有权**：编译后的 Rust/WASM（[browser_highlight.rs](../../../../crates/client-ui-primitives/src/browser_highlight.rs)）拥有别名、boot／lazy 准入、单次加载请求、加载计数发布、订阅、回退行为、HTML 派发、逐行 token 整形和尾部行归一化。由 [`xtask`](../../../../xtask/src/main.rs) 生成的纯库 ESM 包只提供一项薄 Shiki 能力：构造唯一的 `HighlighterCore`、嵌入三种 boot 语法，并承载 23 个语法级动态 import thunk，但不做 Harness 策略决策。Rust 调度延迟预热，使约 120-175ms 的引擎构建不进入流式 finalize 后的渲染路径。assistant 撰写的 `constructor` 等标签会在 Rust 封闭别名表中落空，不会沿对象原型解析。编译后的 `CodeBlock`（[browser_code_block.rs](../../../../crates/client-ui-primitives/src/browser_code_block.rs)）拥有高亮与纯文本两条分支；高亮分支通过 `dangerouslySetInnerHTML` 注入 Shiki 的静态 span 树，这是依赖文档规定的消费路径，且不含用户 HTML、脚本或处理器。
- **主题化**：shiki 的 `createCssVariablesTheme` 让每一种 token 颜色都经由 `--shiki-*` 自定义属性路由；取值本身住在新增的 `ui-theme/styles/shiki.css` token 表里（亮色在 `:root`、暗色在 `body[data-ds-dark-theme]`——层叠方式与其余每张样式表相同），由壳的 `base.css` 导入链引入。组件 CSS 保持只用 token；任何颜色字面量都不进入 JS 或组件样式表。背景/前景以别名指向既有的 markdown 代码块 token，使高亮块与纯文本块彼此一致。
- **表面**：markdown 围栏代码块（`MarkdownText` 的 `pre` 组件把单字符串围栏路由到 `CodeBlock`）、`run_code` 展开后的程序正文（`lang="typescript"`）、details 面板的 Input 参数（`lang="json"`），以及通过同一逐行 tokenizer 渲染的带行号 `ReadBlock`。工具输出从不做语法高亮——它是任意文本，硬猜一种语法，带来的误高亮会多于帮助；bash 卡片的输出只承载其自身 ANSI 序列声明的颜色，经由[终端卡片](../feature/2026-07-28-web-terminal-card.md)渲染。

## 曾考虑的替代方案

**`rehype-highlight`/lowlight。** 屈居次选：天然同步，bundle 体积约为三分之一，但基于正则的语法在 TypeScript 上的保真度肉眼可见地更差，而且仓库将从此同时运行两套高亮体系（站点用 shiki、应用用 highlight.js）、维护两套主题化词汇。

**完整的 `shiki` bundle，或 oniguruma WASM 引擎。** 否决：完整 bundle 会带上每一种语法和主题；WASM 需要异步加载，而这正是同步的 client 启动刻意规避的。细粒度 core 加三种语法，让成本与实际用量成正比。

**在 worker 中高亮/异步高亮。** 否决：载荷都很小（程序、围栏代码块、参数）；同步 JS 引擎微秒级就能把它们 token 化，而异步会引入一段未高亮代码的闪现，外加渲染机制的扰动，却没有任何实测得出的需要。

## 后果

每个消费方共用一套 Rust 所有的策略与一个 Shiki 单例。bundle 增量是 Shiki core 加三种 boot 语法；每种 lazy 语法只在首次使用后付费。token 颜色仍住在 `--shiki-*` 表。固定源码 spec、live WASM 策略／组件测试、优化 ESM 包测试与真实 Shiki→WASM smoke 共同锁定别名、token／HTML 输出、lazy 通知、回退、复制与 block 外壳。其余 Markdown 渲染器与整包 index 行仍各自保持待完成，直到完整组装路径完成编译。
