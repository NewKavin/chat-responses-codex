use super::common::*;
use serde_json::json;

#[path = "chat/support.rs"]
mod support;

use support::capture_single_chat_request;

#[path = "chat/compatibility.rs"]
mod compatibility;
#[path = "chat/context.rs"]
mod context;
#[path = "chat/core.rs"]
mod core;
#[path = "chat/feedback.rs"]
mod feedback;
#[path = "chat/rate_limits.rs"]
mod rate_limits;
#[path = "chat/routing.rs"]
mod routing;
#[path = "chat/streaming.rs"]
mod streaming;
