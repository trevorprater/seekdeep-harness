# KaTeX browser assets

These files are copied verbatim from the `katex@0.16.47` dependency used by the pinned source package. The Rust/WASM package builder projects `katex.min.css`, its `fonts/` directory, and the upstream MIT `LICENSE` into `lib/katex/` so the compiled Markdown renderer has the same visual and accessibility assets as the source CSS import.

Update the stylesheet, fonts, license, and dependency version together; do not edit generated font or CSS files in place.
