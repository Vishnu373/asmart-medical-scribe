//! Crash reporting with PHI fields structurally excluded. Design §10.3. B11.
//!
//! NFR-6 forbids PHI egress; §10.3 allows *technical-only* crash reports. PHI is
//! kept out two ways: (1) by construction — only [`TechnicalContext`] is ever
//! attached, never a transcript/note/label; and (2) defense-in-depth — every event
//! passes through [`scrub_event`], which strips any PHI-named field before send.
//! The scrubber and context are pure and unit-tested here; the Sentry wiring in
//! [`init`] is behind the off-by-default `crash-reporting` feature, so the default
//! build is fully offline (no DSN, nothing sent) and the fragile native build is
//! untouched until a DSN exists.

use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;
use serde_json::Value;

/// PHI field tokens that must never appear in a crash report (§10.3). Matched
/// case-insensitively as a substring, so `soap_data`, `Transcript`, `note_body`,
/// etc. are all caught.
const PHI_KEY_TOKENS: [&str; 5] = ["transcript", "soap", "note", "label", "record"];

/// Technical-only crash context (§10.3). The stack trace and error type come from
/// the reporter; this is the non-PHI metadata we attach alongside. No transcript,
/// note, or patient label — by construction.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TechnicalContext {
    pub app_version: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
}

impl TechnicalContext {
    pub fn current() -> Self {
        Self {
            app_version: env!("CARGO_PKG_VERSION"),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
        }
    }
}

/// Recursively strip any object key that looks like a PHI field. Run on every
/// outgoing event as a backstop: even if a future change attaches a richer
/// payload, a transcript/note/label can never ride along.
pub fn scrub_event(mut value: Value) -> Value {
    scrub_in_place(&mut value);
    value
}

fn scrub_in_place(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| !is_phi_key(key));
            for child in map.values_mut() {
                scrub_in_place(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(scrub_in_place),
        _ => {}
    }
}

fn is_phi_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    PHI_KEY_TOKENS.iter().any(|token| lower.contains(token))
}

/// Strip the user's home directory / username out of an error string before it is
/// written to the on-device log or sent as telemetry (§10.3, 4c). Error strings
/// here are usually file paths (a model failed to resolve/load), and a Windows
/// profile path embeds the account name — PII we don't want in either sink.
/// Best-effort: rewrites a Windows user-profile prefix (`C:\Users\<name>\`) and a
/// Unix home (`/home/<name>/`, `/Users/<name>/`) to `~\` / `~/`, dropping the name.
pub fn sanitize_error(msg: &str) -> String {
    static WIN: OnceLock<Regex> = OnceLock::new();
    static NIX: OnceLock<Regex> = OnceLock::new();
    // `[^\\/]+` = the username segment, up to the next path separator.
    let win = WIN.get_or_init(|| Regex::new(r"(?i)[a-z]:\\Users\\[^\\/]+\\").unwrap());
    let nix = NIX.get_or_init(|| Regex::new(r"(?i)/(?:home|Users)/[^/]+/").unwrap());
    let out = win.replace_all(msg, r"~\");
    nix.replace_all(&out, "~/").into_owned()
}

/// Initialize crash reporting (§10.3). Disabled unless built with the
/// `crash-reporting` feature *and* a DSN is set in `MEDSCRIBE_CRASH_DSN`, so the
/// default offline build sends nothing (NFR-6). When enabled, PII is off and every
/// event is scrubbed before send.
#[cfg(feature = "crash-reporting")]
pub fn init() {
    // Baked in at compile time (`option_env!`), not read at runtime: the tester's
    // machine has no `MEDSCRIBE_CRASH_DSN` set, so the DSN must be embedded during
    // the build. A DSN is a send-only client ingest key, safe to ship in the binary.
    let dsn = option_env!("MEDSCRIBE_CRASH_DSN").unwrap_or_default();
    if dsn.is_empty() {
        return;
    }
    let ctx = TechnicalContext::current();
    let guard = sentry::init((
        dsn,
        sentry::ClientOptions {
            release: Some(ctx.app_version.into()),
            send_default_pii: false,
            before_send: Some(std::sync::Arc::new(|event| {
                // Serialize → scrub → deserialize. If the round-trip fails we drop
                // the event rather than risk sending it unscrubbed — but log it, so
                // a scrubbed event that can't re-deserialize into `sentry::Event`
                // doesn't take crash reporting 100% dark while still looking enabled.
                let scrubbed = match serde_json::to_value(&event) {
                    Ok(v) => scrub_event(v),
                    Err(e) => {
                        log::warn!("crash report dropped: serialize failed: {e}");
                        return None;
                    }
                };
                match serde_json::from_value(scrubbed) {
                    Ok(event) => Some(event),
                    Err(e) => {
                        log::warn!("crash report dropped: scrubbed event failed to round-trip: {e}");
                        None
                    }
                }
            })),
            ..Default::default()
        },
    ));
    // Outlive `setup`: the client lives for the whole process (it flushes pending
    // events on its own). NOTE: pending first build with the feature enabled.
    std::mem::forget(guard);
}

/// Offline default: crash reporting compiled out (NFR-6 — no network egress).
#[cfg(not(feature = "crash-reporting"))]
pub fn init() {}

/// Submit a doctor-typed "report a problem" message through the same seam as
/// crashes (§10.3): an info-level event with only [`TechnicalContext`] attached,
/// run through the same `before_send` scrub backstop in [`init`]. Sent only when
/// built with `crash-reporting` *and* a DSN is set (else `capture_message` has no
/// client and no-ops). CAVEAT: the body is free text the clinician typed, so it
/// can't be scrubbed for PHI — the UI warns against including patient info.
#[cfg(feature = "crash-reporting")]
pub fn report_feedback(message: &str) {
    let ctx = TechnicalContext::current();
    sentry::with_scope(
        |scope| {
            scope.set_extra("app_version", ctx.app_version.into());
            scope.set_extra("os", ctx.os.into());
            scope.set_extra("arch", ctx.arch.into());
        },
        || {
            sentry::capture_message(message, sentry::Level::Info);
        },
    );
}

/// Offline default: feedback isn't sent anywhere, just logged locally (NFR-6).
#[cfg(not(feature = "crash-reporting"))]
pub fn report_feedback(message: &str) {
    log::info!("feedback (crash reporting disabled, not sent): {message}");
}

/// Record a deliberate, PHI-free product event (implementation.md §3) through the
/// same seam as crashes: an info-level message named `name` with [`TechnicalContext`]
/// plus the caller's `props`, run through the same `before_send` scrub backstop as
/// every other event. Sent only when built with `crash-reporting` *and* a DSN is set
/// (else `capture_message` has no client and no-ops). Call sites must pass only
/// technical values (tier, counts, booleans) — never a transcript, note, or label.
#[cfg(feature = "crash-reporting")]
pub fn track_event(name: &str, props: Value) {
    let ctx = TechnicalContext::current();
    sentry::with_scope(
        |scope| {
            scope.set_extra("app_version", ctx.app_version.into());
            scope.set_extra("os", ctx.os.into());
            scope.set_extra("arch", ctx.arch.into());
            scope.set_extra("props", props);
        },
        || {
            sentry::capture_message(name, sentry::Level::Info);
        },
    );
}

/// Offline default: events aren't sent anywhere, just logged locally (NFR-6).
#[cfg(not(feature = "crash-reporting"))]
pub fn track_event(name: &str, props: Value) {
    log::info!("event {name}: {props}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn technical_context_carries_no_phi() {
        let json = serde_json::to_string(&TechnicalContext::current()).unwrap();
        for token in PHI_KEY_TOKENS {
            assert!(!json.contains(token), "technical context leaked `{token}`");
        }
    }

    #[test]
    fn scrub_removes_phi_fields_at_any_depth() {
        let event = json!({
            "exception": "panic",
            "extra": {
                "transcript": "patient says...",
                "soap_data": "## Subjective\n...",
                "label": "John Doe 1990",
                "record_id": "r1",
                "os": "windows"
            },
            "breadcrumbs": [{ "message": "ok", "note": "secret" }]
        });
        let scrubbed = scrub_event(event);

        // Technical fields survive.
        assert_eq!(scrubbed["exception"], json!("panic"));
        assert_eq!(scrubbed["extra"]["os"], json!("windows"));

        // Every PHI-named field is gone, including nested inside arrays.
        let dump = scrubbed.to_string();
        for token in PHI_KEY_TOKENS {
            assert!(!dump.contains(token), "scrub missed `{token}`: {dump}");
        }
        assert!(!dump.contains("patient says"));
        assert!(!dump.contains("John Doe"));
    }

    #[test]
    fn sanitize_error_strips_windows_username() {
        let msg = r"failed to load LLM model C:\Users\jane.doe\AppData\Roaming\medscribe\gemma.gguf: no such file";
        let out = sanitize_error(msg);
        assert!(!out.contains("jane.doe"), "username leaked: {out}");
        assert!(
            out.contains(r"~\AppData\Roaming\medscribe\gemma.gguf"),
            "{out}"
        );
    }

    #[test]
    fn sanitize_error_strips_unix_home() {
        assert_eq!(
            sanitize_error("open /home/jane/models/x.onnx failed"),
            "open ~/models/x.onnx failed"
        );
        assert_eq!(
            sanitize_error("open /Users/jane/models/x.onnx failed"),
            "open ~/models/x.onnx failed"
        );
    }

    #[test]
    fn sanitize_error_leaves_plain_messages_untouched() {
        assert_eq!(sanitize_error("disk full"), "disk full");
    }
}
