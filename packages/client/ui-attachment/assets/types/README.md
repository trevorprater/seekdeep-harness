# UI-attachment compatibility declarations

English | [中文](README.zh.md)

Each `*.d.ts.txt` file is a verbatim declaration emitted by the pinned source package. The Rust/WASM package builder removes the final `.txt`, applies the required package-identity substitutions, and writes the six-file declaration tree under `lib/types/`.

The storage suffix keeps declaration metadata outside the executable foreign-language surface guarded by `cargo xtask parity`. Refresh the complete tree together from the pinned oracle.
