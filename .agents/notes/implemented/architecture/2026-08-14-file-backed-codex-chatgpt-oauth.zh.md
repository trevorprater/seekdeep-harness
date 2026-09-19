# Agent Note：基于文件的 Codex ChatGPT OAuth

Status: implemented

[English](2026-08-14-file-backed-codex-chatgpt-oauth.md) | 中文

## 问题

pi-ai 适配器已随包提供 `openai-codex` 提供方及其 Codex Responses 实现，但该提供方只能从 pi-ai 的 `CredentialStore` 中取得 OAuth 凭据。Harness 按请求提供 API 密钥，并刻意让该存储保持为空，因此这条路由无法使用 ChatGPT 订阅。要求用户把 access token 粘贴进 API 密钥字段，会产生一条会过期、无法刷新的凭据，也会错误描述设置方式。

Codex 官方 CLI 已经拥有 ChatGPT 浏览器登录、token 刷新、登出，以及位于 `$CODEX_HOME` 或 `~/.codex` 下的可配置凭据存储。Gollem 展示了提供方行为与登录类别，但它由应用持有的凭据文件是一份独立会话，并非用户的 Codex 官方登录。Harness 需要让登录只有一个明确所有者，并安全地把该登录适配给提供方，同时不能让 pi-ai 的凭据存储成为 API 密钥路由的第二真源。

先前的[仅 OAuth 提供方隐藏决策](../bug-fix/2026-08-13-oauth-only-providers-withheld.md)仍是没有这种适配器的提供方所采用的默认规则；本 Note 只针对 `openai-codex` 取代其中一部分。

## 决策

登录与登出归 Codex 官方 CLI 所有。`llm-pi-ai` 把其存于文件的 ChatGPT 会话适配为一份只为 `openai-codex` 返回值的 `CredentialStore`。`$CODEX_HOME` 从不可变启动环境快照读取，否则采用 Codex 默认的 `~/.codex`；每次请求仍会重新读取所选的 `auth.json`。Harness 既不启动 OAuth 回调服务器，也不创建或删除共享凭据；会话缺失时会指引用户设置 `cli_auth_credentials_store = "file"` 并运行 `codex login`，删除时则指引运行 `codex logout`。

该文件是外部凭据边界。读取时要求它是至多一 mebibyte、仅限所有者访问的普通文件，具有当前的 `auth_mode: "chatgpt"`、完整的 access/refresh/ID token 字段、有效的 access-token 到期时间，并且 token claim 与已存账户字段指向同一个 ChatGPT 账户。文件缺失或并非 ChatGPT 模式时视为无凭据。内容无效会关闭失败，且诊断不包含凭据值。桥接会保留根对象和 token 对象里的未知字段，因此刷新不会收窄较新版 Codex 文档。

过期判断、OAuth 刷新与 Codex 请求认证归 pi-ai 所有。其刷新回调在桥接的 `modify` 操作内运行。桥接使用仓库文件锁串行化自身写方，拒绝账户变化，在网络回调后重新读取外部文件，保留它观察到的 Codex 官方轮换，并且只在所观察的 token 仍为当前值时，才以 `0600` 模式原子替换文档。access token 的 claim 同时提供持久化到期时间，以及 pi-ai Codex Responses 传输所使用的 `chatgpt-account-id`。

API 密钥路由继续使用 Harness 凭据 seam，并以请求覆盖形式传入解析出的密钥；注入的存储不会向它们暴露任何值。可配置提供方目录使用 `authentication: 'api-key' | 'provider-native' | 'codex-oauth'` 明确标注设置方式。显式指定 `apiKeyEnv` 的 profile 仍标记为 `api-key`，包括原有的带密钥 Codex 路径；空的 `openai-codex` profile 标记为 `codex-oauth`。该值通过 `llm.providers` 到达 Models 页面；页面对 `codex-oauth` 显示 Codex 登录说明，省略 API 密钥输入框，且绝不派生或写入 API 密钥引用。该行为跟随适配器元数据，而不是浏览器内硬编码的提供方名称。

## 考虑过的替代方案

- **采用 Gollem 由应用持有的 OAuth 存储与登录流程。** 它证明了 ChatGPT 后端，但会创建另一份登录，且不使用用户的 Codex 官方会话。提供方在可用前，还需要 Harness 新增浏览器／设备登录的交互界面与生命周期。
- **把当前 access token 复制进 `ctx.credentials`。** 复制出的 token 会过期，API 密钥 seam 也无法携带刷新与账户元数据。refresh-token 轮换随后会让 Codex 或 Harness 中的一方持有陈旧状态。
- **给 pi-ai 一份通用的持久凭据存储。** 这会为每个 API 密钥提供方重新引入第二凭据来源及其 ambient 回落，削弱具名引用的失败语义。所选存储被刻意限制为不能服务 `openai-codex` 之外的任何 id。
- **每次请求或刷新都 shell out 到 `codex`。** CLI 提供登录命令，不是请求期凭据服务。子进程会增加延迟，仍然需要一项未记录的 token 交换或传输协议。
- **从操作系统钥匙串读取凭据。** Codex 可以选择钥匙串，但没有为其他应用公开可移植的读取 API。要求使用其有文档的文件存储，使数据来源显式且可测试；只有 `auto` 确实生成 `auth.json` 时它才可用。

## 影响

现有的文件型 `codex login` 现在无需 API 密钥即可认证一份 `openai-codex` profile，其中包括 pi-ai 原生 token 刷新与 Codex Responses 标头。Harness 启动后才完成的登录会在下一次请求被观察到。状态缺失、格式错误、权限过宽或账户不一致时，会在提供方网络 I/O 前以针对设置的凭据错误码失败。

适配器现在依赖 Codex 官方文件的当前字段。未来若出现不兼容格式，会关闭失败，直到该适配器更新。虽然文件内容保持实时，`$CODEX_HOME` 本身固定于 Harness 启动时。其他仅 OAuth 的 pi-ai 提供方继续隐藏，直到每个提供方都有显式、仅限自身的凭据所有者。

Harness 文件锁无法让 Codex 官方进程参与同一套写方协议。原子提交前观察到的 Codex 轮换会胜出，包括常见的 refresh token 重用竞争；在最后一次重新读取后才启动的外部写方仍不在此保证内。账户身份、完整文档与仅限所有者的替换规则，将后果限制在轮换协调，而不是凭据泄露或跨账户使用。

## 测试

凭据存储测试使用隔离的 `CODEX_HOME` 目录与合成的无签名 JWT。覆盖范围包括提供方隔离、缺失、非 ChatGPT 模式、错误 JSON、账户不匹配、权限拒绝、保留未知字段与 Codex 持有字段、仅限所有者的原子替换、拒绝账户变化、Codex 官方并发轮换，以及登录／登出所有权。

一条真实 Loader 组合挂载 `LlmRuntime`、settings-file、credentials-local 与 `llm-pi-ai`，随后向本地 Codex Responses 端点发送 `openai-codex` 请求。它断言 bearer token、`chatgpt-account-id`、端点与组装出的 assistant 文本。第二条组装路径会让初始 token 过期，让 pi-ai 针对一份本地响应执行其真实刷新实现，验证刷新后的 token 到达 Codex 请求，再读回共享文件以证明轮换结果与字段保留。会话缺失路径证明网络访问前出现 `MISSING_CREDENTIAL`。Host 协议测试与浏览器测试钉住 `authentication`、Codex 说明、API 密钥字段／写入的缺失、提供方选择，以及录制的 Models 页面输出。
