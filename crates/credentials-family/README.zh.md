# credentials — 凭据引用

[English](README.md) | 中文

凭据能力家族将引用解析与具体提供方分离：

| Crate | 职责 | Context 键 |
|---|---|---|
| [`seekdeep-credentials`](../credentials/README.zh.md) | 凭据引用 seam | `credentials` |
| [`seekdeep-credentials-local`](../credentials-local/README.zh.md) | 环境与本地文件提供方 | 注册 `credentials` |

配置只携带引用，不携带机密值。消费者在各自操作边界解析引用；写入、优先级与存储语义由子 crate 的 README 定义。

子系统参考——`CredentialRef`、逐操作解析、UI 安全的 `CredentialInfo` 与提供方分层——见[凭据子系统指南](../../docs/subsystems/credentials.md)。
