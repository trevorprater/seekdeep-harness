//! Repository-wide scrollbar token, rendering-path, width, and rebind contract.

#![cfg(not(target_arch = "wasm32"))]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use regex::Regex;
use seekdeep_client_ui_theme::{DESIGN_PLATFORM_STYLES, SCROLLBAR_STYLES};

fn css_files(root: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| matches!(name, "lib" | "node_modules"))
            {
                continue;
            }
            css_files(&path, output);
        } else if path.extension().and_then(|value| value.to_str()) == Some("css") {
            output.push(path);
        }
    }
}

fn without_comments(css: &str) -> String {
    Regex::new(r"/\*[\s\S]*?\*/")
        .unwrap()
        .replace_all(css, " ")
        .into_owned()
}

fn block(css: &str, marker: &str) -> Option<(usize, usize)> {
    let start = css.find(marker)?;
    let open = start + css[start..].find('{')?;
    let mut depth = 0_u32;
    for (offset, byte) in css.as_bytes()[open..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open, open + offset));
                }
            }
            _ => {}
        }
    }
    None
}

#[test]
#[allow(clippy::too_many_lines)]
fn scrollbar_contract_is_complete_across_every_package_stylesheet() {
    let token = Regex::new(r"(--dsw-alias-scrollbar-[a-z0-9-]+)\s*:").unwrap();
    let definitions = token
        .captures_iter(DESIGN_PLATFORM_STYLES)
        .map(|capture| capture[1].to_owned())
        .collect::<Vec<_>>();
    let names = definitions.iter().cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "--dsw-alias-scrollbar-bg-l1".to_owned(),
            "--dsw-alias-scrollbar-bg-l2".to_owned(),
            "--dsw-alias-scrollbar-hover-l1".to_owned(),
            "--dsw-alias-scrollbar-hover-l2".to_owned(),
        ]
        .into_iter()
        .collect()
    );
    for name in &names {
        assert_eq!(
            definitions
                .iter()
                .filter(|candidate| *candidate == name)
                .count(),
            2,
            "{name} must exist in both palette blocks"
        );
        assert!(
            DESIGN_PLATFORM_STYLES
                .lines()
                .filter(|line| line.contains(&format!("{name}:")))
                .all(|line| line.contains("var(--dsw-static-")),
            "{name} must resolve directly to the static scale"
        );
    }

    let scrollbar = without_comments(SCROLLBAR_STYLES);
    for exact in [
        "--seekdeep-scrollbar-thumb: var(--dsw-alias-scrollbar-bg-l1)",
        "--seekdeep-scrollbar-thumb-hover: var(--dsw-alias-scrollbar-hover-l1)",
        "--seekdeep-scrollbar-width: 8px",
        "scrollbar-color: var(--seekdeep-scrollbar-thumb) transparent",
        "background: var(--seekdeep-scrollbar-thumb)",
        "background: var(--seekdeep-scrollbar-thumb-hover)",
        "::-webkit-scrollbar-corner",
    ] {
        assert!(scrollbar.contains(exact), "missing {exact:?}");
    }
    let gate_name = "@supports not selector(::-webkit-scrollbar)";
    let (gate_start, gate_end) = block(&scrollbar, gate_name).expect("standard-path gate");
    for property in ["scrollbar-width:", "scrollbar-color:"] {
        let offsets = Regex::new(&format!(
            r"(?m)(^|[;{{\s]){}\s*:",
            regex::escape(property.trim_end_matches(':'))
        ))
        .unwrap()
        .find_iter(&scrollbar)
        .map(|matched| matched.start())
        .collect::<Vec<_>>();
        assert!(!offsets.is_empty(), "{property}");
        assert!(
            offsets
                .iter()
                .all(|offset| *offset > gate_start && *offset < gate_end),
            "{property} escaped the Firefox-only gate"
        );
    }
    assert!(scrollbar[gate_end..].contains("::-webkit-scrollbar {"));
    assert!(
        scrollbar
            .find("var(--seekdeep-scrollbar-thumb-hover)")
            .is_some_and(|offset| offset > gate_end)
    );
    assert!(scrollbar[gate_start..gate_end].contains("body *"));
    let width_variable = Regex::new(r"--seekdeep-scrollbar-width\s*:\s*([^;]+)")
        .unwrap()
        .captures(&scrollbar)
        .map(|capture| capture[1].trim().to_owned())
        .unwrap();
    let webkit_width = Regex::new(r"(?s)::-webkit-scrollbar\s*\{[^}]*width\s*:\s*([^;]+)")
        .unwrap()
        .captures(&scrollbar)
        .map(|capture| capture[1].trim().to_owned())
        .unwrap();
    assert_eq!(webkit_width, width_variable);

    let variable = Regex::new(r"(--[a-z0-9-]+)\s*:\s*([^;]+);").unwrap();
    let reference = Regex::new(r"var\(\s*(--[a-z0-9-]+)").unwrap();
    let definitions = variable
        .captures_iter(DESIGN_PLATFORM_STYLES)
        .map(|capture| (capture[1].to_owned(), capture[2].trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let resolve = |name: &str| {
        let mut current = name.to_owned();
        let mut seen = BTreeSet::new();
        while seen.insert(current.clone()) {
            let Some(value) = definitions.get(&current) else {
                break;
            };
            let Some(next) = reference
                .captures(value)
                .map(|capture| capture[1].to_owned())
            else {
                return value.clone();
            };
            current = next;
        }
        current
    };
    let elevated_rungs = [
        resolve("--dsw-alias-bg-layer-2"),
        resolve("--dsw-alias-bg-layer-3"),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let surface_family = Regex::new(r"^--dsw-(?:alias-bg-|specific-)").unwrap();
    let elevated_surfaces = definitions
        .keys()
        .filter(|name| surface_family.is_match(name))
        .filter(|name| elevated_rungs.contains(&resolve(name)))
        .cloned()
        .collect::<BTreeSet<_>>();
    for expected in [
        "--dsw-alias-bg-layer-2",
        "--dsw-alias-bg-layer-3",
        "--dsw-specific-menu",
        "--dsw-specific-input-major",
        "--dsw-specific-tip",
    ] {
        assert!(elevated_surfaces.contains(expected), "{expected}");
    }
    assert!(!elevated_surfaces.contains("--dsw-alias-bg-base"));
    assert!(!elevated_surfaces.contains("--dsw-alias-bg-layer-1"));

    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    let mut files = Vec::new();
    css_files(&workspace.join("packages"), &mut files);
    assert!(!files.is_empty());
    let mut all_css = String::new();
    let mut rebind_count = 0_u32;
    let rules = Regex::new(r"(?s)([^{}]+)\{([^{}]*)\}").unwrap();
    let overflow = Regex::new(r"overflow(?:-x|-y)?\s*:\s*[^;]*(?:auto|scroll)").unwrap();
    let background = Regex::new(r"background(?:-color)?\s*:\s*([^;]+)").unwrap();
    let thumb = Regex::new(r"--seekdeep-scrollbar-thumb\s*:\s*([^;]+)").unwrap();
    let hover = Regex::new(r"--seekdeep-scrollbar-thumb-hover\s*:\s*([^;]+)").unwrap();
    let value = |body: &str, pattern: &Regex| {
        pattern
            .captures(body)
            .map(|capture| capture[1].trim().to_owned())
    };
    for file in &files {
        let css = fs::read_to_string(file).unwrap();
        all_css.push_str(&css);
        if file.ends_with("client/ui-theme/src/styles/scrollbar.css") {
            continue;
        }
        let css = without_comments(&css);
        let scrolls = overflow.is_match(&css);
        let mut surfaces = BTreeSet::new();
        let mut rebinds_elevation = false;
        for capture in rules.captures_iter(&css) {
            let body = &capture[2];
            for background_match in background.captures_iter(body) {
                for token in reference.captures_iter(&background_match[1]) {
                    if elevated_surfaces.contains(&token[1]) {
                        surfaces.insert(token[1].to_owned());
                    }
                }
            }
            let Some(thumb) = value(body, &thumb) else {
                continue;
            };
            let hover = value(body, &hover)
                .unwrap_or_else(|| panic!("{} has an incomplete rebind", file.display()));
            rebind_count += 1;
            let hidden = thumb == "transparent" && hover == "transparent";
            let elevated = thumb == "var(--dsw-alias-scrollbar-bg-l2)"
                && hover == "var(--dsw-alias-scrollbar-hover-l2)";
            rebinds_elevation |= elevated;
            assert!(
                hidden || elevated,
                "{} mixes or misspells its rebind pair: {thumb}, {hover}",
                file.display()
            );
        }
        assert!(
            !scrolls || surfaces.is_empty() || rebinds_elevation,
            "{} scrolls on elevated surfaces without the l2 rebind: {:?}",
            file.display(),
            surfaces
        );
    }
    assert!(rebind_count > 0);
    for name in names {
        assert!(
            all_css.contains(&format!("var({name})")),
            "unconsumed {name}"
        );
    }
    assert!(all_css.contains("var(--seekdeep-scrollbar-width)"));
}
