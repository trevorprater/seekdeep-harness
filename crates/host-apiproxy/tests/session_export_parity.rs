//! Session-log ZIP export behavior mirrored from `session-export.spec.ts`.

use std::{collections::HashMap, io::Read as _, sync::Arc};

use async_trait::async_trait;
use flate2::read::DeflateDecoder;
use futures::StreamExt as _;
use parking_lot::Mutex;
use seekdeep_attachment::{AttachmentId, ImageAttachmentRef, ImageMediaType};
use seekdeep_client_connection::{HttpResponse, HttpResponseStream};
use seekdeep_core::session::{SessionHeader, SessionId};
use seekdeep_host_apiproxy::{
    SessionLineageNode, SessionLogAttachments, SessionLogCompressionLevel, SessionLogExportDeps,
    SessionLogLineageQuery, SessionLogLiveSessions, SessionLogPersistence,
    api::downloads::SessionLogQuery, prepare_session_log_response, session_log_zip_filename,
};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::SessionRawArtifact;
use serde_json::{Value, json};

fn artifact(id: &str, content: impl Into<String>) -> SessionRawArtifact {
    SessionRawArtifact {
        meta: SessionHeader::new(SessionId::new(id)),
        filename: "session.jsonl".to_owned(),
        content: content.into(),
    }
}

fn query(id: &str, descendants: bool) -> SessionLogQuery {
    SessionLogQuery {
        session_id: SessionId::new(id),
        include_descendants: descendants.then_some(true),
    }
}

fn image_ref(id: &str, media_type: ImageMediaType) -> ImageAttachmentRef {
    ImageAttachmentRef {
        attachment_id: AttachmentId::new(id),
        media_type,
        bytes: 4,
        width: 2,
        height: 2,
        name: None,
    }
}

fn image_line(id: &str, media_type: &str) -> String {
    json!({
        "type": "user/message",
        "seq": 1,
        "time": 1000,
        "data": {
            "content": [{
                "type": "image",
                "attachment": {
                    "attachmentId": id,
                    "mediaType": media_type,
                    "bytes": 4,
                    "width": 2,
                    "height": 2,
                },
            }],
        },
    })
    .to_string()
}

struct FakePersistence {
    supports: bool,
    artifacts: Mutex<HashMap<SessionId, SessionRawArtifact>>,
    fail_root: bool,
    reads: Mutex<Vec<(SessionId, AbortSignal)>>,
}

impl FakePersistence {
    fn new(artifacts: impl IntoIterator<Item = SessionRawArtifact>) -> Arc<Self> {
        Arc::new(Self {
            supports: true,
            artifacts: Mutex::new(
                artifacts
                    .into_iter()
                    .map(|artifact| (artifact.meta.id.clone(), artifact))
                    .collect(),
            ),
            fail_root: false,
            reads: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl SessionLogPersistence for FakePersistence {
    fn supports_raw_artifacts(&self) -> bool {
        self.supports
    }

    async fn read_raw(
        &self,
        session_id: &SessionId,
        signal: AbortSignal,
    ) -> anyhow::Result<Option<SessionRawArtifact>> {
        self.reads.lock().push((session_id.clone(), signal));
        if self.fail_root {
            anyhow::bail!("/private/session.jsonl");
        }
        Ok(self.artifacts.lock().get(session_id).cloned())
    }
}

struct FakeQuery {
    descendants: Vec<SessionLineageNode>,
    signals: Mutex<Vec<AbortSignal>>,
}

#[async_trait]
impl SessionLogLineageQuery for FakeQuery {
    async fn descendants(
        &self,
        _session_id: &SessionId,
        signal: AbortSignal,
    ) -> anyhow::Result<Vec<SessionLineageNode>> {
        self.signals.lock().push(signal);
        Ok(self.descendants.clone())
    }
}

#[derive(Default)]
struct FakeAttachments {
    reads: Mutex<Vec<(ImageAttachmentRef, AbortSignal)>>,
    fail: bool,
    started: tokio::sync::Notify,
}

#[async_trait]
impl SessionLogAttachments for FakeAttachments {
    async fn read_image(
        &self,
        reference: &ImageAttachmentRef,
        signal: AbortSignal,
    ) -> anyhow::Result<Vec<u8>> {
        self.reads.lock().push((reference.clone(), signal.clone()));
        self.started.notify_waiters();
        if self.fail {
            anyhow::bail!("attachment bytes missing");
        }
        Ok(vec![1, 2, 3, 4])
    }
}

struct RecordingSessions {
    flushed: Mutex<Vec<SessionId>>,
}

#[async_trait]
impl SessionLogLiveSessions for RecordingSessions {
    async fn flush_if_live(
        &self,
        session_id: &SessionId,
        _signal: AbortSignal,
    ) -> anyhow::Result<()> {
        self.flushed.lock().push(session_id.clone());
        Ok(())
    }
}

fn deps(
    persistence: Arc<dyn SessionLogPersistence>,
    descendants: Vec<SessionLineageNode>,
    attachments: Arc<dyn SessionLogAttachments>,
) -> SessionLogExportDeps {
    SessionLogExportDeps {
        session_query: Some(Arc::new(FakeQuery {
            descendants,
            signals: Mutex::new(Vec::new()),
        })),
        session_persistence: Some(persistence),
        attachments: Some(attachments),
        sessions: None,
    }
}

async fn response_bytes(mut response: HttpResponse) -> anyhow::Result<Vec<u8>> {
    let mut bytes = response.body;
    if let Some(mut stream) = response.body_stream.take() {
        while let Some(chunk) = stream.next().await {
            bytes.extend(chunk?);
        }
    }
    Ok(bytes)
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn unzip(bytes: &[u8]) -> HashMap<String, Vec<u8>> {
    assert!(bytes.len() >= 22);
    let eocd = bytes.len() - 22;
    assert_eq!(u32_at(bytes, eocd), 0x0605_4b50);
    let entries = usize::from(u16_at(bytes, eocd + 10));
    let mut central = usize::try_from(u32_at(bytes, eocd + 16)).unwrap();
    let mut files = HashMap::new();
    for _ in 0..entries {
        assert_eq!(u32_at(bytes, central), 0x0201_4b50);
        let compressed_size = usize::try_from(u32_at(bytes, central + 20)).unwrap();
        let uncompressed_size = usize::try_from(u32_at(bytes, central + 24)).unwrap();
        let name_len = usize::from(u16_at(bytes, central + 28));
        let extra_len = usize::from(u16_at(bytes, central + 30));
        let comment_len = usize::from(u16_at(bytes, central + 32));
        let local = usize::try_from(u32_at(bytes, central + 42)).unwrap();
        let name =
            String::from_utf8(bytes[central + 46..central + 46 + name_len].to_vec()).unwrap();
        assert_eq!(u32_at(bytes, local), 0x0403_4b50);
        let local_name_len = usize::from(u16_at(bytes, local + 26));
        let local_extra_len = usize::from(u16_at(bytes, local + 28));
        let data_start = local + 30 + local_name_len + local_extra_len;
        let mut decoded = Vec::with_capacity(uncompressed_size);
        DeflateDecoder::new(&bytes[data_start..data_start + compressed_size])
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded.len(), uncompressed_size);
        files.insert(name, decoded);
        central += 46 + name_len + extra_len + comment_len;
    }
    files
}

#[test]
fn compression_levels_and_renamed_safe_filenames_are_closed() {
    assert_eq!(SessionLogCompressionLevel::default().get(), 6);
    assert_eq!(SessionLogCompressionLevel::new(0).unwrap().get(), 0);
    assert_eq!(SessionLogCompressionLevel::new(9).unwrap().get(), 9);
    assert!(SessionLogCompressionLevel::new(10).is_err());
    assert_eq!(
        session_log_zip_filename("../a/😀"),
        "seekdeep-session-___a___.zip"
    );
}

#[tokio::test]
async fn preparation_returns_clean_missing_unsupported_not_found_and_private_safe_errors() {
    let missing = prepare_session_log_response(
        &SessionLogExportDeps::default(),
        query("root", false),
        SessionLogCompressionLevel::default(),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_eq!(missing.status, 500);
    assert!(
        String::from_utf8(missing.body)
            .unwrap()
            .contains("session-query")
    );

    let unsupported = Arc::new(FakePersistence {
        supports: false,
        artifacts: Mutex::new(HashMap::new()),
        fail_root: false,
        reads: Mutex::new(Vec::new()),
    });
    let unavailable = prepare_session_log_response(
        &deps(
            unsupported,
            Vec::new(),
            Arc::new(FakeAttachments::default()),
        ),
        query("root", false),
        SessionLogCompressionLevel::default(),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_eq!(unavailable.status, 501);

    let absent = prepare_session_log_response(
        &deps(
            FakePersistence::new([]),
            Vec::new(),
            Arc::new(FakeAttachments::default()),
        ),
        query("root", false),
        SessionLogCompressionLevel::default(),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_eq!(absent.status, 404);

    let failing = Arc::new(FakePersistence {
        supports: true,
        artifacts: Mutex::new(HashMap::new()),
        fail_root: true,
        reads: Mutex::new(Vec::new()),
    });
    let failed = prepare_session_log_response(
        &deps(failing, Vec::new(), Arc::new(FakeAttachments::default())),
        query("root", false),
        SessionLogCompressionLevel::default(),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_eq!(failed.status, 500);
    let body = String::from_utf8(failed.body).unwrap();
    assert_eq!(
        body,
        "session log export failed to prepare the stored artifact"
    );
    assert!(!body.contains("/private/"));

    let cancelled = AbortSignal::default();
    cancelled.abort_with_reason(Value::String("request cancelled".to_owned()));
    let error = prepare_session_log_response(
        &deps(
            FakePersistence::new([artifact("root", "")]),
            Vec::new(),
            Arc::new(FakeAttachments::default()),
        ),
        query("root", false),
        SessionLogCompressionLevel::default(),
        cancelled,
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "request cancelled");
}

#[tokio::test]
async fn zip_streams_root_verbatim_and_compression_level_changes_size() {
    let content = format!("{}😀tail", "compressible\n".repeat(8_000));
    let persistence = FakePersistence::new([artifact("root", content.clone())]);
    let plain = prepare_session_log_response(
        &deps(
            persistence.clone(),
            Vec::new(),
            Arc::new(FakeAttachments::default()),
        ),
        query("root", false),
        SessionLogCompressionLevel::new(0).unwrap(),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_eq!(plain.status, 200);
    assert_eq!(plain.headers["content-type"], "application/zip");
    assert!(plain.headers["content-disposition"].contains("seekdeep-session-root.zip"));
    let plain = response_bytes(plain).await.unwrap();

    let compressed = prepare_session_log_response(
        &deps(
            persistence,
            Vec::new(),
            Arc::new(FakeAttachments::default()),
        ),
        query("root", false),
        SessionLogCompressionLevel::new(9).unwrap(),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    let compressed = response_bytes(compressed).await.unwrap();
    assert!(compressed.len() < plain.len());
    assert_eq!(unzip(&compressed)["session.jsonl"], content.as_bytes());

    let empty = prepare_session_log_response(
        &deps(
            FakePersistence::new([artifact("root", "")]),
            Vec::new(),
            Arc::new(FakeAttachments::default()),
        ),
        query("root", false),
        SessionLogCompressionLevel::default(),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        unzip(&response_bytes(empty).await.unwrap())["session.jsonl"],
        b""
    );
}

#[tokio::test]
async fn descendants_follow_depth_first_order_flush_before_read_and_deduplicate() {
    let persistence = FakePersistence::new([
        artifact("root", "root"),
        artifact("a", "a"),
        artifact("b", "b"),
        artifact("shared", "shared"),
    ]);
    let sessions = Arc::new(RecordingSessions {
        flushed: Mutex::new(Vec::new()),
    });
    let mut dependencies = deps(
        persistence.clone(),
        vec![
            SessionLineageNode {
                session_id: SessionId::new("a"),
                descendants: vec![SessionLineageNode {
                    session_id: SessionId::new("shared"),
                    descendants: Vec::new(),
                }],
            },
            SessionLineageNode {
                session_id: SessionId::new("b"),
                descendants: vec![SessionLineageNode {
                    session_id: SessionId::new("shared"),
                    descendants: Vec::new(),
                }],
            },
        ],
        Arc::new(FakeAttachments::default()),
    );
    dependencies.sessions = Some(sessions.clone());
    let response = prepare_session_log_response(
        &dependencies,
        query("root", true),
        SessionLogCompressionLevel::default(),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    let files = unzip(&response_bytes(response).await.unwrap());
    assert_eq!(files.len(), 4);
    assert_eq!(files["subagents/a/session.jsonl"], b"a");
    assert_eq!(files["subagents/b/session.jsonl"], b"b");
    assert_eq!(files["subagents/shared/session.jsonl"], b"shared");
    assert_eq!(
        sessions
            .flushed
            .lock()
            .iter()
            .map(SessionId::as_str)
            .collect::<Vec<_>>(),
        ["root", "a", "shared", "b"]
    );
    assert_eq!(
        persistence
            .reads
            .lock()
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        ["root", "a", "shared", "b"]
    );
}

#[tokio::test]
async fn media_scan_covers_every_carrier_extension_and_deduplicates() {
    let block = |id: &str, media_type: &str| {
        json!({
            "type": "image",
            "attachment": {
                "attachmentId": id,
                "mediaType": media_type,
                "bytes": 4,
                "width": 2,
                "height": 2,
            },
        })
    };
    let lines = [
        image_line("direct", "image/png"),
        json!({"data": {"message": {"content": ["noise", block("wrapped", "image/jpeg")]}}}).to_string(),
        json!({"data": {"inserted": [{"content": [block("inserted", "image/gif")]}]}}).to_string(),
        json!({"data": {"chunk": {"type": "block-end", "block": block("chunk", "image/webp")}}}).to_string(),
        json!({"data": {"content": [{"type": "tool-result", "content": [block("direct", "image/png")]}]}}).to_string(),
        "not-json".to_owned(),
    ];
    let attachments = Arc::new(FakeAttachments::default());
    let response = prepare_session_log_response(
        &deps(
            FakePersistence::new([artifact("root", lines.join("\n"))]),
            Vec::new(),
            attachments.clone(),
        ),
        query("root", false),
        SessionLogCompressionLevel::default(),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    let files = unzip(&response_bytes(response).await.unwrap());
    assert_eq!(files["media/direct.png"], [1, 2, 3, 4]);
    assert_eq!(files["media/wrapped.jpg"], [1, 2, 3, 4]);
    assert_eq!(files["media/inserted.gif"], [1, 2, 3, 4]);
    assert_eq!(files["media/chunk.webp"], [1, 2, 3, 4]);
    assert_eq!(attachments.reads.lock().len(), 4);
}

#[tokio::test]
async fn pull_backpressure_defers_media_read_and_midstream_failures_stay_errors() {
    let attachments = Arc::new(FakeAttachments::default());
    let root = format!(
        "{}\n{}",
        image_line("after-root", "image/png"),
        "randomish".repeat(128 * 1024)
    );
    let mut response = prepare_session_log_response(
        &deps(
            FakePersistence::new([artifact("root", root)]),
            Vec::new(),
            attachments.clone(),
        ),
        query("root", false),
        SessionLogCompressionLevel::default(),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    let stream = response.body_stream.as_mut().unwrap();
    assert_eq!(attachments.reads.lock().len(), 0);
    assert!(stream.next().await.unwrap().is_ok());
    assert_eq!(attachments.reads.lock().len(), 0);
    while stream.next().await.is_some() {}
    assert_eq!(attachments.reads.lock().len(), 1);

    let failing = Arc::new(FakeAttachments {
        fail: true,
        ..FakeAttachments::default()
    });
    let response = prepare_session_log_response(
        &deps(
            FakePersistence::new([artifact("root", image_line("gone", "image/png"))]),
            Vec::new(),
            failing,
        ),
        query("root", false),
        SessionLogCompressionLevel::default(),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    let error = response_bytes(response).await.unwrap_err();
    assert!(error.to_string().contains("attachment bytes missing"));
}

struct HangingAttachments {
    signal: Mutex<Option<AbortSignal>>,
    started: tokio::sync::Notify,
}

#[async_trait]
impl SessionLogAttachments for HangingAttachments {
    async fn read_image(
        &self,
        _reference: &ImageAttachmentRef,
        signal: AbortSignal,
    ) -> anyhow::Result<Vec<u8>> {
        *self.signal.lock() = Some(signal.clone());
        self.started.notify_waiters();
        signal.cancelled().await;
        anyhow::bail!(signal.reason().unwrap_or(Value::Null).to_string())
    }
}

#[tokio::test]
async fn consumer_cancellation_aborts_active_attachment_work_with_same_reason() {
    let attachments = Arc::new(HangingAttachments {
        signal: Mutex::new(None),
        started: tokio::sync::Notify::new(),
    });
    let mut response = prepare_session_log_response(
        &deps(
            FakePersistence::new([artifact("root", image_line("slow", "image/png"))]),
            Vec::new(),
            attachments.clone(),
        ),
        query("root", false),
        SessionLogCompressionLevel::default(),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    let mut body = response.body_stream.take().unwrap();
    let consumer = body.consumer_signal();
    let collect = tokio::spawn(async move {
        while let Some(chunk) = body.next().await {
            chunk?;
        }
        Ok::<_, anyhow::Error>(())
    });
    attachments.started.notified().await;
    consumer.abort_with_reason(Value::String("download consumer left".to_owned()));
    assert!(collect.await.unwrap().is_err());
    let producer = attachments.signal.lock().clone().unwrap();
    assert!(producer.is_aborted());
    assert_eq!(
        producer.reason(),
        Some(Value::String("download consumer left".to_owned()))
    );
}

#[tokio::test]
async fn dropping_unconsumed_body_uses_stable_cancellation_reason() {
    let consumer = AbortSignal::default();
    let producer = AbortSignal::fuse(&AbortSignal::default(), &consumer);
    let body = HttpResponseStream::new(futures::stream::pending().boxed(), consumer);
    drop(body);
    assert_eq!(
        producer.reason(),
        Some(Value::String(
            "session log export stream cancelled".to_owned()
        ))
    );
}

#[test]
fn image_reference_fixture_covers_all_media_variants() {
    assert_eq!(
        image_ref("a", ImageMediaType::Png).media_type.as_str(),
        "image/png"
    );
    assert_eq!(
        image_ref("b", ImageMediaType::Jpeg).media_type.as_str(),
        "image/jpeg"
    );
    assert_eq!(
        image_ref("c", ImageMediaType::Webp).media_type.as_str(),
        "image/webp"
    );
    assert_eq!(
        image_ref("d", ImageMediaType::Gif).media_type.as_str(),
        "image/gif"
    );
}
