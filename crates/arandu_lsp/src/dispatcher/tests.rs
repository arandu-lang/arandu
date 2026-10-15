use super::*;
use crate::uri_util::parse_uri;
use crate::workspace::WorkspaceFile;
use crossbeam_channel::{bounded, unbounded};

#[test]
fn diagnostics_are_not_scheduled_for_uncommitted_document_text() {
    let mut state = ServerState::new();
    let uri = parse_uri("file:///pending-static.aru").expect("fixture URI");
    let id = state.open_or_commit(&uri, "func main(): int { return 42 }".into());
    state.set_version(id, 1);
    state.queue_change(&uri, "func main(): int { return missing }".into());
    state.set_version(id, 2);

    let pool = WorkerPool::new(1).expect("worker");
    let (started_tx, started_rx) = bounded(1);
    let (release_tx, release_rx) = bounded(1);
    pool.spawn(Priority::Interactive, None, move |_| {
        started_tx.send(()).expect("signal worker barrier");
        release_rx.recv().expect("release worker barrier");
    })
    .expect("enqueue worker barrier");
    started_rx.recv().expect("worker entered barrier");
    let (job_tx, _) = unbounded();
    crate::diagnostics::spawn_diagnostics(&state, &pool, &job_tx, uri, id);
    // Probe the queued key while the worker is blocked, without timing sleeps.
    let scheduled = pool.cancel(&JobKey::Diagnostics(id));
    release_tx.send(()).expect("release worker even on failure");
    assert!(!scheduled, "pending text must not be analyzed as version 2");
}

#[test]
fn pending_document_edits_block_diagnostic_publication_until_commit() {
    let (connection, client) = Connection::memory();
    let mut state = ServerState::new();
    let uri = parse_uri("file:///pending-result.aru").expect("fixture URI");
    let id = state.open_or_commit(&uri, "func main(): int { return 42 }".into());
    state.set_version(id, 1);
    state.queue_change(&uri, "func main(): int { return missing }".into());
    state.set_version(id, 2);
    let revision = state.revision();
    let pool = WorkerPool::new(1).expect("worker");
    let (job_tx, _) = unbounded();
    // A discovery refresh during debounce could previously label committed
    // version-1 text with the latest client version, passing both stale checks.
    handle_job_result(
        &connection,
        &mut state,
        &pool,
        &job_tx,
        JobResult::Diagnostics {
            uri: uri.clone(),
            doc_id: id,
            version: Some(2),
            revision,
            fingerprint: [3; 32],
            diags: Vec::new(),
        },
    )
    .expect("handle uncommitted result");
    assert!(
        client.receiver.try_recv().is_err(),
        "stale result published"
    );
    assert!(!state.last_diag_fp.contains_key(&id));

    assert_eq!(state.flush_all(), vec![(uri.clone(), id)]);
    let (diags, fingerprint) = {
        let snapshot = state.snapshot();
        let source = state.docs.get(id).expect("live document").source;
        crate::diagnostics::compute_diagnostics(&snapshot, source)
    };
    let revision = state.revision();
    handle_job_result(
        &connection,
        &mut state,
        &pool,
        &job_tx,
        JobResult::Diagnostics {
            uri,
            doc_id: id,
            version: Some(2),
            revision,
            fingerprint,
            diags,
        },
    )
    .expect("handle committed result");
    let Message::Notification(message) = client.receiver.try_recv().expect("current diagnostics")
    else {
        panic!("expected diagnostic notification");
    };
    assert_eq!(message.params["version"], serde_json::json!(2));
    assert!(message.params["diagnostics"]
        .as_array()
        .expect("diagnostic array")
        .iter()
        .any(|diagnostic| diagnostic["code"] == "N001"));
}

#[test]
fn discovery_waits_for_interactive_response_delivery() {
    let (connection, client) = Connection::memory();
    let mut state = ServerState::new();
    let uri = parse_uri("file:///open.aru").expect("fixture URI");
    state.open_or_commit(&uri, "func main() {}".into());
    let revision = state.revision();
    let pool = WorkerPool::new(1).expect("worker");
    let (job_tx, job_rx) = unbounded();
    let (workspace_tx, workspace_rx) = bounded(1);
    let (release_tx, release_rx) = bounded(1);
    spawn_json(&mut state, &pool, &job_tx, 1.into(), move |_, _| {
        release_rx.recv().expect("release request");
        serde_json::json!({"items": []})
    });
    workspace_tx
        .send(WorkspaceEvent::File(WorkspaceFile {
            path: std::env::temp_dir().join("arandu-discovered.aru"),
            text: "func discovered() {}".into(),
        }))
        .expect("discovery event");

    // The worker cannot finish until released; discovery is already ready.
    // No sleeps or filesystem speed determine this interleaving.
    let event = next_event(
        &connection,
        &job_rx,
        &workspace_rx,
        !state.pending_requests.is_empty(),
        Duration::ZERO,
    );
    release_tx.send(()).expect("release worker even on failure");
    assert!(
        matches!(event, Event::Timeout),
        "discovery overtook request"
    );

    let Event::Job(Ok(job)) = next_event(
        &connection,
        &job_rx,
        &workspace_rx,
        !state.pending_requests.is_empty(),
        Duration::from_secs(5),
    ) else {
        panic!("request must finish before discovery");
    };
    assert!(
        !state.pending_requests.is_empty(),
        "delivery is still pending"
    );
    handle_job_result(&connection, &mut state, &pool, &job_tx, job).expect("deliver");
    let Message::Response(response) = client.receiver.recv().expect("response") else {
        panic!("expected response");
    };
    assert!(response.response_result.is_ok());
    assert!(state.pending_requests.is_empty());
    assert_eq!(state.revision(), revision);

    let Event::Workspace(Ok(WorkspaceEvent::File(file))) = next_event(
        &connection,
        &job_rx,
        &workspace_rx,
        !state.pending_requests.is_empty(),
        Duration::ZERO,
    ) else {
        panic!("discovery must resume after response delivery");
    };
    crate::workspace::register_workspace_file(&mut state, file);
    assert_ne!(state.revision(), revision);
}

#[test]
fn terminal_errors_and_cancellation_release_discovery_without_publishing_stale_results() {
    enum Outcome {
        Error,
        Panic,
        Cancel,
        Stale,
    }
    for outcome in [
        Outcome::Error,
        Outcome::Panic,
        Outcome::Cancel,
        Outcome::Stale,
    ] {
        let (connection, client) = Connection::memory();
        let mut state = ServerState::new();
        let pool = WorkerPool::new(1).expect("worker");
        let (job_tx, job_rx) = unbounded();
        let (release_tx, release_rx) = bounded(1);
        let (started_tx, started_rx) = bounded(1);
        pool.spawn(Priority::Background, None, move |_| {
            started_tx.send(()).expect("started");
            release_rx.recv().expect("release blocker");
        })
        .expect("block worker");
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("started");
        let cancelled = matches!(outcome, Outcome::Cancel);
        let stale = matches!(outcome, Outcome::Stale);
        spawn_json_result(
            &mut state,
            &pool,
            &job_tx,
            7.into(),
            move |_, _| match outcome {
                Outcome::Error => Err((-32602, "invalid test argument".into())),
                Outcome::Panic => panic!("controlled request panic"),
                Outcome::Cancel | Outcome::Stale => Ok(serde_json::Value::Null),
            },
        );
        if cancelled {
            assert!(pool.cancel(&JobKey::Request(7.into())));
        }
        release_tx.send(()).expect("release worker");
        let job = job_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("terminal result");
        if stale {
            let uri = parse_uri("file:///changed.aru").expect("URI");
            state.open_or_commit(&uri, "func changed() {}".into());
        }
        assert!(!state.pending_requests.is_empty());
        handle_job_result(&connection, &mut state, &pool, &job_tx, job).expect("deliver");
        assert!(
            state.pending_requests.is_empty(),
            "discovery must resume on errors too"
        );
        let Message::Response(response) = client.receiver.recv().expect("response") else {
            panic!("expected response");
        };
        assert!(response.response_result.is_err());
        let response = serde_json::to_value(response).expect("JSON response");
        if stale {
            assert_eq!(
                response.pointer("/error/code"),
                Some(&serde_json::json!(LSP_CONTENT_MODIFIED))
            );
        }
        if cancelled {
            assert_eq!(
                response.pointer("/error/code"),
                Some(&serde_json::json!(LSP_REQUEST_CANCELLED))
            );
        }
    }
}

#[test]
fn paused_discovery_keeps_protocol_and_debounce_live() {
    let (connection, client) = Connection::memory();
    let (_job_tx, job_rx) = unbounded();
    let (workspace_tx, workspace_rx) = bounded(1);
    workspace_tx
        .send(WorkspaceEvent::Done)
        .expect("discovery ready");
    client
        .sender
        .send(Message::Notification(Notification::new(
            "$/cancelRequest".into(),
            serde_json::json!({ "id": 1 }),
        )))
        .expect("client cancellation");
    assert!(matches!(
        next_event(&connection, &job_rx, &workspace_rx, true, Duration::ZERO),
        Event::Protocol(Ok(Message::Notification(_)))
    ));
    assert!(matches!(
        next_event(&connection, &job_rx, &workspace_rx, true, Duration::ZERO),
        Event::Timeout
    ));
}

#[test]
fn package_reload_coalesces_and_commits_after_response_delivery() {
    let (connection, client) = Connection::memory();
    let mut state = ServerState::new();
    let revision = state.revision();
    let pool = WorkerPool::new(1).expect("worker");
    let (job_tx, job_rx) = unbounded();
    let (release_tx, release_rx) = bounded(1);
    spawn_json(&mut state, &pool, &job_tx, 1.into(), move |_, _| {
        release_rx.recv().expect("release request");
        serde_json::Value::Null
    });
    for name in ["first", "latest"] {
        let root = std::env::temp_dir().join("arandu-reload-order");
        let project = crate::workspace::WorkspaceProject {
            manifest_path: root.join("Arandu.toml"),
            manifest_data: arandu_query::ManifestData::legacy(
                name.into(),
                "0.1.0".into(),
                "src/main.aru".into(),
            ),
            manifest_hash: name.into(),
            package_src: root.join("src"),
            entries: Vec::new(),
            stdlib_root: None,
            module_plan: None,
            module_files: Vec::new(),
        };
        handle_job_result(
            &connection,
            &mut state,
            &pool,
            &job_tx,
            JobResult::WorkspaceReload(Ok(Box::new(project))),
        )
        .expect("stage reload");
        apply_deferred_reload(&connection, &mut state, &pool, &job_tx).expect("defer commit");
    }
    assert_eq!(state.revision(), revision);
    assert!(state.package.is_none());
    assert!(client.receiver.try_recv().is_err());
    release_tx.send(()).expect("release worker");
    let job = job_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("response");
    handle_job_result(&connection, &mut state, &pool, &job_tx, job).expect("deliver");
    apply_deferred_reload(&connection, &mut state, &pool, &job_tx).expect("commit reload");
    let Message::Response(response) = client.receiver.recv().expect("response precedes status")
    else {
        panic!("response must precede reload status");
    };
    assert!(response.response_result.is_ok());
    assert_ne!(state.revision(), revision);
    assert_eq!(
        state
            .package
            .as_ref()
            .expect("configured package")
            .package_name,
        "latest"
    );
    assert!(state.deferred_reload.is_none());
}

#[test]
fn requests_awaiting_delivery_are_bounded_and_rejection_does_not_hold_discovery() {
    let (connection, _client) = Connection::memory();
    let mut state = ServerState::new();
    let pool = WorkerPool::new(1).expect("worker");
    let (job_tx, job_rx) = unbounded();
    let (release_tx, release_rx) = bounded(1);
    let (started_tx, started_rx) = bounded(1);
    pool.spawn(Priority::Background, None, move |_| {
        started_tx.send(()).expect("started");
        release_rx.recv().expect("release blocker");
    })
    .expect("block worker");
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("started");
    for id in 0..MAX_PENDING_REQUESTS {
        spawn_json(
            &mut state,
            &pool,
            &job_tx,
            i32::try_from(id).expect("small ID").into(),
            |_, _| serde_json::Value::Null,
        );
    }
    spawn_json(&mut state, &pool, &job_tx, 1000.into(), |_, _| {
        panic!("rejected job ran")
    });
    spawn_json_result(&mut state, &pool, &job_tx, 1001.into(), |_, _| {
        panic!("rejected job ran")
    });
    spawn_goto(
        &mut state,
        &pool,
        &job_tx,
        1002.into(),
        parse_uri("file:///unused.aru").expect("URI"),
        lsp_types::Position::default(),
    );
    // The event loop never sends into the bounded job channel, so saturation
    // rejections are staged on the state and drained by the loop itself.
    assert_eq!(state.deferred_rejections.len(), 3);
    drain_deferred_rejections(&connection, &mut state, &pool, &job_tx).expect("deliver rejection");
    assert!(state.deferred_rejections.is_empty());
    assert_eq!(state.pending_requests.len(), MAX_PENDING_REQUESTS);
    release_tx.send(()).expect("release worker");
    for _ in 0..MAX_PENDING_REQUESTS {
        let job = job_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("admitted response");
        handle_job_result(&connection, &mut state, &pool, &job_tx, job).expect("deliver response");
    }
    assert!(state.pending_requests.is_empty());
}
