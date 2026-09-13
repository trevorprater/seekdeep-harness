//! Static single-page application fallback for the Host Webserver.

use std::{
    future::Future,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use bytes::Bytes;
use hyper::{Method, StatusCode, header};
use percent_encoding::percent_decode_str;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_host_webserver::{WEB_SERVER, WebHandler, WebHandlerFuture, WebResponse, response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Package-owned invariant companion.
pub mod invariant;

pub use invariant::{INVARIANT_NAME, register_invariant};

/// Stable Cordis plugin name.
pub const NAME: &str = "frontend-static";
/// Webserver fallback ownership requires a live server.
pub const INJECT: &[&str] = &["webServer"];

/// Absolute SPA index anchor.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontendStaticConfig {
    /// Absolute path of `index.html` inside the distribution root.
    pub dist_index: PathBuf,
}

/// Serves one decoded GET/HEAD pathname from a distribution root.
///
/// Traversal is forbidden, `/` and the index path render through the supplied
/// tap pipeline, misses fall back to that index, and unknown extensions use
/// `application/octet-stream`.
///
/// # Errors
///
/// Returns index rendering and response-header construction failures.
pub async fn serve_static<Render, RenderFuture>(
    pathname: &str,
    dist_root: &Path,
    dist_index: &Path,
    render_index: Render,
) -> anyhow::Result<WebResponse>
where
    Render: FnOnce() -> RenderFuture,
    RenderFuture: Future<Output = anyhow::Result<String>>,
{
    let Some(target) = resolve_inside(dist_root, pathname) else {
        return Ok(response(StatusCode::FORBIDDEN, Bytes::new()));
    };
    if target == dist_root || target == dist_index {
        return index_response(render_index().await?);
    }
    // The lexical check above cannot see a symlink below the root that points outside it;
    // follow links before reading and refuse a target that leaves the resolved root.
    let Ok(resolved) = tokio::fs::canonicalize(&target).await else {
        return index_response(render_index().await?);
    };
    let resolved_root = tokio::fs::canonicalize(dist_root).await?;
    if !(resolved == resolved_root || resolved.starts_with(&resolved_root)) {
        return Ok(response(StatusCode::FORBIDDEN, Bytes::new()));
    }
    match tokio::fs::read(&resolved).await {
        Ok(body) => typed_response(
            StatusCode::OK,
            mime_for(target.extension().and_then(|extension| extension.to_str())),
            body,
            cache_control_for(&target),
        ),
        Err(_) => index_response(render_index().await?),
    }
}

/// Cache policy for one resolved file.
///
/// The bundler stamps a content hash into every asset it emits, so those bytes can never change
/// under the same URL and may be kept indefinitely. Everything else, including the rendered index
/// that carries the boot manifest, must be revalidated because its content names other revisions.
fn cache_control_for(target: &Path) -> &'static str {
    if content_hashed(target) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// Whether a file name carries a bundler content hash.
///
/// The bundler names every emitted asset `<name>-<hash><extensions>` with an eight-character
/// base64url hash that may itself contain `-` and `_` (`index-C8S-ni_M.js`,
/// `client_bg-D0L-57UV.wasm`), and a source map appends `.map` to its script's hashed name
/// (`index-DSWuNRsQ.js.map`), so the hash is the eight characters before the extensions rather
/// than whatever follows the last `-`.
fn content_hashed(target: &Path) -> bool {
    let Some(name) = target.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.match_indices('.').any(|(dot, _)| {
        let (stem, extensions) = name.split_at(dot);
        stem_carries_hash(stem)
            && extensions.split('.').skip(1).all(|extension| {
                !extension.is_empty() && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
    })
}

/// Whether `stem` ends in `-` followed by an eight-character base64url hash; a hash that happens
/// to be eight lowercase letters is indistinguishable from a plain word such as `manifest`, so it
/// is left revalidating rather than risking an immutable hand-named file.
fn stem_carries_hash(stem: &str) -> bool {
    const HASH_LENGTH: usize = 8;
    let Some(start) = stem.len().checked_sub(HASH_LENGTH) else {
        return false;
    };
    let (Some(prefix), Some(hash)) = (stem.get(..start), stem.get(start..)) else {
        return false;
    };
    prefix.ends_with('-')
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        && hash.bytes().any(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

fn resolve_inside(root: &Path, pathname: &str) -> Option<PathBuf> {
    let mut target = root.to_owned();
    let relative = pathname.strip_prefix('/').unwrap_or(pathname);
    for component in Path::new(relative).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(segment) => target.push(segment),
            Component::ParentDir => {
                if target == root || !target.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (target == root || target.starts_with(root)).then_some(target)
}

fn index_response(body: String) -> anyhow::Result<WebResponse> {
    typed_response(
        StatusCode::OK,
        "text/html; charset=utf-8",
        body.into_bytes(),
        "no-cache",
    )
}

fn typed_response(
    status: StatusCode,
    content_type: &str,
    body: impl Into<Bytes>,
    cache_control: &'static str,
) -> anyhow::Result<WebResponse> {
    let mut response = response(status, body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(content_type)?,
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static(cache_control),
    );
    Ok(response)
}

fn mime_for(extension: Option<&str>) -> &'static str {
    match extension {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("wasm") => "application/wasm",
        Some("json" | "map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

/// Builds the source-compatible static-frontend plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: FrontendStaticConfig = serde_json::from_value(config)?;
            install(&context, config)?;
            Ok(())
        })
    })
    .with_config_validator(|value: &Value| {
        let config: FrontendStaticConfig = serde_json::from_value(value.clone())?;
        anyhow::ensure!(
            !config.dist_index.as_os_str().is_empty(),
            "distIndex is required"
        );
        Ok(serde_json::to_value(config)?)
    })
}

/// Claims the fallback seat for one distribution.
///
/// # Errors
///
/// Returns missing-service, duplicate-seat, config, or lifecycle failures.
pub fn install(
    context: &Context,
    config: FrontendStaticConfig,
) -> anyhow::Result<seekdeep_cordis::fiber::EffectHandle> {
    anyhow::ensure!(
        !config.dist_index.as_os_str().is_empty(),
        "distIndex is required"
    );
    let webserver = context
        .get(WEB_SERVER)
        .ok_or_else(|| anyhow::anyhow!("frontend-static requires webServer"))?;
    let dist_index = config.dist_index;
    let dist_root = dist_index
        .parent()
        .ok_or_else(|| anyhow::anyhow!("distIndex has no parent directory"))?
        .to_owned();
    let handler: WebHandler = Arc::new(move |request| {
        let webserver = webserver.clone();
        let dist_root = dist_root.clone();
        let dist_index = dist_index.clone();
        Box::pin(async move {
            if request.method() != Method::GET && request.method() != Method::HEAD {
                return Ok(response(StatusCode::METHOD_NOT_ALLOWED, Bytes::new()));
            }
            let pathname = percent_decode_str(request.uri().path())
                .decode_utf8()?
                .into_owned();
            serve_static(&pathname, &dist_root, &dist_index, || async {
                let html = tokio::fs::read_to_string(&dist_index).await?;
                Ok(webserver.apply_index_taps(html))
            })
            .await
        }) as WebHandlerFuture
    });
    let registration = context
        .get(WEB_SERVER)
        .ok_or_else(|| anyhow::anyhow!("frontend-static lost webServer"))?
        .register_fallback(handler)?;
    registration.own(context, "frontend-static: fallback seat")
}

#[cfg(test)]
mod content_hashed_tests {
    use std::path::Path;

    use super::content_hashed;

    #[test]
    fn recognizes_bundler_hashes_including_those_containing_separators() {
        for name in [
            "index-C8S-ni_M.js",
            "client_bg-D0L-57UV.wasm",
            "wasm_bg-DN-vQADA.wasm",
            "index-DSWuNRsQ.js",
            "vendor-CrOf2xga.js.map",
            "katex.min-tQFonBqR.css",
            "index-NMDGHlFt.css",
        ] {
            assert!(
                content_hashed(Path::new(&format!("dist/assets/{name}"))),
                "{name} carries a content hash"
            );
        }
    }

    #[test]
    fn leaves_unhashed_names_revalidating() {
        for name in [
            "index.html",
            "favicon.svg",
            "manifest.webmanifest",
            "langs/en-US.json",
            "site-manifest.js",
            "index-abc.js",
            "index-C8S-ni_M",
            "index-C8S-ni_M.js~",
        ] {
            assert!(
                !content_hashed(Path::new(name)),
                "{name} must be revalidated"
            );
        }
    }
}
