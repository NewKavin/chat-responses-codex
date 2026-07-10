use chat_responses_codex::capabilities::*;

fn job(upstream: &str, model: &str) -> ProbeJob {
    ProbeJob {
        key: DialectProfileKey {
            upstream_id: upstream.into(),
            runtime_model_slug: model.into(),
            protocol: WireProtocol::ChatCompletions,
        },
        reason: ProbeReason::ConfigurationChanged,
    }
}

#[test]
fn queue_deduplicates_and_limits_global_and_per_upstream_work() {
    let mut queue = ProbeQueueState::new(2, 1);
    assert!(queue.enqueue(job("u1", "m1")));
    assert!(!queue.enqueue(job("u1", "m1")));
    assert!(queue.enqueue(job("u1", "m2")));
    assert!(queue.enqueue(job("u2", "m3")));

    let first = queue.start_next().unwrap();
    let second = queue.start_next().unwrap();
    assert_ne!(first.key.upstream_id, second.key.upstream_id);
    assert!(queue.start_next().is_none());

    queue.finish(&first.key);
    assert!(queue.start_next().is_some());
}
