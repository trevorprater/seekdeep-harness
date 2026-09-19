# Agent Note: 读取路径上的数字是 JavaScript 数字

Status: implemented

[English](2026-09-19-read-path-numbers-are-javascript-numbers.md) | 中文

## 问题

源码的读取工具、其渲染器、呈现模型和客户端读取卡片都以 JavaScript 数字承载偏移、限制、配置上限、行号和总数：`parsePositiveInteger` 接受任何不小于一的有限整数，`readMetaFromMeta` 用 binary64 算术比较回放值，消息与 JSON 输出按 `Number.prototype.toString` 和 `JSON.stringify` 格式化它们。移植把其中每个字段都收窄成了 `u64` 或 `usize`。偏移 2^64 在越界消息里饱和成 `18446744073709551615`，而源码打印 `18446744073709552000`；配置上限 `1e300` 无法反序列化，而源码接受它；行号超过 `u64` 的回放读取卡片退回到通用卡片，而源码会渲染它。五个 parity 行正是卡在这个缺口上。

## 决定

`seekdeep_lossless_json::JsonNumber` 是带有源码语义的一个 binary64 值：`Display` 通过 `ryu-js` 遵循 `Number.prototype.toString`，序列化对 2^53 以下的精确整数写出其数字，否则把最短的可往返文本写成原始 JSON 数字，非有限值序列化为 `null`，反序列化按 `JSON.parse` 的方式读取任何 JSON 数字字面量，包括把超出 binary64 的量级读作无穷大，相等性把所有 NaN 视为同一个值。读取工具的参数、输入、上限和结果，渲染器的窗口、行、总数和回放元数据，`FileLocation.line`、`ReadFileLine.number`、`ReadResultView.offset` 与 `totalLines`，以及客户端读取卡片都承载它。校验使用源码谓词（偏移、限制、上限和行号须为 `Number.isInteger` 且不小于一；总数须非负），比较以及 `offset - 1` 和 `endLine + 1` 算术在 binary64 中进行，上限只在使用点通过饱和转换来约束 Rust 切片。tools crate 重新导出该类型，因此构造 `FileLocation` 的工具 crate 无需额外依赖。

## 备选方案

**把字段加宽为 `u128` 或 `i128`。** 否决：没有整数类型能再现 binary64 的舍入，`2^53 + 1` 会与 `2^53` 比较不等，而源码的 `offset - 1` 使二者相等；2^60 的格式化仍会与源码的最短数字不同。

**保留 `u64`，只对消息做特判。** 否决：值本身就是可观察面；工具结果的 `offset`、持久化的读取元数据和回放的卡片都承载它，而不只是诊断信息。

**直接存储 `serde_json::Number`。** 否决：搜索卡片已经这样做，但其文本形式不提供算术或比较，而读取路径两者都需要；`JsonNumber` 只在序列化一个 JavaScript 文本并非其整数数字的值时才转换为它。

## 结果

- 七个待办的读取路径与会话重入 parity 行已验证；tool-fs 差分测试比较 4144 个源码观察值，包括位于 2^53、2^53+1、2^60、2^64、1e300 和 1e400 的偏移、限制、回放行号和总数。
- 运行时度量的计数（文件的总行数、窗口的行索引）通过 `From<u64>` 和 `From<usize>` 进入该类型，超过 2^53 时的舍入与源码算术完全一致；harness 能读取的文件都到不了那个范围。
- 搜索结果的行号和总数仍使用 `u64` 与 `serde_json::Number`；它们的行依据其他证据已验证，一旦出现缺口即可迁移到 `JsonNumber`。
