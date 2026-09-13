# UI-primitives compatibility declarations

English | [中文](README.zh.md)

Each `*.d.ts.txt` file is a verbatim declaration emitted by the pinned source package. The Rust/WASM package builder removes the final `.txt`, applies the required DeepSeek Harness → SeekDeep Harness package-name substitution, and writes the resulting declaration tree under `lib/types/`.

The `.txt` storage suffix keeps declaration metadata distinct from executable foreign-language sources under the repository-wide Rust-only gate. Refresh the complete tree together from the pinned oracle; do not edit individual declarations in place.
