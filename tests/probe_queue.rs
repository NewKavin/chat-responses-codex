use chat_responses_codex::capabilities::*;
use chat_responses_codex::server::{probe_plan_for_job, CoreProbeCase};
use std::sync::Arc;

fn plan_configuration(revision: u64) -> Arc<CompiledCapabilityConfiguration> {
    Arc::new(
        CapabilityConfiguration {
            revision,
            ..CapabilityConfiguration::default()
        }
        .compile()
        .unwrap(),
    )
}

fn plan_configuration_for_alias(
    revision: u64,
    exposed_model: &str,
    token_limit_field: TokenLimitField,
) -> Arc<CompiledCapabilityConfiguration> {
    Arc::new(
        CapabilityConfiguration {
            revision,
            policies: vec![CapabilityPolicy {
                id: format!("policy-{revision}"),
                selector: CapabilitySelector {
                    exposed_model: Some(exposed_model.into()),
                    ..CapabilitySelector::default()
                },
                probe_candidates: ProbeCandidates {
                    token_limit_fields: vec![token_limit_field],
                    ..ProbeCandidates::default()
                },
                ..CapabilityPolicy::default()
            }],
            ..CapabilityConfiguration::default()
        }
        .compile()
        .unwrap(),
    )
}

fn job(upstream: &str, model: &str) -> ProbeJob {
    ProbeJob {
        key: DialectProfileKey {
            key_fingerprint: String::new(),
            upstream_id: upstream.into(),
            runtime_model_slug: model.into(),
            protocol: WireProtocol::ChatCompletions,
        },
        exposed_model_slugs: std::collections::BTreeSet::from([model.into()]),
        reason: ProbeReason::ConfigurationChanged,
        configuration: ProbeConfigurationBinding {
            configuration_fingerprint: "test-fingerprint".into(),
            configuration_digest: "test-digest".into(),
            configuration_schema_version: 1,
            configuration_revision: 1,
            probe_schema_version: DIALECT_PROBE_SCHEMA_VERSION,
        },
        plan_configuration: plan_configuration(1),
    }
}

#[test]
fn duplicate_profile_jobs_merge_exposed_aliases() {
    let mut queue = ProbeQueueState::new(1, 1, usize::MAX);
    assert!(queue.enqueue(job("u1", "alias-a")));
    let mut second = job("u1", "alias-a");
    second.exposed_model_slugs = std::collections::BTreeSet::from(["alias-b".into()]);

    assert!(!queue.enqueue(second));

    let merged = queue.start_next().unwrap();
    assert_eq!(
        merged.exposed_model_slugs,
        std::collections::BTreeSet::from(["alias-a".into(), "alias-b".into()])
    );
}

#[test]
fn duplicate_pending_job_replaces_its_captured_plan_configuration() {
    let mut queue = ProbeQueueState::new(1, 1, usize::MAX);
    let configuration_a =
        plan_configuration_for_alias(1, "alias-a", TokenLimitField::MaxCompletionTokens);
    let mut first = job("u1", "runtime-model");
    first.exposed_model_slugs = std::collections::BTreeSet::from(["alias-a".into()]);
    first.configuration.configuration_fingerprint = "fingerprint-a".into();
    first.configuration.configuration_digest = configuration_a.digest().into();
    first.plan_configuration = configuration_a;
    assert!(queue.enqueue(first));

    let replacement_configuration =
        plan_configuration_for_alias(2, "alias-b", TokenLimitField::MaxOutputTokens);
    let mut replacement = job("u1", "runtime-model");
    replacement.exposed_model_slugs = std::collections::BTreeSet::from(["alias-b".into()]);
    replacement.configuration.configuration_fingerprint = "fingerprint-b".into();
    replacement.configuration.configuration_digest = replacement_configuration.digest().into();
    replacement.configuration.configuration_revision = 2;
    replacement.plan_configuration = replacement_configuration.clone();

    assert!(!queue.enqueue(replacement));

    let merged = queue.start_next().unwrap();
    assert_eq!(
        merged.exposed_model_slugs,
        std::collections::BTreeSet::from(["alias-b".into()])
    );
    assert_eq!(
        merged.configuration.configuration_fingerprint,
        "fingerprint-b"
    );
    assert!(Arc::ptr_eq(
        &merged.plan_configuration,
        &replacement_configuration
    ));
    let plan = probe_plan_for_job(&merged);
    assert!(plan.cases.iter().any(|case| matches!(
        case,
        CoreProbeCase::TokenLimit {
            field: TokenLimitField::MaxOutputTokens
        }
    )));
    assert!(!plan.cases.iter().any(|case| matches!(
        case,
        CoreProbeCase::TokenLimit {
            field: TokenLimitField::MaxCompletionTokens
        }
    )));
}

#[test]
fn alias_arriving_while_profile_is_active_schedules_follow_up() {
    let mut queue = ProbeQueueState::new(2, 2, usize::MAX);
    assert!(queue.enqueue(job("u1", "plain-alias")));
    let active = queue.start_next().unwrap();
    let mut vision = job("u1", "plain-alias");
    vision.exposed_model_slugs = std::collections::BTreeSet::from(["vision-alias".into()]);

    assert!(queue.enqueue(vision));
    assert!(queue.start_next().is_none());

    queue.finish(&active.key);
    let follow_up = queue.start_next().unwrap();
    assert_eq!(
        follow_up.exposed_model_slugs,
        std::collections::BTreeSet::from(["vision-alias".into()])
    );
}

#[test]
fn active_job_deduplicates_identical_binding_but_keeps_changed_follow_up() {
    let mut queue = ProbeQueueState::new(2, 2, usize::MAX);
    let first = job("u1", "active-model");
    assert!(queue.enqueue(first.clone()));
    let active = queue.start_next().unwrap();

    assert!(!queue.enqueue(first));

    let mut changed = job("u1", "active-model");
    changed.configuration.configuration_fingerprint = "changed-fingerprint".into();
    changed.configuration.configuration_revision = 2;
    assert!(queue.enqueue(changed.clone()));
    assert!(!queue.enqueue(changed.clone()));
    queue.finish(&active.key);
    let follow_up = queue.start_next().unwrap();
    assert!(!queue.enqueue(changed));
    queue.finish(&follow_up.key);
    assert!(queue.start_next().is_none());
}

#[test]
fn queue_deduplicates_and_limits_global_and_per_upstream_work() {
    let mut queue = ProbeQueueState::new(2, 1, usize::MAX);
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

#[test]
fn queue_limits_pending_jobs_per_reconcile_batch() {
    let mut queue = ProbeQueueState::new(1, 1, 2);
    assert!(queue.enqueue(job("u1", "m1")));
    assert!(queue.enqueue(job("u1", "m2")));
    assert!(!queue.enqueue(job("u1", "m3")));
    assert_eq!(queue.pending_len(), 2);

    let first = queue.start_next().unwrap();
    queue.finish(&first.key);
    let second = queue.start_next().unwrap();
    assert_eq!(second.key.runtime_model_slug, "m2");
    queue.finish(&second.key);
    assert!(queue.start_next().is_none());
}

#[tokio::test]
async fn ingress_capacity_counts_atomic_submission_batches_not_jobs() {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    sender
        .try_send(ProbeJobBatch::new(vec![job("u1", "m1"), job("u1", "m2")]))
        .unwrap();

    assert_eq!(sender.capacity(), 0);
    let batch = receiver.recv().await.unwrap();
    assert_eq!(batch.jobs().len(), 2);
    assert_eq!(sender.capacity(), 1);
}
