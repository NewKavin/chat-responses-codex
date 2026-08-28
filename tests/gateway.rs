#[path = "gateway/aggregate.rs"]
mod aggregate;
#[path = "gateway/auth.rs"]
mod auth;
#[path = "gateway/capability_routing.rs"]
mod capability_routing;
#[path = "gateway/capacity_failure_no_cooldown.rs"]
mod capacity_failure_no_cooldown;
#[path = "gateway/chat.rs"]
mod chat;
#[path = "gateway/claude.rs"]
mod claude;
#[path = "gateway/common.rs"]
mod common;
#[path = "gateway/compatibility.rs"]
mod compatibility;
#[path = "gateway/dialect_matrix.rs"]
mod dialect_matrix;
#[path = "gateway/dialect_retry.rs"]
mod dialect_retry;
#[path = "gateway/images.rs"]
mod images;
#[path = "gateway/model_mappings.rs"]
mod model_mappings;
#[path = "gateway/responses.rs"]
mod responses;
#[path = "gateway/responses/reasoning.rs"]
mod responses_reasoning;
#[path = "gateway/route_exhaustion_budget_invariant.rs"]
mod route_exhaustion_budget_invariant;
#[path = "gateway/slow_stream.rs"]
mod slow_stream;
#[path = "gateway/stream_only.rs"]
mod stream_only;
#[path = "gateway/stream_only_learning.rs"]
mod stream_only_learning;
#[path = "gateway/upstream_concurrency_queue.rs"]
mod upstream_concurrency_queue;
#[path = "gateway/upstream_local_gate_fast_fail.rs"]
mod upstream_local_gate_fast_fail;
