//! Portable controller load, CAS, queue, conflict, transport, and disposal parity.

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use futures::{FutureExt as _, channel::oneshot, executor::block_on, future::LocalBoxFuture};
use seekdeep_client_ui_message_feedback::*;

type ListFuture =
    LocalBoxFuture<'static, Result<FeedbackRemoteResult<Vec<MessageFeedbackItem>>, String>>;
type PutFuture = LocalBoxFuture<'static, Result<FeedbackRemoteResult<MessageFeedbackItem>, String>>;
type DeleteFuture = LocalBoxFuture<'static, Result<FeedbackRemoteResult<()>, String>>;

#[derive(Clone, Debug)]
enum Call {
    List,
    Put {
        message_id: FeedbackMessageId,
        rating: MessageFeedbackRating,
        note: Option<String>,
        if_version: Option<MessageFeedbackVersion>,
    },
    Delete {
        message_id: FeedbackMessageId,
        if_version: MessageFeedbackVersion,
    },
}

#[derive(Default)]
struct FakeRemote {
    calls: RefCell<Vec<Call>>,
    lists: RefCell<VecDeque<ListFuture>>,
    puts: RefCell<VecDeque<PutFuture>>,
    deletes: RefCell<VecDeque<DeleteFuture>>,
}

impl MessageFeedbackRemote for FakeRemote {
    fn list(
        &self,
        _session_id: FeedbackSessionId,
    ) -> LocalBoxFuture<'static, Result<FeedbackRemoteResult<Vec<MessageFeedbackItem>>, String>>
    {
        self.calls.borrow_mut().push(Call::List);
        self.lists
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| async { Ok(Ok(Ok(Vec::new()))) }.boxed_local())
    }

    fn put(
        &self,
        _session_id: FeedbackSessionId,
        message_id: FeedbackMessageId,
        rating: MessageFeedbackRating,
        note: Option<String>,
        if_version: Option<MessageFeedbackVersion>,
    ) -> LocalBoxFuture<'static, Result<FeedbackRemoteResult<MessageFeedbackItem>, String>> {
        self.calls.borrow_mut().push(Call::Put {
            message_id,
            rating,
            note,
            if_version,
        });
        self.puts.borrow_mut().pop_front().expect("unexpected put")
    }

    fn delete(
        &self,
        _session_id: FeedbackSessionId,
        message_id: FeedbackMessageId,
        if_version: MessageFeedbackVersion,
    ) -> LocalBoxFuture<'static, Result<FeedbackRemoteResult<()>, String>> {
        self.calls.borrow_mut().push(Call::Delete {
            message_id,
            if_version,
        });
        self.deletes
            .borrow_mut()
            .pop_front()
            .expect("unexpected delete")
    }
}

fn item(version: &str, rating: MessageFeedbackRating, note: Option<&str>) -> MessageFeedbackItem {
    MessageFeedbackItem {
        message_id: FeedbackMessageId::new("m-1"),
        rating,
        note: note.map(str::to_owned),
        version: MessageFeedbackVersion(version.to_owned()),
        created_at: 1,
        updated_at: 2,
    }
}

fn new_controller(remote: Rc<FakeRemote>) -> MessageFeedbackController {
    MessageFeedbackController::new(remote, FeedbackSessionId::new("s-1"))
}

#[test]
fn lazy_shared_load_publishes_status_is_retryable_and_contains_subscriber_panics() {
    let remote = Rc::new(FakeRemote::default());
    let (send, receive) = oneshot::channel();
    remote.lists.borrow_mut().push_back(
        async move {
            receive.await.unwrap();
            Ok(Ok(Ok(vec![item(
                "v1",
                MessageFeedbackRating::Positive,
                None,
            )])))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote.clone());
    let notifications = Rc::new(RefCell::new(0_u32));
    let count = notifications.clone();
    let _subscription = controller.subscribe(Rc::new(move || {
        *count.borrow_mut() += 1;
    }));
    let _panicking = controller.subscribe(Rc::new(|| panic!("contained")));
    block_on(async {
        let first = controller.ensure();
        let second = controller.ensure();
        let release = async move {
            send.send(()).unwrap();
        };
        let (first, second, ()) = futures::join!(first, second, release);
        assert!(first.is_ok());
        assert!(second.is_ok());
    });
    assert_eq!(
        remote
            .calls
            .borrow()
            .iter()
            .filter(|call| matches!(call, Call::List))
            .count(),
        1
    );
    assert_eq!(controller.snapshot().status, MessageFeedbackStatus::Ready);
    assert_eq!(controller.snapshot().items.len(), 1);
    assert!(*notifications.borrow() >= 2);

    let failed = Rc::new(FakeRemote::default());
    failed
        .lists
        .borrow_mut()
        .push_back(async { Err("offline".to_owned()) }.boxed_local());
    failed
        .lists
        .borrow_mut()
        .push_back(async { Ok(Ok(Ok(Vec::new()))) }.boxed_local());
    let controller = new_controller(failed.clone());
    assert!(!block_on(controller.ensure()).is_ok());
    assert_eq!(controller.snapshot().status, MessageFeedbackStatus::Error);
    assert!(block_on(controller.ensure()).is_ok());
    assert_eq!(failed.calls.borrow().len(), 2);
}

#[test]
fn publication_observes_live_set_deletions_and_insertions_in_source_order() {
    let remote = Rc::new(FakeRemote::default());
    remote
        .lists
        .borrow_mut()
        .push_back(async { Ok(Ok(Ok(Vec::new()))) }.boxed_local());
    let controller = new_controller(remote);
    let second = Rc::new(RefCell::new(None::<FeedbackSubscription>));
    let inserted = Rc::new(RefCell::new(Vec::<FeedbackSubscription>::new()));
    let inserted_count = Rc::new(RefCell::new(0_u32));
    let did_insert = Rc::new(RefCell::new(false));
    let first_controller = controller.clone();
    let first_second = second.clone();
    let first_inserted = inserted.clone();
    let first_count = inserted_count.clone();
    let first_did_insert = did_insert.clone();
    let _first = controller.subscribe(Rc::new(move || {
        first_second.borrow_mut().take();
        if !*first_did_insert.borrow() {
            *first_did_insert.borrow_mut() = true;
            let count = first_count.clone();
            first_inserted
                .borrow_mut()
                .push(first_controller.subscribe(Rc::new(move || {
                    *count.borrow_mut() += 1;
                })));
        }
    }));
    let removed_count = Rc::new(RefCell::new(0_u32));
    let count = removed_count.clone();
    *second.borrow_mut() = Some(controller.subscribe(Rc::new(move || {
        *count.borrow_mut() += 1;
    })));

    assert!(block_on(controller.ensure()).is_ok());
    assert_eq!(*removed_count.borrow(), 0);
    assert_eq!(*inserted_count.borrow(), 2);
}

#[test]
fn rate_seeds_preserves_note_and_uses_the_committed_version() {
    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(vec![item(
                "v1",
                MessageFeedbackRating::Positive,
                Some("keep"),
            )])))
        }
        .boxed_local(),
    );
    remote.puts.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(item(
                "v2",
                MessageFeedbackRating::Negative,
                Some("keep"),
            ))))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote.clone());
    assert!(
        block_on(controller.rate(
            FeedbackMessageId::new("m-1"),
            MessageFeedbackRating::Negative,
            None,
        ))
        .is_ok()
    );
    let calls = remote.calls.borrow();
    let Call::Put {
        note,
        if_version,
        rating,
        message_id,
    } = &calls[1]
    else {
        panic!("put")
    };
    assert_eq!(message_id.as_str(), "m-1");
    assert_eq!(*rating, MessageFeedbackRating::Negative);
    assert_eq!(note.as_deref(), Some("keep"));
    assert_eq!(
        if_version.as_ref().map(|version| version.0.as_str()),
        Some("v1")
    );
    assert_eq!(
        controller.snapshot().items[&FeedbackMessageId::new("m-1")]
            .version
            .0,
        "v2"
    );
}

#[test]
fn toggle_clear_note_and_clear_decide_from_committed_state() {
    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(vec![item(
                "v1",
                MessageFeedbackRating::Positive,
                Some("note"),
            )])))
        }
        .boxed_local(),
    );
    remote
        .deletes
        .borrow_mut()
        .push_back(async { Ok(Ok(Ok(()))) }.boxed_local());
    let controller = new_controller(remote.clone());
    assert!(
        block_on(controller.toggle(
            FeedbackMessageId::new("m-1"),
            MessageFeedbackRating::Positive,
        ))
        .is_ok()
    );
    assert!(controller.snapshot().items.is_empty());
    let calls = remote.calls.borrow();
    let Call::Delete {
        message_id,
        if_version,
    } = &calls[1]
    else {
        panic!("delete")
    };
    assert_eq!(message_id.as_str(), "m-1");
    assert_eq!(if_version.0, "v1");
    drop(calls);
    assert!(block_on(controller.clear(FeedbackMessageId::new("missing"))).is_ok());
    assert_eq!(remote.calls.borrow().len(), 2);

    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(vec![item(
                "v1",
                MessageFeedbackRating::Negative,
                Some("remove"),
            )])))
        }
        .boxed_local(),
    );
    remote.puts.borrow_mut().push_back(
        async { Ok(Ok(Ok(item("v2", MessageFeedbackRating::Negative, None)))) }.boxed_local(),
    );
    let controller = new_controller(remote.clone());
    assert!(block_on(controller.clear_note(FeedbackMessageId::new("m-1"))).is_ok());
    let Call::Put { note, .. } = &remote.calls.borrow()[1] else {
        panic!("put")
    };
    assert_eq!(note, &None);
}

#[test]
fn conflicts_reconcile_authoritative_current_while_other_failures_leave_view_untouched() {
    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(vec![item(
                "v1",
                MessageFeedbackRating::Positive,
                None,
            )])))
        }
        .boxed_local(),
    );
    remote.puts.borrow_mut().push_back(
        async {
            Ok(Ok(Err(MessageFeedbackFailure::VersionConflict {
                current: Some(item("v9", MessageFeedbackRating::Negative, None)),
            })))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote);
    let result = block_on(controller.rate(
        FeedbackMessageId::new("m-1"),
        MessageFeedbackRating::Negative,
        None,
    ));
    assert_eq!(
        result,
        MessageFeedbackActionResult::Error {
            code: "version-conflict".to_owned(),
            message: "feedback changed elsewhere".to_owned(),
        }
    );
    assert_eq!(
        controller.snapshot().items[&FeedbackMessageId::new("m-1")]
            .version
            .0,
        "v9"
    );

    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(vec![item(
                "v3",
                MessageFeedbackRating::Positive,
                None,
            )])))
        }
        .boxed_local(),
    );
    remote.puts.borrow_mut().push_back(
        async {
            Ok(Ok(Err(MessageFeedbackFailure::NoteTooLarge {
                max_bytes: 8,
                actual_bytes: 9,
            })))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote);
    assert!(
        !block_on(controller.rate(
            FeedbackMessageId::new("m-1"),
            MessageFeedbackRating::Negative,
            Some("too long".to_owned()),
        ))
        .is_ok()
    );
    assert_eq!(
        controller.snapshot().items[&FeedbackMessageId::new("m-1")]
            .version
            .0,
        "v3"
    );
}

#[test]
fn mutation_queue_and_resync_preserve_reply_order() {
    let remote = Rc::new(FakeRemote::default());
    remote
        .lists
        .borrow_mut()
        .push_back(async { Ok(Ok(Ok(Vec::new()))) }.boxed_local());
    let controller = new_controller(remote.clone());
    assert!(block_on(controller.ensure()).is_ok());
    let (release, gate) = oneshot::channel();
    remote.puts.borrow_mut().push_back(
        async move {
            gate.await.unwrap();
            Ok(Ok(Ok(item("v1", MessageFeedbackRating::Positive, None))))
        }
        .boxed_local(),
    );
    remote.puts.borrow_mut().push_back(
        async { Ok(Ok(Ok(item("v2", MessageFeedbackRating::Negative, None)))) }.boxed_local(),
    );
    block_on(async {
        let first = controller.rate(
            FeedbackMessageId::new("m-1"),
            MessageFeedbackRating::Positive,
            None,
        );
        let second = controller.rate(
            FeedbackMessageId::new("m-1"),
            MessageFeedbackRating::Negative,
            None,
        );
        let unlock = async move {
            release.send(()).unwrap();
        };
        let (first, second, ()) = futures::join!(first, second, unlock);
        assert!(first.is_ok());
        assert!(second.is_ok());
    });
    let calls = remote.calls.borrow();
    let versions = calls
        .iter()
        .filter_map(|call| match call {
            Call::Put { if_version, .. } => {
                Some(if_version.as_ref().map(|version| version.0.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(versions, [None, Some("v1".to_owned())]);
    assert_eq!(
        controller.snapshot().items[&FeedbackMessageId::new("m-1")]
            .version
            .0,
        "v2"
    );
}

#[test]
fn resync_waits_behind_inflight_mutation_before_publishing_its_list() {
    let remote = Rc::new(FakeRemote::default());
    remote
        .lists
        .borrow_mut()
        .push_back(async { Ok(Ok(Ok(Vec::new()))) }.boxed_local());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(vec![item(
                "v2",
                MessageFeedbackRating::Negative,
                None,
            )])))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote.clone());
    assert!(block_on(controller.ensure()).is_ok());
    let (release, gate) = oneshot::channel();
    remote.puts.borrow_mut().push_back(
        async move {
            gate.await.unwrap();
            Ok(Ok(Ok(item("v1", MessageFeedbackRating::Positive, None))))
        }
        .boxed_local(),
    );
    block_on(async {
        let mutation = controller.rate(
            FeedbackMessageId::new("m-1"),
            MessageFeedbackRating::Positive,
            None,
        );
        let resync = controller.resync();
        let unlock = async move {
            release.send(()).unwrap();
        };
        let (mutation, resync, ()) = futures::join!(mutation, resync, unlock);
        assert!(mutation.is_ok());
        assert!(resync.is_ok());
    });
    assert!(matches!(
        remote.calls.borrow().as_slice(),
        [Call::List, Call::Put { .. }, Call::List]
    ));
    assert_eq!(
        controller.snapshot().items[&FeedbackMessageId::new("m-1")]
            .version
            .0,
        "v2"
    );
}

#[test]
fn mutation_failures_settle_keep_the_queue_live_and_only_conflicts_reconcile() {
    let remote = Rc::new(FakeRemote::default());
    remote
        .lists
        .borrow_mut()
        .push_back(async { Ok(Ok(Ok(Vec::new()))) }.boxed_local());
    remote
        .puts
        .borrow_mut()
        .push_back(async { Err("first blew up".to_owned()) }.boxed_local());
    remote.puts.borrow_mut().push_back(
        async { Ok(Ok(Ok(item("v2", MessageFeedbackRating::Negative, None)))) }.boxed_local(),
    );
    let controller = new_controller(remote.clone());
    block_on(async {
        let first = controller.rate(
            FeedbackMessageId::new("m-1"),
            MessageFeedbackRating::Positive,
            None,
        );
        let second = controller.rate(
            FeedbackMessageId::new("m-1"),
            MessageFeedbackRating::Negative,
            None,
        );
        let (first, second) = futures::join!(first, second);
        assert_eq!(
            first,
            MessageFeedbackActionResult::Error {
                code: "transport".to_owned(),
                message: "first blew up".to_owned(),
            }
        );
        assert!(second.is_ok());
    });
    assert_eq!(
        controller.snapshot().items[&FeedbackMessageId::new("m-1")]
            .version
            .0,
        "v2"
    );

    remote.deletes.borrow_mut().push_back(
        async {
            Ok(Err(FeedbackCarrierFailure {
                code: "host-offline".to_owned(),
                message: "Host offline".to_owned(),
            }))
        }
        .boxed_local(),
    );
    remote.deletes.borrow_mut().push_back(
        async {
            Ok(Ok(Err(MessageFeedbackFailure::VersionConflict {
                current: None,
            })))
        }
        .boxed_local(),
    );
    let failed = block_on(controller.clear(FeedbackMessageId::new("m-1")));
    assert!(matches!(
        failed,
        MessageFeedbackActionResult::Error { ref code, .. } if code == "host-offline"
    ));
    assert!(
        controller
            .snapshot()
            .items
            .contains_key(&FeedbackMessageId::new("m-1"))
    );
    let conflict = block_on(controller.clear(FeedbackMessageId::new("m-1")));
    assert!(matches!(
        conflict,
        MessageFeedbackActionResult::Error { ref code, .. } if code == "version-conflict"
    ));
    assert!(controller.snapshot().items.is_empty());
}

#[test]
fn failed_seed_blocks_the_wire_and_late_rejection_after_dispose_is_swallowed() {
    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Err(MessageFeedbackFailure::SessionNotFound {
                session_id: FeedbackSessionId::new("s-1"),
            })))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote.clone());
    let result = block_on(controller.rate(
        FeedbackMessageId::new("m-1"),
        MessageFeedbackRating::Positive,
        None,
    ));
    assert!(matches!(
        result,
        MessageFeedbackActionResult::Error { ref code, .. } if code == "session-not-found"
    ));
    assert_eq!(controller.snapshot().status, MessageFeedbackStatus::Error);
    assert!(
        remote
            .calls
            .borrow()
            .iter()
            .all(|call| !matches!(call, Call::Put { .. }))
    );

    let late = Rc::new(FakeRemote::default());
    let (release, gate) = oneshot::channel();
    late.lists.borrow_mut().push_back(
        async move {
            gate.await.unwrap();
            Err("late".to_owned())
        }
        .boxed_local(),
    );
    let controller = new_controller(late);
    let result = block_on(async {
        let pending = controller.ensure();
        let dispose = async {
            controller.dispose();
            release.send(()).unwrap();
        };
        let (result, ()) = futures::join!(pending, dispose);
        result
    });
    assert!(result.is_ok());
    assert_ne!(controller.snapshot().status, MessageFeedbackStatus::Error);
}

#[test]
fn disposal_refuses_new_and_post_seed_work_but_contains_inflight_publication() {
    let remote = Rc::new(FakeRemote::default());
    let (release, gate) = oneshot::channel();
    remote.lists.borrow_mut().push_back(
        async move {
            gate.await.unwrap();
            Ok(Ok(Ok(Vec::new())))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote.clone());
    let result = block_on(async {
        let mutation = controller.rate(
            FeedbackMessageId::new("m-1"),
            MessageFeedbackRating::Positive,
            None,
        );
        let dispose = async {
            controller.dispose();
            release.send(()).unwrap();
        };
        let (result, ()) = futures::join!(mutation, dispose);
        result
    });
    assert_eq!(
        result,
        MessageFeedbackActionResult::Error {
            code: "disposed".to_owned(),
            message: "feedback controller is disposed".to_owned(),
        }
    );
    assert!(
        remote
            .calls
            .borrow()
            .iter()
            .all(|call| !matches!(call, Call::Put { .. }))
    );
    assert_eq!(controller.snapshot().status, MessageFeedbackStatus::Loading);
}

#[test]
fn no_op_note_opposite_toggle_and_absent_conflict_match_the_committed_view() {
    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(vec![item(
                "v1",
                MessageFeedbackRating::Positive,
                None,
            )])))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote.clone());
    assert!(block_on(controller.clear_note(FeedbackMessageId::new("m-1"))).is_ok());
    assert_eq!(remote.calls.borrow().len(), 1);

    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(vec![item(
                "v1",
                MessageFeedbackRating::Positive,
                Some("keep"),
            )])))
        }
        .boxed_local(),
    );
    remote.puts.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(item(
                "v2",
                MessageFeedbackRating::Negative,
                Some("keep"),
            ))))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote.clone());
    assert!(
        block_on(controller.toggle(
            FeedbackMessageId::new("m-1"),
            MessageFeedbackRating::Negative,
        ))
        .is_ok()
    );
    let Call::Put {
        note, if_version, ..
    } = &remote.calls.borrow()[1]
    else {
        panic!("put")
    };
    assert_eq!(note.as_deref(), Some("keep"));
    assert_eq!(
        if_version.as_ref().map(|version| version.0.as_str()),
        Some("v1")
    );

    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(vec![item(
                "v1",
                MessageFeedbackRating::Positive,
                None,
            )])))
        }
        .boxed_local(),
    );
    remote.deletes.borrow_mut().push_back(
        async {
            Ok(Ok(Err(MessageFeedbackFailure::VersionConflict {
                current: None,
            })))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote);
    let result = block_on(controller.clear(FeedbackMessageId::new("m-1")));
    assert_eq!(
        result,
        MessageFeedbackActionResult::Error {
            code: "version-conflict".to_owned(),
            message: "feedback changed elsewhere".to_owned(),
        }
    );
    assert!(controller.snapshot().items.is_empty());
}

#[test]
#[allow(clippy::too_many_lines)]
fn carrier_transport_and_business_failures_settle_without_corrupting_the_view() {
    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Err(FeedbackCarrierFailure {
                code: "carrier-closed".to_owned(),
                message: "socket closed".to_owned(),
            }))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote);
    assert_eq!(
        block_on(controller.ensure()),
        MessageFeedbackActionResult::Error {
            code: "carrier-closed".to_owned(),
            message: "socket closed".to_owned(),
        }
    );
    assert_eq!(controller.snapshot().status, MessageFeedbackStatus::Error);
    assert_eq!(
        controller.snapshot().error.as_deref(),
        Some("socket closed")
    );

    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Err(MessageFeedbackFailure::SessionNotFound {
                session_id: FeedbackSessionId::new("s-1"),
            })))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote);
    assert_eq!(
        block_on(controller.ensure()),
        MessageFeedbackActionResult::Error {
            code: "session-not-found".to_owned(),
            message: "this session is no longer persisted".to_owned(),
        }
    );
    assert_eq!(
        controller.snapshot().error.as_deref(),
        Some("this session is no longer persisted")
    );

    let remote = Rc::new(FakeRemote::default());
    remote
        .lists
        .borrow_mut()
        .push_back(async { Ok(Ok(Ok(Vec::new()))) }.boxed_local());
    remote
        .puts
        .borrow_mut()
        .push_back(async { Err("message feedback mutation failed".to_owned()) }.boxed_local());
    remote.puts.borrow_mut().push_back(
        async { Ok(Ok(Ok(item("v2", MessageFeedbackRating::Negative, None)))) }.boxed_local(),
    );
    let controller = new_controller(remote.clone());
    let (first, second) = block_on(async {
        futures::join!(
            controller.rate(
                FeedbackMessageId::new("m-1"),
                MessageFeedbackRating::Positive,
                None,
            ),
            controller.rate(
                FeedbackMessageId::new("m-1"),
                MessageFeedbackRating::Negative,
                None,
            )
        )
    });
    assert_eq!(
        first,
        MessageFeedbackActionResult::Error {
            code: "transport".to_owned(),
            message: "message feedback mutation failed".to_owned(),
        }
    );
    assert!(second.is_ok());
    assert_eq!(
        controller.snapshot().items[&FeedbackMessageId::new("m-1")].rating,
        MessageFeedbackRating::Negative
    );

    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Err(MessageFeedbackFailure::SessionNotFound {
                session_id: FeedbackSessionId::new("s-1"),
            })))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote.clone());
    assert!(
        !block_on(controller.rate(
            FeedbackMessageId::new("m-1"),
            MessageFeedbackRating::Positive,
            None,
        ))
        .is_ok()
    );
    assert!(
        remote
            .calls
            .borrow()
            .iter()
            .all(|call| !matches!(call, Call::Put { .. }))
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn resync_unsubscribe_and_midflight_disposal_preserve_lifecycle_order() {
    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(vec![item(
                "v1",
                MessageFeedbackRating::Positive,
                None,
            )])))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote.clone());
    assert!(block_on(controller.ensure()).is_ok());
    let notifications = Rc::new(RefCell::new(0_u32));
    let count = notifications.clone();
    let subscription = controller.subscribe(Rc::new(move || {
        *count.borrow_mut() += 1;
    }));
    drop(subscription);
    remote.puts.borrow_mut().push_back(
        async { Ok(Ok(Ok(item("v2", MessageFeedbackRating::Negative, None)))) }.boxed_local(),
    );
    assert!(
        block_on(controller.rate(
            FeedbackMessageId::new("m-1"),
            MessageFeedbackRating::Negative,
            None,
        ))
        .is_ok()
    );
    assert_eq!(*notifications.borrow(), 0);

    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(vec![item(
                "v1",
                MessageFeedbackRating::Positive,
                None,
            )])))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote.clone());
    assert!(block_on(controller.ensure()).is_ok());
    let (release, gate) = oneshot::channel();
    remote.puts.borrow_mut().push_back(
        async move {
            gate.await.unwrap();
            Ok(Ok(Ok(item("v9", MessageFeedbackRating::Negative, None))))
        }
        .boxed_local(),
    );
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(vec![item(
                "v10",
                MessageFeedbackRating::Negative,
                None,
            )])))
        }
        .boxed_local(),
    );
    block_on(async {
        let mutation = controller.rate(
            FeedbackMessageId::new("m-1"),
            MessageFeedbackRating::Negative,
            None,
        );
        let resync = controller.resync();
        let unlock = async move {
            release.send(()).unwrap();
        };
        let (mutation, resync, ()) = futures::join!(mutation, resync, unlock);
        assert!(mutation.is_ok());
        assert!(resync.is_ok());
    });
    assert!(matches!(
        remote.calls.borrow().as_slice(),
        [Call::List, Call::Put { .. }, Call::List]
    ));
    assert_eq!(
        controller.snapshot().items[&FeedbackMessageId::new("m-1")]
            .version
            .0,
        "v10"
    );

    let remote = Rc::new(FakeRemote::default());
    remote.lists.borrow_mut().push_back(
        async {
            Ok(Ok(Ok(vec![item(
                "v1",
                MessageFeedbackRating::Positive,
                None,
            )])))
        }
        .boxed_local(),
    );
    let controller = new_controller(remote.clone());
    assert!(block_on(controller.ensure()).is_ok());
    let (release, gate) = oneshot::channel();
    remote.puts.borrow_mut().push_back(
        async move {
            gate.await.unwrap();
            Ok(Ok(Err(MessageFeedbackFailure::VersionConflict {
                current: Some(item("v2", MessageFeedbackRating::Negative, None)),
            })))
        }
        .boxed_local(),
    );
    let notifications = Rc::new(RefCell::new(0_u32));
    let count = notifications.clone();
    let _subscription = controller.subscribe(Rc::new(move || {
        *count.borrow_mut() += 1;
    }));
    block_on(async {
        let mutation = controller.rate(
            FeedbackMessageId::new("m-1"),
            MessageFeedbackRating::Negative,
            None,
        );
        let dispose = async {
            controller.dispose();
            release.send(()).unwrap();
        };
        let (_result, ()) = futures::join!(mutation, dispose);
    });
    assert_eq!(*notifications.borrow(), 0);
}

#[test]
fn business_descriptions_and_invariant_contract_are_exact() {
    apply_host();
    for (code, message) in [
        ("session-not-found", "this session is no longer persisted"),
        (
            "target-not-found",
            "this message is not a persisted assistant message",
        ),
        ("version-conflict", "feedback changed elsewhere"),
        (
            "note-blank",
            "a note must contain a non-whitespace character",
        ),
        ("note-too-large", "the note is too long"),
        ("custom", "custom"),
    ] {
        assert_eq!(describe(code), message);
    }
    assert_eq!(
        INJECT,
        ["slots", "remote", "remote.messageFeedback", "locale"]
    );
    assert_eq!(INVARIANT_NAME, "client-ui-feedback-invariant");
    assert_eq!(LOCALE_NAMESPACE, "feedback");
    assert_eq!(
        FEEDBACK_EN,
        [
            ("action.like", "Good response"),
            ("action.likeActive", "Remove rating"),
            ("action.dislike", "Bad response"),
            ("action.dislikeActive", "Remove rating"),
            ("note.open", "Add a note"),
            (
                "note.placeholder",
                "What was good, or what went wrong? (optional)",
            ),
            ("note.save", "Save"),
            ("note.cancel", "Cancel"),
            ("note.aria", "Feedback note"),
            (
                "error.conflict",
                "This feedback changed elsewhere; the latest state is shown",
            ),
            ("error.load", "Could not load feedback"),
            ("error.generic", "Could not save feedback"),
        ]
    );
    assert_eq!(
        FEEDBACK_ZH,
        [
            ("action.like", "好的回答"),
            ("action.likeActive", "取消标记"),
            ("action.dislike", "有问题的回答"),
            ("action.dislikeActive", "取消标记"),
            ("note.open", "补充说明"),
            ("note.placeholder", "这条回答哪里好，或哪里有问题？（可选）"),
            ("note.save", "保存"),
            ("note.cancel", "取消"),
            ("note.aria", "反馈说明"),
            ("error.conflict", "这条反馈已在别处改动，已显示最新状态"),
            ("error.load", "反馈状态加载失败"),
            ("error.generic", "反馈保存失败"),
        ]
    );

    let wire = serde_json::json!({
        "code": "brand-new-code",
        "futureDetail": { "retained": true }
    });
    let failure: MessageFeedbackFailure = serde_json::from_value(wire.clone()).unwrap();
    let MessageFeedbackFailure::Unknown { code, fields } = &failure else {
        panic!("unknown wire code must remain explicit")
    };
    assert_eq!(code, "brand-new-code");
    assert_eq!(fields["futureDetail"]["retained"], true);
    assert_eq!(serde_json::to_value(failure).unwrap(), wire);
}
