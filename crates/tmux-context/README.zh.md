# seekdeep-tmux-context

[English](README.md) | 中文

这是一个可选插件，用持久上下文描述智能体进程所在的 tmux 会话、窗口、
窗格以及窗口的窗格树布局。插件在模型请求准备阶段每轮最多采样一次，默认的
Web 和无头组合不会加载它。

## 配置

```yaml
- id: tmux-context
  name: seekdeep-tmux-context
  config:
    refreshIntervalMs: 60000 # 可选；省略或设为 0 时检查每个发生变化的轮次
```

`refreshIntervalMs` 必须是非负的 JavaScript 安全整数。省略或设为 `0` 时，
插件会在每个符合条件的轮次查询，但只在 tmux 状态变化时注入。正值还会抑制
距离最近一次持久注入不足该毫秒数的查询。

## 如何读取 tmux

插件以前置方式注册 `agent/pre-step` 监听器，并且只在下游监听器允许进入第一步
后运行。需要采样时，它通过可选的 `shell` 服务执行一条只读命令：

```sh
[ -n "$TMUX_PANE" ] || exit 1
self_tty=$(ps -o tty= -p <pid> | tr -d ' ')
[ -n "$self_tty" ] || exit 1
pane_tty=$(tmux display-message -t "$TMUX_PANE" -p '#{pane_tty}') || exit 1
[ "$pane_tty" = "/dev/$self_tty" ] || exit 1
exec tmux display-message -t "$TMUX_PANE" -p '<format>'
```

tty 检查会排除仅从 tmux 祖先进程继承 `$TMUX_PANE` 的进程。插件不实现子进程
逻辑，执行会继承部署环境的 shell 沙箱与策略。缺少 shell 服务、tty 不属于
tmux、非零退出码、空窗格 ID 或格式错误都会静默跳过。位置是可选信息，因此
解析器拒绝或执行器故障会被包含并记录为警告，而不会使当前轮次失败。

命令返回会话与窗口标识、窗格标识、活动状态以及 `window_layout`。它不会读取
兄弟窗格内容，也不会报告像素尺寸。

## 时序与持久化

成功读取到变化状态后，插件会在批次开头加入一条带来源的 `UserMessage`。
智能体循环在 `step/start` 后记录它，来源为
`{ kind: "plugin", plugin: "tmux-context" }`。状态变化抑制和间隔调度扫描原始的
追加式会话事件，因此在压缩遮蔽和进程恢复后仍然有效，而且各会话相互独立。
下游拒绝、失败、取消或非第一步都不会记录内容。

模型看到的文本为：

```text
tmux location (turn <turn>):
session <session>, window <index> "<name>", pane <index> <pane-id>
window active=<0|1>, pane active=<0|1>, layout <window-layout>
```

每次变化的读取都会追加，直到被压缩遮蔽。状态未变和间隔抑制不会增加 token，
也不会使已有的 KV 缓存前缀失效。

## 限制

- 一轮中途发生的状态变化会在下一轮出现，而不会在步骤之间出现。
- 若窗口名含有字面量双字符序列 `\t`，制表符分隔的返回值会被视为格式错误并跳过。
- 基于 tty 的检测会有意排除只继承了 tmux 环境变量、但不共享窗格控制终端的进程。
- `ps -o tty=` 属于 POSIX；不支持它的环境会静默跳过。
