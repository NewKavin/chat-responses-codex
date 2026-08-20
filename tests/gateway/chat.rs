use super::common::*;
use serde_json::json;

#[path = "chat/support.rs"]
mod support;

use support::{
    capture_single_chat_request, capture_single_chat_request_with_options,
    capture_single_chat_request_with_profile,
};

#[path = "chat/compatibility.rs"]
mod compatibility;
#[path = "chat/context.rs"]
mod context;
#[path = "chat/core.rs"]
mod core;
#[path = "chat/feedback.rs"]
mod feedback;
#[path = "chat/half_open_busy_ledger.rs"]
mod half_open_busy_ledger;
#[path = "chat/half_open_verdict.rs"]
mod half_open_verdict;
#[path = "chat/rate_limits.rs"]
mod rate_limits;
#[path = "chat/routing.rs"]
mod routing;
#[path = "chat/streaming.rs"]
mod streaming;
