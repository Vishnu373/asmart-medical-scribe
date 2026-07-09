//! Compiled-in beta trial gate (implementation.md §1).
//!
//! The beta hard-stops on a build-time date. On launch the frontend asks the
//! backend for [`status`]; past [`TRIAL_END`] it shows an "expired" screen instead
//! of the app. Fully offline — no server, no login: the date is baked into the
//! binary and compared against the system clock here in Rust.
//!
//! Accepted risk (implementation.md §1): this trusts the local clock, so a tester
//! could roll their PC date back. Fine for a small, trusted beta — there's no
//! server, so there's no way to prevent it, and it isn't worth building one.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Last day the beta is usable, inclusive, as `YYYY-MM-DD` (UTC). Baked into the
/// binary — bump this and rebuild to extend the trial.
const TRIAL_END: &str = "2026-07-31";

/// Trial verdict handed to the frontend (`trial_status` command).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TrialStatus {
    /// True once the current (UTC) date is past [`TRIAL_END`] — the app blocks.
    pub expired: bool,
    /// [`TRIAL_END`], echoed so the expired screen can name the date.
    pub end_date: &'static str,
}

/// Guard for state-changing commands (implementation.md §1, #2). Rejects once the
/// trial has expired, so the backend can't be driven past [`TRIAL_END`] even if the
/// frontend gate is bypassed (e.g. `invoke` called directly). Fails open on a
/// broken clock, matching [`status`].
pub fn ensure_active() -> Result<(), String> {
    if status().expired {
        return Err(format!("This beta ended on {TRIAL_END}."));
    }
    Ok(())
}

/// Compute the trial verdict from the system clock. Compared lexicographically,
/// which is correct for fixed-width `YYYY-MM-DD` — the last usable day is
/// `TRIAL_END` itself; the day after, `today > TRIAL_END` and the app blocks.
pub fn status() -> TrialStatus {
    TrialStatus {
        expired: today_utc().as_deref() > Some(TRIAL_END),
        end_date: TRIAL_END,
    }
}

/// Today's UTC date as `YYYY-MM-DD`. `None` if the clock predates the epoch
/// (treated as not-expired by the caller, so a broken clock never locks a tester
/// out). UTC, not local time — a day-boundary skew is within the §1 accepted risk.
fn today_utc() -> Option<String> {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// Days since the Unix epoch → `(year, month, day)`, proleptic Gregorian. Howard
/// Hinnant's `civil_from_days` (public domain). Avoids pulling in a date crate for
/// one conversion.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1)); // Unix epoch
        assert_eq!(civil_from_days(19_723), (2024, 1, 1)); // leap year
        assert_eq!(civil_from_days(20_665), (2026, 7, 31)); // TRIAL_END
        assert_eq!(civil_from_days(20_666), (2026, 8, 1)); // day after
    }

    #[test]
    fn expiry_is_lexicographic_on_trial_end() {
        // Sanity-check the comparison used by `status`: usable through TRIAL_END,
        // blocked the day after.
        assert!(Some("2026-07-31") <= Some(TRIAL_END)); // last usable day
        assert!(Some("2026-08-01") > Some(TRIAL_END)); // expired
        assert!(Some("2026-07-30") <= Some(TRIAL_END));
    }
}
