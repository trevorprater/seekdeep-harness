# KaTeX 浏览器资源

[English](README.md) | 中文

这些文件逐字复制自固定版本源包所使用的 `katex@0.16.47` 依赖。Rust/WASM 包构建器将 `katex.min.css`、其 `fonts/` 目录及上游 MIT `LICENSE` 投影到 `lib/katex/`，使编译后的 Markdown 渲染器拥有与源 CSS 导入相同的视觉及无障碍资源。

样式表、字体、许可证及依赖版本须一同更新；不要就地编辑生成的字体或 CSS 文件。
