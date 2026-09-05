# seekdeep-launch-environment

[English](README.md) | 中文

SeekDeep 单次启动环境的不可变快照，记录每个值由哪一层提供。消费方通过该快照解析面向
用户的配置，而不是依赖扁平化的进程环境，因为各层具有不同的信任级别和来源信息。

| 层 | Rust 变体 | 含义 |
|---|---|---|
| 继承的进程环境 | `Process` | 启动 shell、CI 任务或容器传入的显式意图 |
| `<invocation cwd>/.env` | `ProjectEnv` | SeekDeep 启动所在项目拥有的配置 |
| `$SEEKDEEP_HOME/.env` | `UserEnv` | 用户级机器默认值 |

启动器也可以把已接受值写入进程环境，供配置表达式和第三方库使用。但该扁平化视图不是
harness 自有解析的权威来源。

## 解析

`LaunchEnvironmentSnapshot::get(name)` 按规范信任顺序搜索所有层；
`get_from(name, sources)` 只搜索允许的层，同时无视参数切片顺序，始终保留规范顺序。
省略某一层表示拒绝，而非降低优先级；调用方无法通过重新排序让它胜出。

名称匹配遵循平台语义：POSIX 上精确匹配，Windows 上不区分大小写。这样可防止项目中
大小写不同的变量越过启动进程继承的同一变量。

```rust,no_run
use seekdeep_cordis::Context;
use seekdeep_util::launch_environment::launch_environment_of;

let context = Context::new();
let endpoint = launch_environment_of(&context)
    .get("DEEPSEEK_BASE_URL")
    .map(|entry| entry.value);
```

`launch_environment_of(context)` 返回启动器提供的同一个快照。若产品启动器没有引导该
组合，则把继承的进程环境冻结为唯一层。SDK 宿主或裸组合没有发现环境文件，所以可用值
确实都来自其进程。

构造时会复制每一层输入。后续 map 修改、`chdir`、工作区选择或会话恢复都不能改变启动
快照。空字符串保持为已存在值，由拥有该字段的消费方判断是否合法。同一来源重复输入时，
最后一层胜出。

## 已知限制

- 快照不是子进程边界；已物化的值仍可在子进程清理策略允许的范围内到达子进程。
- 不存在按工作区划分的层。项目层固定为启动目录，模型后来选择的工作区不能在会话中途改变
  harness 环境。
