//! Pull-driven Session-log ZIP export with bounded memory and cancellation.

use std::{collections::HashSet, io::Write as _, sync::Arc};

use async_trait::async_trait;
use crc32fast::Hasher;
use flate2::{Compression, write::DeflateEncoder};
use futures::{StreamExt as _, stream::BoxStream};
use indexmap::IndexMap;
use seekdeep_attachment::{AttachmentStore, ImageAttachmentRef, ImageMediaType};
use seekdeep_client_connection::{HttpResponse, HttpResponseStream};
use seekdeep_core::{session::SessionId, session_store::SessionStore};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::{SessionPersistence, SessionRawArtifact};
use serde_json::Value;

use crate::api::downloads::SessionLogQuery;

/// Balanced default used when deployment configuration omits a level.
pub const DEFAULT_SESSION_LOG_COMPRESSION_LEVEL: u8 = 6;
/// Maximum uncompressed input passed through one compressor push.
pub const SESSION_LOG_PUSH_CHUNK_BYTES: usize = 1 << 16;

/// Validated DEFLATE level used for every archive entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionLogCompressionLevel(u8);

impl SessionLogCompressionLevel {
    /// Creates a level in the inclusive `0..=9` source range.
    ///
    /// # Errors
    ///
    /// Rejects levels outside the DEFLATE configuration contract.
    pub fn new(level: u8) -> anyhow::Result<Self> {
        anyhow::ensure!(
            level <= 9,
            "session export compression level must be 0 through 9"
        );
        Ok(Self(level))
    }

    /// Numeric DEFLATE level.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl Default for SessionLogCompressionLevel {
    fn default() -> Self {
        Self(DEFAULT_SESSION_LOG_COMPRESSION_LEVEL)
    }
}

/// Minimal descendant tree required by Session-log export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLineageNode {
    /// Descendant Session id.
    pub session_id: SessionId,
    /// Children in query-engine lineage order.
    pub descendants: Vec<SessionLineageNode>,
}

/// Narrow lineage seam consumed by the exporter.
#[async_trait]
pub trait SessionLogLineageQuery: Send + Sync + 'static {
    /// Returns the target's descendant forest in stable lineage order.
    async fn descendants(
        &self,
        session_id: &SessionId,
        signal: AbortSignal,
    ) -> anyhow::Result<Vec<SessionLineageNode>>;
}

/// Narrow raw-artifact seam consumed by the exporter.
#[async_trait]
pub trait SessionLogPersistence: Send + Sync + 'static {
    /// Whether one verbatim raw artifact exists per Session.
    fn supports_raw_artifacts(&self) -> bool;

    /// Reads one exact stored artifact.
    async fn read_raw(
        &self,
        session_id: &SessionId,
        signal: AbortSignal,
    ) -> anyhow::Result<Option<SessionRawArtifact>>;
}

/// Adapter from the repository-wide persistence service to the export seam.
pub struct SessionPersistenceExportAdapter(pub Arc<dyn SessionPersistence>);

#[async_trait]
impl SessionLogPersistence for SessionPersistenceExportAdapter {
    fn supports_raw_artifacts(&self) -> bool {
        self.0.supports_raw_artifacts()
    }

    async fn read_raw(
        &self,
        session_id: &SessionId,
        signal: AbortSignal,
    ) -> anyhow::Result<Option<SessionRawArtifact>> {
        self.0.read_raw(session_id, Some(signal)).await
    }
}

/// Narrow immutable-image seam consumed by the exporter.
#[async_trait]
pub trait SessionLogAttachments: Send + Sync + 'static {
    /// Reads and verifies one referenced media object.
    async fn read_image(
        &self,
        reference: &ImageAttachmentRef,
        signal: AbortSignal,
    ) -> anyhow::Result<Vec<u8>>;
}

/// Adapter from the repository-wide attachment store to the export seam.
pub struct AttachmentStoreExportAdapter(pub Arc<AttachmentStore>);

#[async_trait]
impl SessionLogAttachments for AttachmentStoreExportAdapter {
    async fn read_image(
        &self,
        reference: &ImageAttachmentRef,
        signal: AbortSignal,
    ) -> anyhow::Result<Vec<u8>> {
        Ok(self.0.read_image(reference, Some(signal)).await?.data)
    }
}

/// Optional live-session durability barrier.
#[async_trait]
pub trait SessionLogLiveSessions: Send + Sync + 'static {
    /// Flushes `session_id` if it is currently live; cold ids are no-ops.
    async fn flush_if_live(
        &self,
        session_id: &SessionId,
        signal: AbortSignal,
    ) -> anyhow::Result<()>;
}

/// Adapter from the repository-wide live Session store to the export seam.
pub struct SessionStoreExportAdapter(pub Arc<SessionStore>);

#[async_trait]
impl SessionLogLiveSessions for SessionStoreExportAdapter {
    async fn flush_if_live(
        &self,
        session_id: &SessionId,
        signal: AbortSignal,
    ) -> anyhow::Result<()> {
        ensure_not_aborted(&signal)?;
        if let Some(session) = self.0.get(session_id) {
            self.0.flush(&session).await?;
        }
        ensure_not_aborted(&signal)
    }
}

/// Optional deployment services resolved before one export.
#[derive(Default)]
pub struct SessionLogExportDeps {
    /// Lineage query service.
    pub session_query: Option<Arc<dyn SessionLogLineageQuery>>,
    /// Raw Session persistence service.
    pub session_persistence: Option<Arc<dyn SessionLogPersistence>>,
    /// Immutable attachment service.
    pub attachments: Option<Arc<dyn SessionLogAttachments>>,
    /// Optional live-session flush service.
    pub sessions: Option<Arc<dyn SessionLogLiveSessions>>,
}

struct SessionLogExportReady {
    session_query: Arc<dyn SessionLogLineageQuery>,
    session_persistence: Arc<dyn SessionLogPersistence>,
    attachments: Arc<dyn SessionLogAttachments>,
    sessions: Option<Arc<dyn SessionLogLiveSessions>>,
}

/// One stored file included in an export archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionLogZipEntry {
    /// Verbatim UTF-8 Session artifact.
    Artifact {
        /// ZIP pathname.
        path: String,
        /// Exact stored text.
        content: String,
    },
    /// Verified immutable attachment bytes.
    Media {
        /// ZIP pathname.
        path: String,
        /// Exact stored bytes.
        data: Vec<u8>,
    },
}

impl SessionLogZipEntry {
    fn path(&self) -> &str {
        match self {
            Self::Artifact { path, .. } | Self::Media { path, .. } => path,
        }
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Self::Artifact { content, .. } => content.as_bytes(),
            Self::Media { data, .. } => data,
        }
    }
}

/// Produces the renamed archive attachment filename.
#[must_use]
pub fn session_log_zip_filename(session_id: &str) -> String {
    format!(
        "seekdeep-session-{}.zip",
        safe_session_id_segment(session_id)
    )
}

/// Prepares one Session-log response before any ZIP byte is emitted.
///
/// Missing services and missing root artifacts are returned as clean HTTP
/// responses. Request cancellation remains an error rather than being
/// translated into a preparation failure.
///
/// # Errors
///
/// Returns caller cancellation or response construction failures.
pub async fn prepare_session_log_response(
    deps: &SessionLogExportDeps,
    request: SessionLogQuery,
    compression_level: SessionLogCompressionLevel,
    signal: AbortSignal,
) -> anyhow::Result<HttpResponse> {
    ensure_not_aborted(&signal)?;
    let (Some(session_query), Some(session_persistence), Some(attachments)) = (
        deps.session_query.clone(),
        deps.session_persistence.clone(),
        deps.attachments.clone(),
    ) else {
        return Ok(HttpResponse::text(
            500,
            "session log export is unavailable: missing session-query, session-persistence, or attachments service",
        ));
    };
    if !session_persistence.supports_raw_artifacts() {
        return Ok(HttpResponse::text(
            501,
            "session log export is unavailable: the persistence backend does not expose per-session raw artifacts",
        ));
    }
    let ready = Arc::new(SessionLogExportReady {
        session_query,
        session_persistence,
        attachments,
        sessions: deps.sessions.clone(),
    });
    let Ok(root) = prepare_root(&ready, &request.session_id, signal.clone()).await else {
        ensure_not_aborted(&signal)?;
        return Ok(HttpResponse::text(
            500,
            "session log export failed to prepare the stored artifact",
        ));
    };
    let Some(root) = root else {
        return Ok(HttpResponse::text(404, "session not found"));
    };

    let consumer_signal = AbortSignal::default();
    let producer_signal = AbortSignal::fuse(&signal, &consumer_signal);
    let entries = session_log_zip_entries(
        ready,
        root,
        request.session_id.clone(),
        request.include_descendants == Some(true),
        producer_signal.clone(),
    );
    let stream = stream_session_log_zip(entries, compression_level, producer_signal);
    Ok(HttpResponse {
        status: 200,
        headers: [
            ("content-type".to_owned(), "application/zip".to_owned()),
            (
                "content-disposition".to_owned(),
                format!(
                    "attachment; filename=\"{}\"",
                    session_log_zip_filename(request.session_id.as_str())
                ),
            ),
        ]
        .into_iter()
        .collect(),
        body: Vec::new(),
        body_stream: Some(HttpResponseStream::new(stream, consumer_signal)),
    })
}

async fn prepare_root(
    deps: &SessionLogExportReady,
    session_id: &SessionId,
    signal: AbortSignal,
) -> anyhow::Result<Option<SessionRawArtifact>> {
    flush_live_session_log(deps.sessions.as_ref(), session_id, signal.clone()).await?;
    let root = deps
        .session_persistence
        .read_raw(session_id, signal.clone())
        .await?;
    ensure_not_aborted(&signal)?;
    Ok(root)
}

/// Flushes one live Session before its raw artifact is read.
///
/// # Errors
///
/// Returns request cancellation or the live store's durability failure.
pub async fn flush_live_session_log(
    sessions: Option<&Arc<dyn SessionLogLiveSessions>>,
    session_id: &SessionId,
    signal: AbortSignal,
) -> anyhow::Result<()> {
    ensure_not_aborted(&signal)?;
    if let Some(sessions) = sessions {
        sessions.flush_if_live(session_id, signal.clone()).await?;
    }
    ensure_not_aborted(&signal)
}

fn session_log_zip_entries(
    deps: Arc<SessionLogExportReady>,
    root: SessionRawArtifact,
    session_id: SessionId,
    include_descendants: bool,
    signal: AbortSignal,
) -> BoxStream<'static, anyhow::Result<SessionLogZipEntry>> {
    Box::pin(async_stream::try_stream! {
        let mut media = IndexMap::<String, ImageAttachmentRef>::new();
        remember_media(&root.content, &mut media);
        yield SessionLogZipEntry::Artifact {
            path: root.filename,
            content: root.content,
        };

        if include_descendants {
            ensure_not_aborted(&signal)?;
            let descendants = deps
                .session_query
                .descendants(&session_id, signal.clone())
                .await?;
            ensure_not_aborted(&signal)?;
            let mut flattened = Vec::new();
            flatten_descendants(&descendants, &mut flattened);
            let mut seen = HashSet::from([session_id]);
            for id in flattened {
                ensure_not_aborted(&signal)?;
                if !seen.insert(id.clone()) {
                    continue;
                }
                flush_live_session_log(deps.sessions.as_ref(), &id, signal.clone()).await?;
                let raw = deps
                    .session_persistence
                    .read_raw(&id, signal.clone())
                    .await?;
                ensure_not_aborted(&signal)?;
                let raw = raw.ok_or_else(|| anyhow::anyhow!(
                    "subagent \"{}\" has no stored log artifact",
                    id.as_str()
                ))?;
                remember_media(&raw.content, &mut media);
                yield SessionLogZipEntry::Artifact {
                    path: format!(
                        "subagents/{}/{}",
                        safe_session_id_segment(id.as_str()),
                        raw.filename
                    ),
                    content: raw.content,
                };
            }
        }

        for (_, reference) in media {
            ensure_not_aborted(&signal)?;
            let data = deps
                .attachments
                .read_image(&reference, signal.clone())
                .await?;
            ensure_not_aborted(&signal)?;
            yield SessionLogZipEntry::Media {
                path: media_entry_path(&reference),
                data,
            };
        }
    })
}

fn flatten_descendants(nodes: &[SessionLineageNode], output: &mut Vec<SessionId>) {
    for node in nodes {
        output.push(node.session_id.clone());
        flatten_descendants(&node.descendants, output);
    }
}

fn remember_media(content: &str, media: &mut IndexMap<String, ImageAttachmentRef>) {
    for (id, reference) in image_refs_in_artifact(content) {
        media.insert(id, reference);
    }
}

fn image_refs_in_artifact(content: &str) -> IndexMap<String, ImageAttachmentRef> {
    let mut references = IndexMap::new();
    for line in content.split('\n').filter(|line| !line.is_empty()) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        collect_event_image_refs(&event, &mut references);
    }
    references
}

fn collect_event_image_refs(event: &Value, references: &mut IndexMap<String, ImageAttachmentRef>) {
    let Some(data) = event.get("data").and_then(Value::as_object) else {
        return;
    };
    collect_image_refs(data.get("content"), references);
    collect_image_refs(
        data.get("message")
            .and_then(|message| message.get("content")),
        references,
    );
    if let Some(inserted) = data.get("inserted").and_then(Value::as_array) {
        for message in inserted {
            collect_image_refs(message.get("content"), references);
        }
    }
    if data
        .get("chunk")
        .and_then(|chunk| chunk.get("type"))
        .and_then(Value::as_str)
        == Some("block-end")
    {
        collect_image_refs(
            data.get("chunk").and_then(|chunk| chunk.get("block")),
            references,
        );
    }
}

fn collect_image_refs(
    content: Option<&Value>,
    references: &mut IndexMap<String, ImageAttachmentRef>,
) {
    let Some(content) = content else { return };
    let mut pending = match content {
        Value::Array(items) => items.iter().collect::<Vec<_>>(),
        value => vec![value],
    };
    while let Some(value) = pending.pop() {
        let Some(object) = value.as_object() else {
            continue;
        };
        if object.get("type").and_then(Value::as_str) == Some("image")
            && let Some(reference) = object
                .get("attachment")
                .and_then(|value| serde_json::from_value::<ImageAttachmentRef>(value.clone()).ok())
        {
            references.insert(reference.attachment_id.as_str().to_owned(), reference);
        }
        if let Some(nested) = object.get("content").and_then(Value::as_array) {
            pending.extend(nested);
        }
    }
}

fn media_entry_path(reference: &ImageAttachmentRef) -> String {
    let extension = match reference.media_type {
        ImageMediaType::Png => "png",
        ImageMediaType::Jpeg => "jpg",
        ImageMediaType::Webp => "webp",
        ImageMediaType::Gif => "gif",
    };
    format!("media/{}.{}", reference.attachment_id.as_str(), extension)
}

fn safe_session_id_segment(id: &str) -> String {
    id.encode_utf16()
        .map(|unit| {
            if u8::try_from(unit)
                .is_ok_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                char::from_u32(u32::from(unit)).expect("allowed units are ASCII")
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug)]
struct CentralEntry {
    path: Vec<u8>,
    crc32: u32,
    compressed_size: u32,
    uncompressed_size: u32,
    local_offset: u32,
}

fn stream_session_log_zip(
    mut entries: BoxStream<'static, anyhow::Result<SessionLogZipEntry>>,
    compression_level: SessionLogCompressionLevel,
    signal: AbortSignal,
) -> BoxStream<'static, anyhow::Result<Vec<u8>>> {
    Box::pin(async_stream::try_stream! {
        let mut central = Vec::<CentralEntry>::new();
        let mut offset = 0_u64;
        while let Some(entry) = entries.next().await {
            ensure_not_aborted(&signal)?;
            let entry = entry?;
            let path = entry.path().as_bytes().to_vec();
            let local_offset = u32::try_from(offset)
                .map_err(|_| anyhow::anyhow!("session export exceeds classic ZIP offset range"))?;
            let local = local_header(&path)?;
            offset = checked_add(offset, local.len())?;
            yield local;

            let mut encoder = DeflateEncoder::new(
                Vec::new(),
                Compression::new(u32::from(compression_level.get())),
            );
            let mut crc = Hasher::new();
            let mut compressed_size = 0_u64;
            let bytes = entry.bytes();
            if bytes.is_empty() {
                encoder.try_finish()?;
                let output = std::mem::take(encoder.get_mut());
                compressed_size = checked_add(compressed_size, output.len())?;
                if !output.is_empty() {
                    offset = checked_add(offset, output.len())?;
                    yield output;
                }
            } else {
                for chunk in bytes.chunks(SESSION_LOG_PUSH_CHUNK_BYTES) {
                    ensure_not_aborted(&signal)?;
                    crc.update(chunk);
                    encoder.write_all(chunk)?;
                    let output = std::mem::take(encoder.get_mut());
                    compressed_size = checked_add(compressed_size, output.len())?;
                    if !output.is_empty() {
                        offset = checked_add(offset, output.len())?;
                        yield output;
                    }
                }
                encoder.try_finish()?;
                let output = std::mem::take(encoder.get_mut());
                compressed_size = checked_add(compressed_size, output.len())?;
                if !output.is_empty() {
                    offset = checked_add(offset, output.len())?;
                    yield output;
                }
            }
            let crc32 = crc.finalize();
            let compressed_size = u32::try_from(compressed_size)
                .map_err(|_| anyhow::anyhow!("session export entry exceeds classic ZIP size range"))?;
            let uncompressed_size = u32::try_from(bytes.len())
                .map_err(|_| anyhow::anyhow!("session export entry exceeds classic ZIP size range"))?;
            let descriptor = data_descriptor(crc32, compressed_size, uncompressed_size);
            offset = checked_add(offset, descriptor.len())?;
            yield descriptor;
            central.push(CentralEntry {
                path,
                crc32,
                compressed_size,
                uncompressed_size,
                local_offset,
            });
        }
        ensure_not_aborted(&signal)?;
        let central_offset = u32::try_from(offset)
            .map_err(|_| anyhow::anyhow!("session export exceeds classic ZIP offset range"))?;
        let mut directory = Vec::new();
        for entry in &central {
            directory.extend(central_header(entry)?);
        }
        let central_size = u32::try_from(directory.len())
            .map_err(|_| anyhow::anyhow!("session export central directory is too large"))?;
        if !directory.is_empty() {
            yield directory;
        }
        yield end_of_central_directory(central.len(), central_size, central_offset)?;
    })
}

const ZIP_FLAGS: u16 = 0x0808;
const ZIP_DEFLATE: u16 = 8;

fn local_header(path: &[u8]) -> anyhow::Result<Vec<u8>> {
    let path_len = u16::try_from(path.len())
        .map_err(|_| anyhow::anyhow!("session export ZIP pathname is too long"))?;
    let mut output = Vec::with_capacity(30 + path.len());
    put_u32(&mut output, 0x0403_4b50);
    put_u16(&mut output, 20);
    put_u16(&mut output, ZIP_FLAGS);
    put_u16(&mut output, ZIP_DEFLATE);
    put_u16(&mut output, 0);
    put_u16(&mut output, 0);
    put_u32(&mut output, 0);
    put_u32(&mut output, 0);
    put_u32(&mut output, 0);
    put_u16(&mut output, path_len);
    put_u16(&mut output, 0);
    output.extend_from_slice(path);
    Ok(output)
}

fn data_descriptor(crc32: u32, compressed_size: u32, uncompressed_size: u32) -> Vec<u8> {
    let mut output = Vec::with_capacity(16);
    put_u32(&mut output, 0x0807_4b50);
    put_u32(&mut output, crc32);
    put_u32(&mut output, compressed_size);
    put_u32(&mut output, uncompressed_size);
    output
}

fn central_header(entry: &CentralEntry) -> anyhow::Result<Vec<u8>> {
    let path_len = u16::try_from(entry.path.len())
        .map_err(|_| anyhow::anyhow!("session export ZIP pathname is too long"))?;
    let mut output = Vec::with_capacity(46 + entry.path.len());
    put_u32(&mut output, 0x0201_4b50);
    put_u16(&mut output, 20);
    put_u16(&mut output, 20);
    put_u16(&mut output, ZIP_FLAGS);
    put_u16(&mut output, ZIP_DEFLATE);
    put_u16(&mut output, 0);
    put_u16(&mut output, 0);
    put_u32(&mut output, entry.crc32);
    put_u32(&mut output, entry.compressed_size);
    put_u32(&mut output, entry.uncompressed_size);
    put_u16(&mut output, path_len);
    put_u16(&mut output, 0);
    put_u16(&mut output, 0);
    put_u16(&mut output, 0);
    put_u16(&mut output, 0);
    put_u32(&mut output, 0);
    put_u32(&mut output, entry.local_offset);
    output.extend_from_slice(&entry.path);
    Ok(output)
}

fn end_of_central_directory(
    entries: usize,
    central_size: u32,
    central_offset: u32,
) -> anyhow::Result<Vec<u8>> {
    let entries = u16::try_from(entries)
        .map_err(|_| anyhow::anyhow!("session export has too many ZIP entries"))?;
    let mut output = Vec::with_capacity(22);
    put_u32(&mut output, 0x0605_4b50);
    put_u16(&mut output, 0);
    put_u16(&mut output, 0);
    put_u16(&mut output, entries);
    put_u16(&mut output, entries);
    put_u32(&mut output, central_size);
    put_u32(&mut output, central_offset);
    put_u16(&mut output, 0);
    Ok(output)
}

fn checked_add(offset: u64, amount: usize) -> anyhow::Result<u64> {
    offset
        .checked_add(u64::try_from(amount)?)
        .ok_or_else(|| anyhow::anyhow!("session export byte offset overflow"))
}

fn put_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn ensure_not_aborted(signal: &AbortSignal) -> anyhow::Result<()> {
    if signal.is_aborted() {
        let reason = signal.reason().map_or_else(
            || "This operation was aborted".to_owned(),
            |reason| match reason {
                Value::String(reason) => reason,
                other => other.to_string(),
            },
        );
        anyhow::bail!(reason);
    }
    Ok(())
}
