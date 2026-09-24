# @seekdeep-ai/seekdeep-typert-loader

[English](README.md) | 中文

生成的 Typert 产物所用的原生 Loader 集成。该插件需要 `ctx.loader`、`ctx.typert` 和编译产物目录 `ctx.typertArtifacts`；它本身不提供这些服务。

激活时，该插件会扫描现有的 Loader 配置项。随后它会监听 Cordis `internal/plugin` 生命周期通知，在原生产物目录中解析每个配置项所属的包，装载其编译后的 Host contribution，校验类型化 manifest（元数据清单），并注册该贡献项，直到配置项或本插件卸载。如果 factory 在任一所有者卸载后才结束，系统会丢弃其结果。

`packages` 用于列出需要为嵌套在另一 Loader 配置项下的插件额外注册的包产物。Cordis fiber 不会保留这些嵌套插件的包身份，因此这里通过显式配置划定边界；配置中列出的每个包都必须存在于 `ctx.typertArtifacts` 并暴露 Host factory。

发现式扫描遇到没有已注册 Host 产物的包时会跳过。缺失、无 Host、已装载和失败判定都会在本 loader 生命周期内缓存，因此变更产物集合后必须重启 loader。插件激活时，如果已挂载 Loader 配置项对应的产物格式错误，激活会失败；之后才发生的失败只会记录到日志，不会阻止无关包完成注册。

## 模型体验

无。loader 只向 [`ctx.typert`](../registry/README.md) 提供注册项；任何模型可见投影均由消费方负责。

#### KV Cache 影响

无直接影响。

## 已知限制与暂缓事项

- 发现机制只会装载 Host 侧产物；若要为 Client 运行时添加等价的发现机制，需要先有独立的组合所有者。
- 原生、WebAssembly 和兼容 package host 必须在 `ctx.typertArtifacts` 注册自己的类型化 factory。Loader 配置项会自动发现；嵌套插件或非 Loader 插件需要显式加入 `packages`，或由其所有者直接负责调用 `ctx.typert.register()`。
