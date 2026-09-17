use std::collections::HashMap;

/// 费用归属：一次请求的花费记在哪个预算账本上。
///
/// 有门户归属用户的 Key 记在用户账本上（同一用户的所有 Key 共用一份日预算）；
/// 没有归属用户的直连 Key 记在它自己的账本上。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CostScope {
    pub scope_id: String,
    pub daily_limit_cents: Option<u64>,
}

impl CostScope {
    /// 该账本是否配置了日费用上限。没有上限就不维护滚动窗口。
    pub fn is_limited(&self) -> bool {
        self.daily_limit_cents.is_some_and(|limit| limit > 0)
    }
}

pub fn resolve_cost_scope(
    downstream_id: &str,
    owners: &HashMap<String, String>,
    limits: &HashMap<String, u64>,
) -> CostScope {
    let scope_id = owners
        .get(downstream_id)
        .cloned()
        .unwrap_or_else(|| downstream_id.to_string());
    let daily_limit_cents = limits.get(&scope_id).copied().filter(|limit| *limit > 0);
    CostScope {
        scope_id,
        daily_limit_cents,
    }
}

#[cfg(test)]
#[path = "../../tests/unit/state/cost_scope.rs"]
mod tests;
