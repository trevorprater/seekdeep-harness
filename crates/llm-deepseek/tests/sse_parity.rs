//! Behavioral mirror of `packages/llm/llm-deepseek/tests/sse.spec.ts`.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures::{TryStreamExt as _, stream};
use seekdeep_llm_deepseek::sse::{ByteStream, CommentObserver, DONE, parse_sse};

fn bytes(fragments: &[&str]) -> ByteStream {
    let chunks = fragments
        .iter()
        .map(|fragment| Ok(Bytes::copy_from_slice(fragment.as_bytes())))
        .collect::<Vec<anyhow::Result<Bytes>>>();
    Box::pin(stream::iter(chunks))
}

async fn collect(
    fragments: &[&str],
    observer: Option<CommentObserver>,
) -> anyhow::Result<Vec<String>> {
    parse_sse(bytes(fragments), observer).try_collect().await
}

#[tokio::test]
async fn yields_payloads_and_done_then_ignores_late_data() {
    assert_eq!(
        collect(&["data: {\"a\":1}\n\ndata: [DONE]\n\n"], None)
            .await
            .unwrap(),
        ["{\"a\":1}", DONE]
    );
    assert_eq!(
        collect(&["data: [DONE]\n\ndata: {\"late\":1}\n\n"], None)
            .await
            .unwrap(),
        [DONE]
    );
}

#[tokio::test]
async fn reports_comments_out_of_band() {
    let comments = Arc::new(Mutex::new(Vec::<String>::new()));
    let shared_comments = comments.clone();
    let callback: CommentObserver = Arc::new(move |comment| {
        shared_comments.lock().unwrap().push(comment.to_owned());
    });
    let events = collect(
        &[": keep-alive\n\ndata: {\"a\":1}\n\ndata: [DONE]\n\n"],
        Some(callback),
    )
    .await
    .unwrap();
    assert_eq!(*comments.lock().unwrap(), ["keep-alive"]);
    assert_eq!(events, ["{\"a\":1}", DONE]);
}

#[tokio::test]
async fn every_eof_before_terminated_done_is_stream_closed() {
    for fragments in [
        vec!["data: {\"a\":1}\n\n"],
        vec![],
        vec!["data: {\"a\""],
        vec!["data: {\"a\":1}\n\ndata: [DONE]"],
    ] {
        let error = collect(&fragments, None).await.unwrap_err();
        assert!(error.to_string().contains("without [DONE]"), "{error:#}");
        assert_eq!(
            error
                .downcast_ref::<seekdeep_llm::LlmError>()
                .unwrap()
                .code(),
            "STREAM_CLOSED"
        );
    }
}

#[tokio::test]
async fn framing_handles_splits_crlf_bom_multidata_and_utf8() {
    let events = collect(
        &[
            "\u{feff}da",
            "ta: first\r\ndata: caf",
            "\u{00e9}\r\n\r\n: pulse\r\ndata: [DO",
            "NE]\r\n\r\n",
        ],
        None,
    )
    .await
    .unwrap();
    assert_eq!(events, ["first\ncaf\u{00e9}", DONE]);

    let utf8 = "data: 😀\n\ndata: [DONE]\n\n".as_bytes();
    let chunks = vec![
        Ok(Bytes::copy_from_slice(&utf8[..8])),
        Ok(Bytes::copy_from_slice(&utf8[8..10])),
        Ok(Bytes::copy_from_slice(&utf8[10..])),
    ];
    let events = parse_sse(Box::pin(stream::iter(chunks)), None)
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    assert_eq!(events, ["😀", DONE]);
}

#[tokio::test]
async fn a_final_cr_is_a_real_event_terminator() {
    assert_eq!(collect(&["data: [DONE]\r\r"], None).await.unwrap(), [DONE]);
}
