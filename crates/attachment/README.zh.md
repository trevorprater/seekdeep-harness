# seekdeep-attachment

[English](README.md) | 中文

SeekDeep Harness 的持久附件服务边界。`AttachmentStore` 校验并持久提交不可变图片字节，
随后返回可序列化的 `ImageAttachmentRef`。消费方绝不会在会话事件中持久保存浏览器路径、
对象 URL、提供方 URL 或 base64 载荷。

`AttachmentId` 是内容寻址存储身份的不透明 Rust newtype。引用记录该 ID、已验证媒体类型、
精确编码字节数、固有尺寸，以及去除本地路径语义的可选显示名。第一版的封闭媒体枚举接受
PNG、JPEG、WebP 和 GIF。

未发送的输入区图片仍是浏览器拥有的临时草稿。`validate_image` 应用相同准入策略但不持久化；
批量写入方先验证所有成员，再保存任何成员，避免某一张非法图片使之前的图片成为无引用对象。
`save_image` 在追加其所属的模型可见会话事件之前，验证并提交每张已接受图片。
`read_image` 验证存储字节仍与日志引用匹配。

调用方可以向 `read_image` 传入 `AbortSignal`。实现在后端及验证工作边界观察取消，并保留
原始取消原因，而不是把它转换为存储错误。`AttachmentError` 携带稳定路由码、人类安全消息
和可选来源，同时不依赖 LLM crate，从而避免依赖环。

store 注册在类型化的 Cordis `attachments` 服务槽上；返回的 effect 拥有该注册并在卸载时
移除。具体后端拥有字节验证和不可变对象完整性；服务边界的 no-op invariant companion 只
保留重命名后的包身份，不虚构可变关系。

## 模型体验

该服务边界通过角色无关的核心图片块，以及解析其持久引用的提供方适配器间接影响模型。
添加图片会改变提供方请求，并使受影响的 KV cache 后缀失效。

## 已知限制

- 第一版只接受 PNG、JPEG、WebP 和 GIF。
- 保留策略和垃圾回收暂缓，因为恢复或 fork 后的会话可能共享不可变对象。
- 通用文件、音频、视频和持久未发送草稿需要单独的生命周期与提供方契约。
