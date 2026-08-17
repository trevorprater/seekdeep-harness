# seekdeep-shell-env

[English](README.md) | 中文

这是工具无关的受管 shell 环境插件。它拥有 `shellEnv` 服务，即一份受信任的、
按执行收集的 `SEEKDEEP_*` 值注册表；模型可见的 shell 工具会把这些值加入前台
和后台调用。内置项归注册表自身所有，插件可以添加随 effect 生命周期释放的
声明。重复所有权、错误声明、未声明的运行时键或非字符串运行时值都会在快照
逸出之前失败。

本 crate 导出 Loader 插件（`plugin`）、直接安装器（`apply`）、
`ShellEnvRegistry`、contributor 类型、`SHELL_ENV` 强类型服务槽位以及解释为空的
不变量伴随模块。

## 配置

```yaml
- id: shell-env
  name: seekdeep-shell-env
  config:
    seekdeepHome: C:\Users\me\.seekdeep # 默认：$SEEKDEEP_HOME，然后 ~/.seekdeep
```

Loader 把 null 视为默认配置，在任何插件 effect 开始前验证 `seekdeepHome` 必须
是字符串，并与源对象 schema 一样容许面向未来的未知字段。

## 受管环境

每次调用都会收到一份重新收集、不可变并按键字典序排列的覆盖层：

- `SEEKDEEP_HOME`：绝对 harness 主目录，依次采用显式配置、非空环境变量，
  或操作系统主目录下的 `.seekdeep`。
- `SEEKDEEP_SHELL=1`：标识受管子进程。
- `SEEKDEEP_SESSION_ID`：精确的活动智能体会话 ID；无智能体执行时省略。
- `SEEKDEEP_SESSION_JSONL`：当前可选持久化提供方为活动智能体报告 `jsonl`
  位置时的绝对目标路径。

JSONL 路径只是位置提示而不是凭据；它可能在首次刷新前就出现，也不保证包含
仍在缓冲的当前轮次。

Contributor 声明稳定名称、保持插入顺序的键与描述，以及接收精确
`ToolExecution` 的同步解析器。注册过程会原子检查精确 `SEEKDEEP_` 前缀、全大写
后缀语法、保留内置键、空描述、重复名称和重复所有者。返回的 effect 既可显式
释放，也归注册时的 Cordis 上下文所有。`list()` 按键排序声明且不会运行解析器。

收集过程先快照 contributor，再按名称顺序运行，验证每个返回键都已声明且每个
值都是字符串，最后生成 `SeekDeepEnvironment`。本地执行器将通过专用
`ShellExecRequest.seekdeep_env` 通道先删除继承的受管键，再合并可信快照；本注册表
从不修改父进程环境。

## 模型与缓存影响

插件本身不生成模型可见内容，也不会使 KV 缓存前缀失效。shell 工具消费方负责
决定如何在描述和请求中呈现通用 `$SEEKDEEP_*` 约定。

## 限制

`list()` 有意只报告 contributor 声明。注册表自有的内置键
（`SEEKDEEP_HOME`、`SEEKDEEP_SHELL`、`SEEKDEEP_SESSION_ID`）不在其中，因此诊断
和 UI 代码不得把它当作完整目录。
