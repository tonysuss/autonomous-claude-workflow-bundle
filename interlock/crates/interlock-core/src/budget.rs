//! Task budgets beyond the attempt count: wall-clock time and cost, summed
//! over every attempt's session. Each measure stays in its host's own unit
//! (dollars for Claude Code, premium requests for Copilot CLI), so a limit
//! applies only where a host reports that measure.

use interlock_schema::{Attempt, Budget, Spent};

/// What a task's sessions used, summed over its attempts. A measure no
/// attempt reported stays `None`.
pub fn total(attempts: &[Attempt]) -> Spent {
    let mut sum = Spent::default();
    for s in attempts.iter().filter_map(|a| a.spent.as_ref()) {
        sum.wall_ms += s.wall_ms;
        sum.cost_usd = add(sum.cost_usd, s.cost_usd);
        sum.premium_requests = add(sum.premium_requests, s.premium_requests);
        sum.turns = match (sum.turns, s.turns) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
    }
    sum
}

fn add(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    }
}

/// Why the budget is spent, or `None` while every limit has room.
pub fn exhausted(budget: &Budget, spent: &Spent) -> Option<String> {
    let mut why = Vec::new();
    if let Some(max) = budget.max_wall_secs
        && spent.wall_ms >= max.saturating_mul(1000)
    {
        why.push(format!("the wall-clock budget of {max}s is spent ({}s used)", spent.wall_ms / 1000));
    }
    if let (Some(max), Some(used)) = (budget.max_cost_usd, spent.cost_usd)
        && used >= max
    {
        why.push(format!("the cost budget of ${max:.2} is spent (${used:.4} used)"));
    }
    if let (Some(max), Some(used)) = (budget.max_premium_requests, spent.premium_requests)
        && used >= max
    {
        why.push(format!("the budget of {max} premium requests is spent ({used} used)"));
    }
    if why.is_empty() { None } else { Some(format!("budget exhausted: {}", why.join("; "))) }
}

/// Wall-clock milliseconds left, if the budget limits time.
pub fn wall_left_ms(budget: &Budget, spent: &Spent) -> Option<u64> {
    budget.max_wall_secs.map(|max| max.saturating_mul(1000).saturating_sub(spent.wall_ms))
}

/// Dollars left, if the budget limits cost.
pub fn cost_left_usd(budget: &Budget, spent: &Spent) -> Option<f64> {
    budget.max_cost_usd.map(|max| (max - spent.cost_usd.unwrap_or(0.0)).max(0.0))
}
