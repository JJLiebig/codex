//! Reconcile cached account cooldowns with refreshed usage.

use super::AccountUsage;

pub(super) fn reset_at(usage: AccountUsage) -> Option<i64> {
    if usage.five_hour_exhausted || usage.weekly_exhausted {
        return usage.exhausted_until();
    }
    if !usage.included_usage_denied
        && (usage.five_hour_remaining_percent.is_some() || usage.weekly_remaining_percent.is_some())
    {
        return Some(chrono::Utc::now().timestamp());
    }
    None
}
