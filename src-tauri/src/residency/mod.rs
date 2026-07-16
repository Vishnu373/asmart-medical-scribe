//! Model residency strategy: RAM probe and co-resident-vs-swap decision. Design §7. B9.
//!
//! Two sizable CPU models share one machine: the STT model used while recording
//! and the LLM used after Stop. On a roomy box both stay warm (zero-latency
//! hand-off); on a tight box keeping both resident risks OS disk paging, so we
//! load one at a time. With a single small note model (§3), the regime follows a
//! plain **total physical RAM** threshold — a stable per-device property — rather
//! than a per-model footprint sum; the result is cached.
//!
//! Only the one-time *mode* decision lives here; run-time load/unload is the
//! lifecycle's job (STT engine idle-watcher, B5; LLM hand-off, B10).

use crate::settings::Settings;

const GIB: u64 = 1024 * 1024 * 1024;

/// Version of the residency decision. The cached decision is derived from RAM *and*
/// this rule, so bump it whenever the rule changes (e.g. the threshold below),
/// otherwise a new build would trust a stale cache on unchanged hardware and keep
/// the wrong mode (§7). Bumped 2 → 3 when the footprint-sum calc was replaced by
/// this flat RAM threshold.
const RESIDENCY_CALC_VERSION: u32 = 3;

/// Total-RAM cutoff for co-residency (§7). At/above this we keep both the STT and
/// the single note model warm; below it (e.g. an 8 GB box) we swap one in at the
/// hand-off. Compared against *total physical* RAM ([`probe_total_ram`]), a stable
/// per-device property — never momentary available RAM.
const CO_RESIDENT_MIN_RAM: u64 = 16 * GIB;

/// The chosen residency regime (§7 output flag).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResidencyMode {
    /// Both STT and LLM stay warm in RAM throughout a session.
    CoResident,
    /// One model resident at a time; the LLM loads at the transcription→generation
    /// hand-off, trading a few seconds of latency for a lower peak footprint.
    Swap,
}

impl ResidencyMode {
    /// Persisted form stored in the settings JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            ResidencyMode::CoResident => "co_resident",
            ResidencyMode::Swap => "swap",
        }
    }

    /// Parse the persisted/override form; unknown strings yield `None` so a typo
    /// falls back to the automatic decision rather than silently mis-deciding.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "co_resident" => Some(ResidencyMode::CoResident),
            "swap" => Some(ResidencyMode::Swap),
            _ => None,
        }
    }
}

/// The pure feasibility decision (§7): co-resident at/above the total-RAM cutoff,
/// otherwise swap. A single small note model makes a flat threshold sufficient —
/// no per-model footprint sum.
pub fn decide_mode(total_ram: u64) -> ResidencyMode {
    if total_ram >= CO_RESIDENT_MIN_RAM {
        ResidencyMode::CoResident
    } else {
        ResidencyMode::Swap
    }
}

/// Resolve the effective residency mode, updating `settings` with the cached
/// decision when it is (re)computed. Precedence (§7):
///   1. A manual override always wins.
///   2. Otherwise the cached automatic decision, valid only while total RAM is
///      unchanged (a mismatch means the hardware changed → re-decide).
///   3. Otherwise (re)decide from `total_ram` and cache it.
///
/// `total_ram` is supplied by the caller (production: [`probe_total_ram`]); the
/// function itself is pure so the boundary/override paths are unit-testable.
/// Returns whether `settings` was mutated, so the caller knows to persist it.
pub fn resolve(settings: &mut Settings, total_ram: u64) -> (ResidencyMode, bool) {
    // 1. Manual force takes precedence over any automatic decision.
    if let Some(mode) = settings
        .residency_override
        .as_deref()
        .and_then(ResidencyMode::from_str)
    {
        return (mode, false);
    }

    // 2. Cached automatic decision, trusted only while *both* the hardware and the
    //    decision rule are unchanged — the cache is keyed on its input (RAM) and the
    //    rule version, so a build with a changed threshold re-decides.
    if settings.observed_total_ram == Some(total_ram)
        && settings.residency_calc_version == Some(RESIDENCY_CALC_VERSION)
    {
        if let Some(mode) = settings
            .residency_mode
            .as_deref()
            .and_then(ResidencyMode::from_str)
        {
            return (mode, false);
        }
    }

    // 3. (Re)decide and cache alongside the RAM value and rule version it was
    //    decided from.
    let mode = decide_mode(total_ram);
    settings.residency_mode = Some(mode.as_str().to_string());
    settings.observed_total_ram = Some(total_ram);
    settings.residency_calc_version = Some(RESIDENCY_CALC_VERSION);
    (mode, true)
}

/// The effective residency mode from the persisted settings alone, honoring the
/// same precedence [`resolve`] uses — a manual override wins, else the cached
/// automatic decision — but **without** a RAM probe or any mutation. For read-only
/// callers (e.g. the `get_llm_status` command) that only need the current mode, not
/// to (re)decide it. `None` when neither field holds a recognized value, i.e. the
/// mode has not been decided yet.
///
/// This is the single source of truth for that precedence: reading `residency_mode`
/// directly is wrong, because the override path returns early from `resolve` and
/// never writes `residency_mode` (a swap-by-override device leaves it `None`).
pub fn effective_mode(settings: &Settings) -> Option<ResidencyMode> {
    settings
        .residency_override
        .as_deref()
        .and_then(ResidencyMode::from_str)
        .or_else(|| {
            settings
                .residency_mode
                .as_deref()
                .and_then(ResidencyMode::from_str)
        })
}

/// Read the machine's total physical RAM in bytes. Stable per-device, so reading
/// it each launch only validates the cache (§7 "re-probe only if total RAM
/// changes"); we deliberately never sample momentary *available* RAM.
pub fn probe_total_ram() -> u64 {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.total_memory()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_boundaries_around_16_gib() {
        // 8 GB: below the 16 GB cutoff → swap.
        assert_eq!(
            resolve(&mut Settings::default(), 8 * GIB).0,
            ResidencyMode::Swap
        );
        // Just under the cutoff → still swap (threshold is inclusive at 16 GB).
        assert_eq!(
            resolve(&mut Settings::default(), 16 * GIB - 1).0,
            ResidencyMode::Swap
        );
        // Exactly 16 GB → co-resident.
        assert_eq!(
            resolve(&mut Settings::default(), 16 * GIB).0,
            ResidencyMode::CoResident
        );
        // 32 GB: comfortably above → co-resident.
        assert_eq!(
            resolve(&mut Settings::default(), 32 * GIB).0,
            ResidencyMode::CoResident
        );
    }

    #[test]
    fn override_takes_precedence_over_automatic() {
        // A roomy machine would auto-pick co-resident; a Swap override wins and
        // does not overwrite the cache.
        let mut s = Settings::default();
        s.residency_override = Some("swap".to_string());
        let (mode, changed) = resolve(&mut s, 32 * GIB);
        assert_eq!(mode, ResidencyMode::Swap);
        assert!(!changed);
        assert_eq!(s.residency_mode, None);

        // A bogus override is ignored — falls through to the automatic decision.
        let mut s = Settings::default();
        s.residency_override = Some("nonsense".to_string());
        assert_eq!(resolve(&mut s, 32 * GIB).0, ResidencyMode::CoResident);
    }

    #[test]
    fn decision_is_cached_and_reused() {
        let mut s = Settings::default();
        let (first, changed) = resolve(&mut s, 32 * GIB);
        assert!(changed, "first decision is computed and cached");
        assert_eq!(s.observed_total_ram, Some(32 * GIB));
        assert_eq!(s.residency_mode.as_deref(), Some("co_resident"));

        // Same hardware → cached value reused, settings untouched.
        let (second, changed) = resolve(&mut s, 32 * GIB);
        assert_eq!(first, second);
        assert!(!changed);
    }

    #[test]
    fn effective_mode_honors_override_without_a_written_residency_mode() {
        // The bug this guards: a swap override leaves `residency_mode` unset (resolve
        // returns early), so reading it raw reports the wrong mode. `effective_mode`
        // must see the override.
        let mut s = Settings::default();
        s.residency_override = Some("swap".to_string());
        assert_eq!(s.residency_mode, None);
        assert_eq!(effective_mode(&s), Some(ResidencyMode::Swap));

        // Override wins even over a conflicting cached decision.
        let mut s = Settings::default();
        s.residency_override = Some("swap".to_string());
        s.residency_mode = Some("co_resident".to_string());
        assert_eq!(effective_mode(&s), Some(ResidencyMode::Swap));

        // No override → the cached decision; none cached → None (undecided).
        let mut s = Settings::default();
        s.residency_mode = Some("co_resident".to_string());
        assert_eq!(effective_mode(&s), Some(ResidencyMode::CoResident));
        assert_eq!(effective_mode(&Settings::default()), None);
    }

    #[test]
    fn probe_total_ram_returns_plausible_bytes() {
        // Exercises the one production-only line against a sysinfo units/feature
        // regression. Any real machine has ≥1 GiB of RAM, so a KB-scale value (the
        // pre-0.30 unit) would fall below this floor — and a too-small probe would
        // silently force Swap on every install. Bytes clear it comfortably.
        let total = probe_total_ram();
        assert!(
            total >= GIB,
            "total RAM probe returned {total} bytes — implausibly small; \
             did sysinfo's units regress to KB?"
        );
    }

    #[test]
    fn formula_change_retriggers_the_decision() {
        // Cached by an older build (same hardware, but a stale calc version, e.g.
        // the RAM threshold changed since). The RAM matches, yet the cache must NOT
        // be trusted — re-decide rather than keep a stale mode.
        let mut s = Settings::default();
        resolve(&mut s, 32 * GIB);
        s.residency_calc_version = Some(RESIDENCY_CALC_VERSION - 1);
        let (_mode, changed) = resolve(&mut s, 32 * GIB);
        assert!(changed);
        assert_eq!(s.residency_calc_version, Some(RESIDENCY_CALC_VERSION));
    }

    #[test]
    fn hardware_change_retriggers_the_decision() {
        // Cached as a roomy co-resident box...
        let mut s = Settings::default();
        resolve(&mut s, 32 * GIB);
        // ...then the same settings move to a tight machine: RAM mismatch forces a
        // re-decision rather than trusting the stale cache.
        let (mode, changed) = resolve(&mut s, 8 * GIB);
        assert_eq!(mode, ResidencyMode::Swap);
        assert!(changed);
        assert_eq!(s.observed_total_ram, Some(8 * GIB));
    }
}
