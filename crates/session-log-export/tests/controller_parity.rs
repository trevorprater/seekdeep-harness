//! Browser controller, helper, and lifecycle parity.

#![cfg(not(target_arch = "wasm32"))]

use std::{cell::RefCell, rc::Rc};

use futures::{FutureExt as _, channel::oneshot};
use seekdeep_core::session::SessionId;
use seekdeep_session_log_export::{
    DownloadFetcher, DownloadRequest, DownloadResponse, DownloadSaver,
    SessionLogDownloadController, SessionLogDownloadState, SessionLogDownloadStatus, host_base,
    session_log_zip_filename,
};

fn id(value: &str) -> SessionId {
    SessionId::new(value)
}

#[tokio::test(flavor = "current_thread")]
async fn downloads_exact_head_url_and_publishes_success() {
    let requests = Rc::new(RefCell::new(Vec::<DownloadRequest>::new()));
    let seen = requests.clone();
    let fetcher: DownloadFetcher = Rc::new(move |request| {
        seen.borrow_mut().push(request);
        async {
            Ok(DownloadResponse {
                status: 200,
                detail: Ok("zip".to_owned()),
            })
        }
        .boxed_local()
    });
    let saves = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
    let save_sink = saves.clone();
    let save: DownloadSaver = Rc::new(move |url, filename| {
        save_sink
            .borrow_mut()
            .push((url.to_owned(), filename.to_owned()));
        Ok(())
    });
    let controller =
        SessionLogDownloadController::new(fetcher, save, Some("https://harness.example/base"));
    controller.download(id("session-export-controller")).await;

    let request = &requests.borrow()[0];
    assert_eq!(
        request.url,
        "https://harness.example/api/session.export?sessionId=session-export-controller&includeDescendants=true"
    );
    assert!(!request.signal.is_aborted());
    assert_eq!(
        saves.borrow().as_slice(),
        [(
            request.url.clone(),
            "seekdeep-session-session-export-controller.zip".to_owned()
        )]
    );
    let entry = &controller.state().by_session["session-export-controller"];
    assert_eq!(entry.status, SessionLogDownloadStatus::Success);
    assert!(entry.open);
    assert_eq!(entry.error, None);
}

#[tokio::test(flavor = "current_thread")]
async fn collapses_concurrent_gestures_and_preserves_dismissal_or_external_clear() {
    let (sender, receiver) = oneshot::channel::<DownloadResponse>();
    let receiver = Rc::new(RefCell::new(Some(receiver)));
    let calls = Rc::new(RefCell::new(0_usize));
    let call_count = calls.clone();
    let fetcher: DownloadFetcher = Rc::new(move |_| {
        *call_count.borrow_mut() += 1;
        let receiver = receiver.borrow_mut().take().unwrap();
        async move { receiver.await.map_err(|error| error.to_string()) }.boxed_local()
    });
    let controller =
        SessionLogDownloadController::new(fetcher, Rc::new(|_, _| Ok(())), Some("http://host"));
    let session_id = id("shared");
    let first = controller.download(session_id.clone());
    let second = controller.download(session_id.clone());
    controller.dismiss(&session_id);
    sender
        .send(DownloadResponse {
            status: 200,
            detail: Ok(String::new()),
        })
        .unwrap();
    futures::join!(first, second);
    assert_eq!(*calls.borrow(), 1);
    assert!(!controller.state().by_session["shared"].open);
    controller.dismiss(&session_id);

    let cleared = SessionLogDownloadController::new(
        Rc::new(|_| {
            async {
                Ok(DownloadResponse {
                    status: 200,
                    detail: Ok(String::new()),
                })
            }
            .boxed_local()
        }),
        Rc::new(|_, _| Ok(())),
        Some("http://host"),
    );
    let pending = cleared.download(id("cleared"));
    cleared.set_state(SessionLogDownloadState::default());
    pending.await;
    assert!(cleared.state().by_session["cleared"].open);
}

#[tokio::test(flavor = "current_thread")]
async fn contains_http_transport_body_and_save_failures() {
    for (response, expected) in [
        (
            Ok(DownloadResponse {
                status: 500,
                detail: Ok("backend unavailable".to_owned()),
            }),
            "Export failed: HTTP 500 backend unavailable",
        ),
        (
            Ok(DownloadResponse {
                status: 503,
                detail: Err("body unavailable".to_owned()),
            }),
            "Export failed: HTTP 503",
        ),
        (Err("offline".to_owned()), "offline"),
    ] {
        let response = RefCell::new(Some(response));
        let controller = SessionLogDownloadController::new(
            Rc::new(move |_| {
                let response = response.borrow_mut().take().unwrap();
                async move { response }.boxed_local()
            }),
            Rc::new(|_, _| Ok(())),
            Some("http://host"),
        );
        controller.download(id("failure")).await;
        let entry = &controller.state().by_session["failure"];
        assert_eq!(entry.status, SessionLogDownloadStatus::Error);
        assert_eq!(entry.error.as_deref(), Some(expected));
    }
    let controller = SessionLogDownloadController::new(
        Rc::new(|_| {
            async {
                Ok(DownloadResponse {
                    status: 200,
                    detail: Ok(String::new()),
                })
            }
            .boxed_local()
        }),
        Rc::new(|_, _| Err("save failed".to_owned())),
        Some("http://host"),
    );
    controller.download(id("save")).await;
    assert_eq!(
        controller.state().by_session["save"].error.as_deref(),
        Some("save failed")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn disposal_aborts_fetches_joins_them_and_ignores_late_requests() {
    let captured = Rc::new(RefCell::new(None));
    let signal = captured.clone();
    let fetcher: DownloadFetcher = Rc::new(move |request| {
        *signal.borrow_mut() = Some(request.signal.clone());
        async move {
            request.signal.cancelled().await;
            Err("aborted".to_owned())
        }
        .boxed_local()
    });
    let controller =
        SessionLogDownloadController::new(fetcher, Rc::new(|_, _| Ok(())), Some("http://host"));
    let pending = controller.download(id("active"));
    controller.dispose().await;
    pending.await;
    assert!(captured.borrow().as_ref().unwrap().is_aborted());
    controller.download(id("late")).await;
    assert!(!controller.state().by_session.contains_key("late"));
    controller.dispose().await;
}

#[test]
fn sanitizes_filenames_and_resolves_null_origin() {
    assert_eq!(
        session_log_zip_filename(&id("a/b:中")),
        "seekdeep-session-a_b__.zip"
    );
    assert_eq!(host_base(Some("null")), "http://seekdeep.internal");
    assert_eq!(host_base(None), "http://seekdeep.internal");
    assert_eq!(
        host_base(Some("https://harness.example")),
        "https://harness.example"
    );
}
