//! Model residency strategy: RAM probe and co-resident-vs-swap decision. Design §7. B9.
//!
//! Two sizable CPU models share one machine: the STT model used while recording
//! and the LLM used after Stop. On a roomy box both stay warm (zero-latency
//! hand-off); on a tight box keeping both resident risks OS disk paging, so we
//! load one at a time. This module decides which regime to use from the machine's
//! **total physical RAM** — a stable per-device property — and caches the result.
//!
//! Only the one-time *mode* decision lives here; run-time load/unload is the
//! lifecycle's job (STT engine idle-watcher, B5; LLM hand-off, B10).

use crate::settings::Settings;

const GIB: u64 = 1024 * 1024 * 1024;

/// Version of the footprint formula below. The cached decision is derived from
/// RAM *and* these constants, so bump this whenever any footprint constant changes
/// — otherwise a new build with corrected sizes would trust a stale cache on
/// unchanged hardware and silently keep the wrong mode (§7).
const RESIDENCY_CALC_VERSION: u32 = 1;

// Footprint inputs (§7 "feasibility calculation"). These are design-target
// *estimates* to validate during benchmarking (design §3 "numbers are targets"),
// kept as named constants so the residency logic holds whatever the real sizes
// turn out to be.
//
// STT: the default Parakeet TDT 0.6B engine, resident.
const STT_FOOTPRINT: u64 = 5 * GIB / 2; // ~2.5 GB
// LLM defaults, keyed on total RAM per the model-choice policy (§8.2): ≥16 GB →
// Mistral-7B Q4_K_M (~4.4 GB), <16 GB → Phi-3.5-mini Q8_0 (~4.0 GB).
const MISTRAL_7B_Q4_FOOTPRINT: u64 = 22 * GIB / 5; // ~4.4 GB
const PHI35_Q8_FOOTPRINT: u64 = 4 * GIB; // ~4.0 GB
// Reserve for the app, webview, and OS on top of the two models (§7: ~2–3 GB);
// take the conservative upper end.
const HEADROOM: u64 = 3 * GIB;
// Required free buffer above the combined footprint before we trust co-residency
// (§7 "margin, not bare fit"): a bare fit invites paging under any pressure.
const CO_RESIDENT_MARGIN: u64 = 2 * GIB;
// The model-choice threshold (§8.2): at/above this, the larger LLM is selected.
const LLM_MODEL_THRESHOLD: u64 = 16 * GIB;

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

/// The default LLM footprint for this machine per the model-choice policy (§8.2).
/// The residency calc needs the *size* of whichever LLM will be selected; the
/// model's identity and loading are B10's concern.
fn default_llm_footprint(total_ram: u64) -> u64 {
    if total_ram >= LLM_MODEL_THRESHOLD {
        MISTRAL_7B_Q4_FOOTPRINT
    } else {
        PHI35_Q8_FOOTPRINT
    }
}

/// The pure feasibility calculation (§7): co-resident only when total RAM clears
/// the combined footprint *plus* the safety margin; otherwise swap.
pub fn decide_mode(total_ram: u64, llm_footprint: u64) -> ResidencyMode {
    let footprint = STT_FOOTPRINT + llm_footprint + HEADROOM;
    if total_ram >= footprint + CO_RESIDENT_MARGIN {
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
    //    footprint formula are unchanged — the cache is keyed on its inputs (RAM)
    //    and the formula version, so a build with corrected sizes re-decides.
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

    // 3. (Re)decide and cache alongside the RAM value and formula version it was
    //    decided from.
    let mode = decide_mode(total_ram, default_llm_footprint(total_ram));
    settings.residency_mode = Some(mode.as_str().to_string());
    settings.observed_total_ram = Some(total_ram);
    settings.residency_calc_version = Some(RESIDENCY_CALC_VERSION);
    (mode, true)
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
    fn decision_boundaries_8_16_32_gib() {
        // 8 GB: below the combined footprint + margin → swap.
        assert_eq!(
            resolve(&mut Settings::default(), 8 * GIB).0,
            ResidencyMode::Swap
        );
        // 16 GB: clears footprint + 2 GB margin with the larger LLM → co-resident.
        assert_eq!(
            resolve(&mut Settings::default(), 16 * GIB).0,
            ResidencyMode::CoResident
        );
        // 32 GB: comfortable margin → co-resident.
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
        // the footprint constants were corrected since). The RAM matches, yet the
        // cache must NOT be trusted — re-decide rather than keep a stale mode.
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
