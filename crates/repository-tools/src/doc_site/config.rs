use std::path::Path;

use serde_json::{Value, json};

use super::{DocsLocale, DocsManifest, route_link};

const REPOSITORY: &str = "https://github.com/trevorprater/seekdeep-harness";

/// Builds the locale, navigation, search, and presentation data consumed by `VitePress`.
///
/// # Errors
/// Rejects missing wordmark assets, undeclared sections, and empty navigation collections.
pub fn site_configuration(
    root: &Path,
    manifest: &DocsManifest,
    base: &str,
) -> anyhow::Result<Value> {
    let wordmark = std::fs::read_to_string(root.join("website/public/wordmark.svg"))?
        .trim()
        .replacen("<svg ", "<svg class=\"seekdeep-wordmark\" ", 1);
    let title = |tag: &str| {
        format!(
            "<span class=\"seekdeep-lockup\">{wordmark}<span class=\"seekdeep-tag\">{tag}</span></span>"
        )
    };
    let root_modules = modules(DocsLocale::Root);
    let en_modules = modules(DocsLocale::En);
    Ok(json!({
        "title": "SeekDeep Harness",
        "description": "用于构建 Agent Harness 的插件化 SDK",
        "base": base,
        "head": [
            ["link", {"rel":"icon","type":"image/svg+xml","href":format!("{base}favicon.svg")}],
            ["style", {}, include_str!("../../../../website/site.css")],
            ["script", {"type":"module","src":format!("{base}_seekdeep/docs-site.mjs")}]
        ],
        "cleanUrls": true,
        "srcDir": ".generated",
        "cacheDir": ".cache",
        "outDir": ".dist",
        "locales": {
            "root": {
                "label":"简体中文", "lang":"zh-CN",
                "themeConfig": {
                    "siteTitle": title("技术预览"),
                    "nav": navigation(manifest, DocsLocale::Root)?,
                    "sidebar": {
                        "/guide/": guide_sidebar(manifest, DocsLocale::Root)?,
                        "/develop/": sidebar(manifest, DocsLocale::Root, root_modules.develop.1)?,
                        "/reference/": sidebar(manifest, DocsLocale::Root, root_modules.reference.1)?
                    },
                    "outline": {"label":"本页目录"},
                    "docFooter": {"prev":"上一篇", "next":"下一篇"},
                    "darkModeSwitchLabel":"外观",
                    "lightModeSwitchTitle":"切换到浅色主题",
                    "darkModeSwitchTitle":"切换到深色主题",
                    "sidebarMenuLabel":"菜单",
                    "returnToTopLabel":"返回顶部",
                    "langMenuLabel":"切换语言",
                    "skipToContentLabel":"跳至内容"
                }
            },
            "en": {
                "label":"English", "lang":"en-US", "link":"/en/",
                "themeConfig": {
                    "siteTitle":title("Preview"),
                    "nav":navigation(manifest, DocsLocale::En)?,
                    "sidebar": {
                        "/en/guide/":guide_sidebar(manifest, DocsLocale::En)?,
                        "/en/develop/":sidebar(manifest, DocsLocale::En, en_modules.develop.1)?,
                        "/en/reference/":sidebar(manifest, DocsLocale::En, en_modules.reference.1)?
                    },
                    "editLink":{"text":"Edit this page on GitHub"},
                    "outline":{"label":"On this page"},
                    "docFooter":{"prev":"Previous", "next":"Next"}
                }
            }
        },
        "vite": {"publicDir":root.join("website/.cache/public")},
        "mermaid": {},
        "themeConfig": {
            "search": {"provider":"local", "options":{"locales":{"root":{"translations":{
                "button":{"buttonText":"搜索文档","buttonAriaLabel":"搜索文档"},
                "modal":{
                    "displayDetails":"显示详细列表", "resetButtonTitle":"清除搜索", "backButtonTitle":"关闭搜索",
                    "noResultsText":"未找到相关结果",
                    "footer":{
                        "selectText":"选择", "selectKeyAriaLabel":"回车键", "navigateText":"切换",
                        "navigateUpKeyAriaLabel":"上方向键", "navigateDownKeyAriaLabel":"下方向键",
                        "closeText":"关闭", "closeKeyAriaLabel":"Esc 键"
                    }
                }
            }}}}},
            "socialLinks":[{"icon":"github","link":REPOSITORY}],
            "editLink":{"text":"在 GitHub 上编辑此页"}
        }
    }))
}

struct GuideModules {
    guide: (&'static str, &'static str),
    develop: (&'static str, &'static str),
    reference: (&'static str, &'static str),
    prefix: &'static str,
}

const fn modules(locale: DocsLocale) -> GuideModules {
    match locale {
        DocsLocale::Root => GuideModules {
            guide: ("入门", "zh-guide"),
            develop: ("开发", "zh-develop"),
            reference: ("参考", "zh-reference"),
            prefix: "",
        },
        DocsLocale::En => GuideModules {
            guide: ("Guide", "en-guide"),
            develop: ("Development", "en-develop"),
            reference: ("Reference", "en-reference"),
            prefix: "/en",
        },
    }
}

fn navigation(manifest: &DocsManifest, locale: DocsLocale) -> anyhow::Result<Vec<Value>> {
    let modules = modules(locale);
    [
        (modules.guide, "guide"),
        (modules.develop, "develop"),
        (modules.reference, "reference"),
    ]
    .into_iter()
    .map(|((text, collection), route)| {
        Ok(json!({
            "text":text, "link":manifest.landing_link(locale, collection)?,
            "activeMatch":format!("^{}/{route}/", modules.prefix)
        }))
    })
    .collect()
}

fn guide_sidebar(manifest: &DocsManifest, locale: DocsLocale) -> anyhow::Result<Vec<Value>> {
    let modules = modules(locale);
    let mut groups = sidebar(manifest, locale, modules.guide.1)?;
    for (text, collection) in [modules.develop, modules.reference] {
        groups.push(json!({"text":text, "link":manifest.landing_link(locale, collection)?}));
    }
    Ok(groups)
}

fn sidebar(
    manifest: &DocsManifest,
    locale: DocsLocale,
    collection: &str,
) -> anyhow::Result<Vec<Value>> {
    let mut sections = indexmap::IndexMap::<&str, Vec<Value>>::new();
    for page in manifest.ordered_pages(locale, collection)? {
        sections
            .entry(&page.section)
            .or_default()
            .push(json!({"text":page.label, "link":route_link(&page.route)}));
    }
    sections
        .into_iter()
        .map(|(text, items)| {
            let mut group = json!({"text":text, "items":items});
            if let Some(collapsed) = manifest.section_spec(locale, text)?.1.collapsed {
                group["collapsed"] = json!(collapsed);
            }
            Ok(group)
        })
        .collect()
}
