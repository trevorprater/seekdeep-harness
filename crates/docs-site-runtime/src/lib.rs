//! Markdown interpolation, canonical edit links, and owned sidebar scroll behavior.

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::{
    SidebarScrollbar, edit_link, edit_link_pattern, should_project, validate_markdown_rules,
};

/// Escapes Vue interpolation delimiters in rendered text and inline code.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen(js_name = escapeVueInterpolation))]
pub fn escape_vue_interpolation(html: &str) -> String {
    html.replace("{{", "&#123;&#123;")
        .replace("}}", "&#125;&#125;")
}

#[cfg(test)]
mod tests {
    #[test]
    fn rendered_tags_remain_intact_while_interpolations_become_text() {
        assert_eq!(
            super::escape_vue_interpolation("<code>{{value}}</code> {x} {{{{ }}}}"),
            "<code>&#123;&#123;value&#125;&#125;</code> {x} &#123;&#123;&#123;&#123; &#125;&#125;&#125;&#125;"
        );
    }
}
