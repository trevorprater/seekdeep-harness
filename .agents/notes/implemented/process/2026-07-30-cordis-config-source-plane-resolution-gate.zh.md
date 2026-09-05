# Agent Note: verify-cordis-config 对配置中插件的源码归属实施门禁

Status: implemented

[English](2026-07-30-cordis-config-source-plane-resolution-gate.md) | 中文

## 问题

Loader 配置用外部包说明符命名插件，而 Rust 启动器会先从已编译的 `PluginCatalog` 解析；model（模型）编写的 JavaScript 只能通过兼容路径运行。兼容 manifest（元数据清单）、构建后的 JavaScript 与 Cargo 包可以分别发生漂移：本地产品说明符可能在其已编译 Rust 归属消失后仍留在 cordis.yml 中，开发目录树也可能用陈旧的 `lib/` 产物掩盖缺失的归属。由此产生的故障只会在干净检出目录启动该组合时出现。一次启动冒烟测试只能证明一种组合与一个平台；仓库中的发布配置、示例配置、fixture（测试前置数据）与 overlay 各自使用不同的插件集合。

## 决策

Rust [`verify-cordis-config`](../../../../crates/repository-tools/src/cordis_config_verifier.rs) 实现要求配置中每个本地包说明符都有源码树内的 Rust 归属。名为 `seekdeep-foo` 的 Cargo 包归属 `@seekdeep-ai/seekdeep-foo`；已提交的 Rust 源码可以声明额外的确切包标识；一张小型别名表则覆盖已编译的 Loader 内置项以及有意保留的 NPM／Cargo 命名差异。只有别名指定的 Cargo 包确实存在时，该别名才有效。检查使用子路径说明符中的包部分，报告每个引用无归属包的配置，并把相对路径、URL 与外部 model 编写的 JavaScript 说明符排除在本地包规则之外。

同一命令保留周边源约束：递归校验 group、insert 与 include patch 的元数据；检查所属 manifest 的依赖；检查自适应目录选择器通过运行时字符串挂载的包；分离 host 与每会话 preset 所属平面；并要求客户端包的 `./client` 导出与 `seekdeep.client` 声明一致。`!!js` 只解析而不执行。YAML 边界会保留真正的 tag，同时在 tag 规范化时排除带引号文本、注释与块标量正文。

## 备选方案

**依赖无密钥启动冒烟测试。** 冒烟测试只能针对所选 profile、环境与平台发现缺失归属。静态校验器覆盖发现到的每份配置，并在一次运行中报告所有缺失归属。

**把兼容 `package.json` 的 exports 当作源码归属。** 这些 exports 指向构建后的 JavaScript 与声明。接受它们会再次引入陈旧产物掩盖问题，还会让其他语言兼容层凌驾于 Rust 生产归属之上。

**要求 NPM 与 Cargo 后缀完全一致。** 大多数包遵循该约定，但框架内置项和少数既有产品标识有意采用不同名称。确切且带条件的别名会让这些例外保持可见，同时不削弱缺失归属检测。

## 结果

- 配置中的本地包若没有已编译 Rust 归属，`verify-cordis-config` 会在任何 profile 启动前失败。
- 新增或重命名配置中的包时，必须在同一次变更中提供 Cargo 归属、由 Rust 明确声明的标识或经评审的条件别名。
- 外部 model 编写的 JavaScript 仍受支持；归属检查只适用于仓库本地的 SeekDeep 与 Cordis 包标识。
