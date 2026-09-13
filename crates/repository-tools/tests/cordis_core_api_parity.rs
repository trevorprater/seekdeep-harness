//! The generated Cordis core API reference: the five pages rendered from the
//! pinned vendor declarations equal the committed port pages, and an
//! undocumented public class is rejected.

use std::path::Path;

use seekdeep_repository_tools::cordis_core_api::{
    CORDIS_CORE_API_PAGES, CordisCoreApiPage, CordisCoreApiSection, render_cordis_core_api_page,
    render_cordis_core_api_pages,
};

fn source_root() -> std::path::PathBuf {
    std::env::var_os("SEEKDEEP_PARITY_SOURCE").map_or_else(
        || std::path::PathBuf::from("/Users/trevor/ws/deepseek-harness"),
        Into::into,
    )
}

#[test]
fn renders_the_five_detailed_pages_from_pinned_vendor_declarations() {
    let pages = render_cordis_core_api_pages(&source_root()).unwrap();
    assert_eq!(
        pages.keys().map(String::as_str).collect::<Vec<_>>(),
        CORDIS_CORE_API_PAGES
            .iter()
            .map(|page| page.out)
            .collect::<Vec<_>>()
    );
    assert!(pages["docs/cordis-api/context.md"].contains("### ctx.extend(meta?)"));
    assert!(pages["docs/cordis-api/events.md"].contains("## DispatchMode"));
    assert!(pages["docs/cordis-api/fiber.md"].contains("## EffectMeta"));
    assert!(pages["docs/cordis-api/registry.md"].contains("## Plugin"));
    assert!(pages["docs/cordis-api/service.md"].contains("### Service.resolveConfig"));
    let fiber = &pages["docs/cordis-api/fiber.md"];
    assert!(fiber.contains("```\n\nRegister a cleanup-aware effect on this fiber."));
    assert!(fiber.contains("- `execute` — the effect body; see `Effect` for accepted shapes."));
    assert!(
        fiber.contains("**Returns** a disposer that tears the effect down and settles once done.")
    );
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (out, rendered) in &pages {
        let committed = std::fs::read_to_string(repository.join(out)).unwrap();
        assert!(
            rendered == &committed,
            "{out} differs from the committed page"
        );
    }
}

#[test]
fn rejects_a_public_core_class_without_source_jsdoc() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("vendor/cordis/src")).unwrap();
    std::fs::write(
        root.path().join("vendor/cordis/src/service.ts"),
        "export class Service {\n  run(): string { return \"ok\" }\n}\n",
    )
    .unwrap();
    let page = CordisCoreApiPage {
        out: "docs/cordis-api/service.md",
        title: "Service",
        intro: "Service API.",
        sections: &[CordisCoreApiSection::Class {
            file: "vendor/cordis/src/service.ts",
            symbol: "Service",
            prefix: None,
            heading: None,
        }],
    };
    let error = render_cordis_core_api_page(&page, root.path()).unwrap_err();
    assert!(error.to_string().contains("class Service"), "{error}");
}
