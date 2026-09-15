# `seekdeep-host-frontend-static`

[English](README.md) | 中文

Web 壳的 SPA dist 服务器：一个配置为 `{ distIndex }` 的插件，占据 webserver 的唯一回退席位，并按壳层锁定的语义服务已构建的前端目录。越出 dist 根目录的遍历返回 403，任何未命中项都以 HTTP 200 回退到 `index.html`，未知扩展名按 `application/octet-stream` 提供，GET／HEAD 之外的方法在没有匹配的具名路由时返回 405。每个 index 响应都会经过 webserver 已注册的 index 转换器（`apply_index_taps`），启动 manifest（元数据清单）就是经这条路径送达页面的。`distIndex` 是组合应用提供的组装事实；部署不会硬编码它。

回退席位只有单一所有者，并受 effect 作用域约束。释放插件 fiber 会释放席位，此后无人占据的 webserver 回答 404。

## 模型体验

无。该 crate 只服务浏览器资产；其中没有任何内容会进入模型请求。

#### KV cache 影响

无；该 crate 既不组装也不发送提供方请求。

## 已知限制与延期工作

- **初始 MIME 表很精简**：它覆盖实际输出的资产集合及交付的 PWA manifest；其他扩展名在相应资产类别实际发布前都会回退到 `application/octet-stream`。
