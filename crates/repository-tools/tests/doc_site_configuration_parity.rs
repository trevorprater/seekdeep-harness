//! Materialized `VitePress` configuration and repository-layout contracts.

use std::{path::PathBuf, process::Command};

use seekdeep_repository_tools::doc_site::{
    DocsContentLocale, DocsLocale, DocsManifest, projected_page_content, site_configuration,
};
use serde_json::{Value, json};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

#[test]
fn every_materialized_locale_navigation_and_search_setting_matches_the_source() {
    let root = repository_root();
    let manifest = DocsManifest::read(&root.join("website/docs.json")).unwrap();
    let snapshot = std::fs::read_to_string(root.join("SOURCE_SNAPSHOT")).unwrap();
    let oracle = PathBuf::from(
        snapshot
            .lines()
            .find_map(|line| line.strip_prefix("repository="))
            .unwrap(),
    );
    let original = std::fs::read_to_string(oracle.join("website/.vitepress/config.ts")).unwrap();
    let source = original
        .lines()
        .filter(|line| {
            !line.starts_with("import { withMermaid }")
                && !line.starts_with("import { docsSourceFiles,")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .replace(
            "from '../docs.ts'",
            &format!("from {}", json!(oracle.join("website/docs.ts"))),
        )
        .replace("projectDocs()", "void 0")
        .replace(
            "resolve(import.meta.dirname, '../public/wordmark.svg')",
            &json!(root.join("website/public/wordmark.svg")).to_string(),
        );
    let directory = tempfile::tempdir().unwrap();
    let module = directory.path().join("config.mts");
    std::fs::write(
        &module,
        format!(
            "const withMermaid = value => value;\nconst docsSourceFiles = () => [];\n{source}\n"
        ),
    )
    .unwrap();
    let output = Command::new("node")
        .args([
            "--input-type=module",
            "-e",
            "const m=await import(process.argv[1]);console.log(JSON.stringify(m.default));",
        ])
        .arg(&module)
        .env("DOCS_BASE", "/preview/")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut expected: Value = serde_json::from_str(
        &String::from_utf8(output.stdout)
            .unwrap()
            .replace("DeepSeek Harness", "SeekDeep Harness")
            .replace(
                "deepseek-ai/deepseek-harness",
                "trevorprater/seekdeep-harness",
            )
            .replace("dsh-", "seekdeep-"),
    )
    .unwrap();
    expected.as_object_mut().unwrap().remove("markdown");
    expected["vite"].as_object_mut().unwrap().remove("plugins");
    expected["vite"]["publicDir"] = json!(root.join("website/.cache/public"));
    expected["head"][1][2] = json!(format!(
        "{}\n",
        expected["head"][1][2].as_str().unwrap().trim()
    ));
    expected["head"][2] =
        json!(["script", {"type":"module", "src":"/preview/_seekdeep/docs-site.mjs"}]);
    assert_eq!(
        site_configuration(&root, &manifest, "/preview/").unwrap(),
        expected
    );
}

#[test]
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the source checks literal lowercase Markdown extensions"
)]
fn source_layout_and_locale_content_remain_canonical() {
    let root = repository_root();
    let output = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "website",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(output.status.success());
    let copies = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|file| {
            file.ends_with(".md") && *file != "website/AGENTS.md" && root.join(file).exists()
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(
        copies.is_empty(),
        "Canonical Markdown must remain outside website/: {copies:?}"
    );
    let manifest = DocsManifest::read(&root.join("website/docs.json")).unwrap();
    for page in manifest.pages.iter().filter(|page| page.sidebar.is_none()) {
        let source = std::fs::read_to_string(root.join(&page.source)).unwrap();
        let projected = projected_page_content(&source, page).unwrap();
        assert!(projected.contains("layout: false"));
        assert!(projected.contains("http-equiv: refresh"));
        assert!(projected.contains("content: 0; url=./guide/quickstart"));
        assert!(!projected.contains("# SeekDeep Harness"));
    }
    for page in manifest
        .pages
        .iter()
        .filter(|page| page.locale == DocsLocale::Root)
    {
        let other = manifest
            .pages
            .iter()
            .find(|other| other.route == format!("en/{}", page.route))
            .unwrap();
        if page.content_locale == DocsContentLocale::ZhCn {
            assert_eq!(other.source, page.source.replace(".zh.md", ".md"));
            assert_eq!(other.content_locale, DocsContentLocale::EnUs);
        } else {
            assert_eq!(page.source, other.source);
            assert!(!root.join(page.source.replace(".md", ".zh.md")).exists());
        }
    }
    let subsystem_pages = std::fs::read_dir(root.join("docs/subsystems"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|file| file.ends_with(".md") && !file.ends_with(".zh.md") && file != "README.md")
        .collect::<Vec<_>>();
    assert!(!subsystem_pages.is_empty());
    for readme in ["README.md", "README.zh.md"] {
        let index = std::fs::read_to_string(root.join("docs/subsystems").join(readme)).unwrap();
        for page in &subsystem_pages {
            assert!(
                index.contains(&format!("| [{page}]({page}) |")),
                "{readme}: {page}"
            );
        }
    }
}
