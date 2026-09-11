//! Source-differential documentation routing, Markdown preservation, and asset ownership.

use std::{
    collections::BTreeMap,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use seekdeep_repository_tools::doc_site::{
    DocsContentLocale, DocsLocale, DocsManifest, DocsPage, DocsSection, DocsSidebar,
    RewriteOptions, add_projection_frontmatter, docs_source_files, project_docs,
    projected_page_content, publishable_image, rewrite_markdown, route_link,
};
use serde_json::{Value, json};
use tempfile::TempDir;

fn page(source: &str, route: &str, locale: DocsLocale) -> DocsPage {
    DocsPage {
        locale,
        content_locale: DocsContentLocale::EnUs,
        source: source.to_owned(),
        route: route.to_owned(),
        label: source.to_owned(),
        sidebar: Some(if locale == DocsLocale::Root {
            DocsSidebar::ZhReference
        } else {
            DocsSidebar::EnReference
        }),
        section: "Test".to_owned(),
        order: 0.0,
        outline: None,
        source_aliases: Vec::new(),
    }
}

fn fixture() -> (TempDir, DocsManifest) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::create_dir_all(root.join("packages")).unwrap();
    for (path, data) in [
        ("docs/a.md", "# A\n"),
        ("docs/a.zh.md", "# 中文\n"),
        ("docs/b.md", "# B\n"),
        ("docs/x(y).md", "# Parentheses\n"),
        ("docs/a b.md", "# Space\n"),
        ("packages/tool.ts", "one\ntwo\n"),
        ("packages/logo.svg", "<svg/>\n"),
        ("packages/图 image.png", "image bytes"),
    ] {
        std::fs::write(root.join(path), data).unwrap();
    }
    let pages = vec![
        page("docs/a.md", "a.md", DocsLocale::Root),
        page("docs/b.md", "reference-root/b.md", DocsLocale::Root),
        page("docs/a.md", "en/a.md", DocsLocale::En),
        page("docs/b.md", "en/reference/b.md", DocsLocale::En),
    ];
    let sections = [DocsLocale::Root, DocsLocale::En]
        .into_iter()
        .map(|locale| {
            (
                locale,
                vec![DocsSection {
                    label: "Test".to_owned(),
                    collapsed: None,
                }],
            )
        })
        .collect();
    (
        directory,
        DocsManifest {
            source_commit: "fixture".to_owned(),
            pages,
            sections,
        },
    )
}

fn options<'a>(root: &'a Path, pages: &'a [DocsPage]) -> RewriteOptions<'a> {
    RewriteOptions {
        locale: DocsLocale::En,
        source_path: "docs/a.md",
        route: "en/a.md",
        pages,
        repo_root: root,
        repository_ref: "abc123",
    }
}

fn source_root() -> PathBuf {
    let snapshot = include_str!("../../../SOURCE_SNAPSHOT");
    PathBuf::from(
        snapshot
            .lines()
            .find_map(|line| line.strip_prefix("repository="))
            .unwrap(),
    )
}

fn source_call(input: &Value) -> Value {
    let script = r"
import { pathToFileURL } from 'node:url';
import { basename } from 'node:path';
const source = await import(pathToFileURL(process.argv[1]).href);
let body = ''; for await (const chunk of process.stdin) body += chunk;
const input = JSON.parse(body);
try {
  const options = input.options;
  if (input.place) options.placeImage = path => './' + basename(path);
  const value = input.operation === 'rewrite' ? source.rewriteMarkdown(input.text, options)
    : input.operation === 'frontmatter' ? source.addProjectionFrontmatter(input.text, input.page)
    : source.projectedPageContent(input.text, input.page);
  console.log(JSON.stringify({ value }));
} catch (error) { console.log(JSON.stringify({ error: error.message })); }
";
    let mut child = Command::new("node")
        .args(["--input-type=module", "-e", script])
        .arg(source_root().join("scripts/project-doc-site.ts"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(input).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let normalized = String::from_utf8(output.stdout).unwrap().replace(
        "deepseek-ai/deepseek-harness",
        "trevorprater/seekdeep-harness",
    );
    serde_json::from_str(&normalized).unwrap()
}

#[test]
fn source_differential_preserves_destinations_and_every_other_byte() {
    let (directory, manifest) = fixture();
    let root = directory.path();
    let options = options(root, &manifest.pages);
    for text in [
        "[B](b.md#part) [source](../packages/tool.ts:2) [web](https://example.com)\n",
        "[B](b?query=1#part) [code](../packages/tool.ts)\n",
        "[rounded](../packages/tool.ts:9007199254740993) [large](../packages/tool.ts:10000000000000000000000000000)\n",
        "```md\n[B](b.md)\n```\n`[B](b.md)`\n",
        "[title](b.md \"b.md\") [escaped](x\\(y\\).md)\n",
        "中文 🦀 [B](b.md) [space](<a b.md> 'title')\n",
        "[label [nested]](b.md)\n[ref]: <b.md> \"same b.md\"\n[B][ref]\n",
        "![logo](../packages/logo.svg#view)\n",
        "[anchor](#same) [site](/guide/) [email](mailto:test@example.com)\n",
        "[broken](missing.md)\n",
        "[bad percent](bad%ZZ.md)\n",
    ].into_iter().map(str::to_owned).chain([format!("[overflow](../packages/tool.ts:{})\n", "9".repeat(350))]) {
        let expected = source_call(
            &json!({ "operation": "rewrite", "text": text, "options": { "locale": "en", "sourcePath": "docs/a.md", "route": "en/a.md", "pages": manifest.pages, "repoRoot": root, "repositoryRef": "abc123" } }),
        );
        match rewrite_markdown(&text, &options, None) {
            Ok(value) => assert_eq!(json!({"value":value}), expected, "{text}"),
            Err(error) => assert_eq!(json!({"error":error.to_string()}), expected, "{text}"),
        }
    }
    let mut placed = Vec::new();
    let mut place = |path: &Path| {
        placed.push(path.to_owned());
        Ok(format!("./{}", path.file_name().unwrap().to_string_lossy()))
    };
    let text = "![logo](../packages/logo.svg?raw#view) [B](b.md)\n";
    let actual = rewrite_markdown(text, &options, Some(&mut place)).unwrap();
    let expected = source_call(
        &json!({ "operation":"rewrite", "text":text, "place":true, "options": {"locale":"en","sourcePath":"docs/a.md","route":"en/a.md","pages":manifest.pages,"repoRoot":root,"repositoryRef":"abc123"} }),
    );
    assert_eq!(expected["value"], actual);
    assert_eq!(placed, vec![root.join("packages/logo.svg")]);
}

#[test]
fn localized_links_switch_languages_only_for_the_current_pages_counterpart() {
    let (directory, mut manifest) = fixture();
    manifest.pages.retain(|page| page.source != "docs/a.md");
    let mut chinese = page("docs/a.zh.md", "guide/a.md", DocsLocale::Root);
    chinese.source_aliases.push("docs/a.md".to_owned());
    let mut english = page("docs/a.md", "en/guide/a.md", DocsLocale::En);
    english.source_aliases.push("docs/a.zh.md".to_owned());
    manifest.pages.extend([chinese, english]);
    let options = RewriteOptions {
        locale: DocsLocale::Root,
        source_path: "docs/a.zh.md",
        route: "guide/a.md",
        ..options(directory.path(), &manifest.pages)
    };
    assert_eq!(
        rewrite_markdown("[English](a.md) [B](b.md)\n", &options, None).unwrap(),
        "[English](../en/guide/a.md) [B](../reference-root/b.md)\n"
    );
    manifest.pages.push(manifest.pages[0].clone());
    assert!(
        rewrite_markdown(
            "text",
            &options_for_duplicate(directory.path(), &manifest),
            None
        )
        .unwrap_err()
        .to_string()
        .contains("duplicate source or alias")
    );
}

fn options_for_duplicate<'a>(root: &'a Path, manifest: &'a DocsManifest) -> RewriteOptions<'a> {
    options(root, &manifest.pages)
}

#[test]
fn manifest_rejects_unknown_locale_collections_and_missing_home_classification() {
    let value = serde_json::to_value(page("docs/a.md", "a.md", DocsLocale::En)).unwrap();
    for (field, invalid) in [
        ("locale", json!("fr")),
        ("contentLocale", json!("fr-FR")),
        ("sidebar", json!("en-docs")),
        ("sideBar", json!("en-guide")),
    ] {
        let mut invalid_page = value.clone();
        invalid_page[field] = invalid;
        assert!(
            serde_json::from_value::<DocsPage>(invalid_page).is_err(),
            "{field}"
        );
    }
    let mut absent = value.clone();
    absent.as_object_mut().unwrap().remove("sidebar");
    assert!(serde_json::from_value::<DocsPage>(absent).is_err());
    let mut home = value;
    home["sidebar"] = Value::Null;
    assert!(
        serde_json::from_value::<DocsPage>(home)
            .unwrap()
            .sidebar
            .is_none()
    );
}

#[test]
fn projection_frontmatter_and_repository_chrome_match_the_source() {
    let ordinary = page("docs/a.md", "a.md", DocsLocale::En);
    let home = DocsPage {
        sidebar: None,
        ..ordinary.clone()
    };
    for (text, page) in [
        ("# Guide\n\nEnglish | [中文](a.zh.md)\n\nBody.\n", &ordinary),
        ("# 指南\n\n[English](a.md) | 中文\n\n正文。\n", &ordinary),
        (
            "# Guide\n\nBody.\n\n[![](https://img.shields.io/badge/powered_by-dsh-blue)](https://example.com)\n",
            &ordinary,
        ),
        (
            "# Guide\n\nA\n\nB\n\nC\n\nD\n\nEnglish | [中文](a.zh.md)\n",
            &ordinary,
        ),
        ("---\nlayout: false\n---\n\n# Hidden body\n", &home),
        ("# Missing frontmatter\n", &home),
        ("---\nunclosed: true\n", &home),
    ] {
        let expected = source_call(&json!({"operation":"content","text":text,"page":page}));
        match projected_page_content(text, page) {
            Ok(value) => assert_eq!(json!({"value":value}), expected),
            Err(error) => assert_eq!(json!({"error":error.to_string()}), expected),
        }
    }
    let outlined = DocsPage {
        outline: Some(json!([2.0, 4.0])),
        ..ordinary
    };
    for text in ["# Guide\n", "---\nlayout: home\n---\n"] {
        let expected = source_call(&json!({"operation":"frontmatter","text":text,"page":outlined}));
        assert_eq!(
            expected["value"],
            add_projection_frontmatter(text, &outlined)
        );
    }
}

#[test]
fn projection_owns_local_images_and_refuses_collisions_and_escaping_images() {
    let (directory, mut manifest) = fixture();
    let root = directory.path().canonicalize().unwrap();
    std::fs::write(
        root.join("docs/a.md"),
        "# A\n\n![one](../packages/logo.svg#view) ![two](<../packages/图 image.png>)\n",
    )
    .unwrap();
    let output = root.join("website/.generated");
    let report = project_docs(&root, &output, &manifest, "revision").unwrap();
    assert_eq!((report.pages, report.images), (4, 4));
    assert_eq!(
        std::fs::read(output.join("en/logo.svg")).unwrap(),
        b"<svg/>\n"
    );
    assert!(
        std::fs::read_to_string(output.join("en/a.md"))
            .unwrap()
            .contains("./%E5%9B%BE%20image.png")
    );
    let watched = docs_source_files(&root, &manifest).unwrap();
    assert_eq!(watched.len(), 4);
    assert!(watched.contains(&root.join("packages/logo.svg")));
    assert_eq!(
        publishable_image(&root.join("packages"), &root).unwrap(),
        None
    );
    std::fs::create_dir_all(root.join("other")).unwrap();
    std::fs::write(root.join("other/logo.svg"), "different").unwrap();
    std::fs::write(
        root.join("docs/a.md"),
        "![one](../packages/logo.svg) ![two](../other/logo.svg)\n",
    )
    .unwrap();
    assert!(
        project_docs(&root, &output, &manifest, "revision")
            .unwrap_err()
            .to_string()
            .contains("both project to")
    );
    manifest.pages.push(manifest.pages[0].clone());
    std::fs::write(root.join("docs/a.md"), "# A\n").unwrap();
    assert!(
        project_docs(&root, &output, &manifest, "revision")
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.png"), "outside").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.png"),
            root.join("packages/escape.png"),
        )
        .unwrap();
        assert_eq!(
            publishable_image(&root.join("packages/escape.png"), &root).unwrap(),
            None
        );
    }
}

#[test]
fn canonical_publication_manifest_preserves_source_routes_and_navigation() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = DocsManifest::read(&root.join("website/docs.json")).unwrap();
    assert_eq!(manifest.pages.len(), 166);
    let mut slots = BTreeMap::new();
    for page in &manifest.pages {
        assert!(root.join(&page.source).is_file(), "{}", page.source);
        if page.sidebar.is_some() {
            manifest.section_spec(page.locale, &page.section).unwrap();
        }
        assert!(
            slots
                .insert(
                    (
                        page.locale,
                        page.sidebar,
                        page.section.clone(),
                        if page.order == 0.0 {
                            "0".to_owned()
                        } else {
                            page.order.to_string()
                        }
                    ),
                    &page.route
                )
                .is_none()
        );
    }
    assert_eq!(
        manifest.landing_link(DocsLocale::En, "en-guide").unwrap(),
        "/en/guide/quickstart"
    );
    assert_eq!(route_link("en/index.md"), "/en/");
    assert!(manifest.section_spec(DocsLocale::En, "入门").is_err());
    assert!(manifest.landing_link(DocsLocale::Root, "missing").is_err());
    let directory = tempfile::tempdir().unwrap();
    let source = std::fs::read_to_string(source_root().join("website/docs.ts"))
        .unwrap()
        .replace("const sections:", "export const sections:");
    let module = directory.path().join("manifest.mts");
    std::fs::write(&module, source).unwrap();
    let script = "const m=await import(process.argv[1]);console.log(JSON.stringify({pages:m.docsPages,sections:m.sections}));";
    let result = Command::new("node")
        .args(["--input-type=module", "-e", script])
        .arg(&module)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let expected: Value = serde_json::from_str(
        &String::from_utf8(result.stdout)
            .unwrap()
            .replace("DeepSeek Harness", "SeekDeep Harness"),
    )
    .unwrap();
    let expected_pages: Vec<DocsPage> = serde_json::from_value(expected["pages"].clone()).unwrap();
    assert_eq!(manifest.pages, expected_pages);
    assert_eq!(
        serde_json::to_value(manifest.sections).unwrap(),
        expected["sections"]
    );
}
