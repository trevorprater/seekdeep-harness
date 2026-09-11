//! Structural release fixtures; relocated execution uses the actual compiled package separately.

use std::{fs, io::Write as _, path::Path};

use seekdeep_code_runtime_worker_thread::node_assets;
use seekdeep_python_release::{
    executable::{Arch, Platform, Target},
    node_runtime::{NodeDistribution, NodeProvenance},
};
use serde_json::json;
use zip::write::SimpleFileOptions;

pub(crate) fn native_header(target: &Target, library: bool) -> [u8; 32] {
    let mut header = [0_u8; 32];
    match target.platform() {
        Platform::Linux => {
            header[..4].copy_from_slice(b"\x7fELF");
            header[4] = 2;
            header[5] = 1;
            header[16..18].copy_from_slice(&(if library { 3_u16 } else { 2 }).to_le_bytes());
            header[18..20].copy_from_slice(
                &(match target.arch() {
                    Arch::X64 => 62_u16,
                    Arch::Arm64 => 183,
                })
                .to_le_bytes(),
            );
        }
        Platform::Macos => {
            header[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
            header[4..8].copy_from_slice(
                &(match target.arch() {
                    Arch::X64 => 0x0100_0007_u32,
                    Arch::Arm64 => 0x0100_000c,
                })
                .to_le_bytes(),
            );
            header[12..16].copy_from_slice(&(if library { 6_u32 } else { 2 }).to_le_bytes());
        }
    }
    header
}

pub(crate) fn make_assets(directory: &Path, target: &Target) {
    if directory.exists() {
        fs::remove_dir_all(directory).unwrap();
    }
    fs::create_dir_all(directory.join("bin")).unwrap();
    fs::create_dir_all(directory.join("snippets/compiled-fixture")).unwrap();
    for path in [
        "loader.mjs",
        "plugin-loader.cjs",
        "wasm-runtime.cjs",
        "seekdeep_code_runtime_node.js",
        "snippets/compiled-fixture/inline0.js",
    ] {
        fs::write(directory.join(path), "// structural fixture\n").unwrap();
    }
    fs::write(
        directory.join("seekdeep_code_runtime_node_bg.wasm"),
        b"\0asm\x01\0\0\0",
    )
    .unwrap();
    fs::write(directory.join("package.json"), "{\"type\":\"commonjs\"}\n").unwrap();
    fs::write(
        directory.join("snippets/package.json"),
        "{\"type\":\"module\"}\n",
    )
    .unwrap();
    for (name, version) in node_assets::DEPENDENCIES {
        let package = directory.join("node_modules").join(name);
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join("package.json"),
            json!({"name":name,"version":version,"main":"index.js"}).to_string(),
        )
        .unwrap();
        fs::write(package.join("index.js"), "module.exports = {};\n").unwrap();
        fs::write(package.join("LICENSE"), "fixture dependency license\n").unwrap();
    }
    fs::write(directory.join("bin/node"), native_header(target, false)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(
            directory.join("bin/node"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    fs::write(
        directory.join("bin/LICENSE"),
        "fixture upstream Node license\n",
    )
    .unwrap();
    let descriptor = NodeDistribution::for_version("v24.0.0", target).unwrap();
    let provenance = NodeProvenance {
        version: descriptor.version.clone(),
        target: target.platform_arch(),
        archive: descriptor.archive.clone(),
        archive_sha256: "0".repeat(64),
        url: descriptor.url(),
        executable: "bin/node".to_owned(),
        license: "bin/LICENSE".to_owned(),
    };
    node_assets::write_manifest_with_node(directory, serde_json::to_value(provenance).unwrap())
        .unwrap();
}

pub(crate) fn zip_assets(archive: &mut zip::ZipWriter<fs::File>, directory: &Path) {
    let mut pending = vec![directory.to_owned()];
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    for file in files {
        let relative = file.strip_prefix(directory).unwrap().to_str().unwrap();
        let mode = if relative == "bin/node" { 0o755 } else { 0o644 };
        archive
            .start_file(
                format!("deepseek_harness_runtime/runtime/code-runtime-node/{relative}"),
                SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .unix_permissions(mode),
            )
            .unwrap();
        archive.write_all(&fs::read(file).unwrap()).unwrap();
    }
}
