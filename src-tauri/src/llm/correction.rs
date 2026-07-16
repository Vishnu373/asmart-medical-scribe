//! Post-ASR correction prompt + record parsing (design §6.7). Like `prompt.rs`,
//! this is pure string/JSON work kept out of the native engine so the part that
//! shapes clinical safety — what the model is asked to change, and the guard that
//! a suggestion can only *replace an existing span* — is unit-testable without a
//! model.
//!
//! The pass finds contextual mishearings that the deterministic word-fixer (§6.3)
//! can't: phrases transcribed as fluent-but-wrong English (every word valid, the
//! phrase wrong). The model emits **one JSON record per line** — `{"original",
//! "replacement"}` — so the UI can parse and show each suggestion the instant its
//! line completes (§6.7 "streamed, parse-as-you-go"). `original` is copied verbatim
//! from the transcript; the caller drops any record whose `original` isn't found in
//! the transcript, so a "suggestion" can never become an invention.

use serde::{Deserialize, Serialize};

use super::LlmModel;

/// The correction instruction (design §6.7). Scopes the model to contextual
/// mishearings only — not stylistic rewrites — mirrors the §8.3 anti-fabrication
/// rule (no added/removed clinical facts), and pins the one-record-per-line JSON
/// output so the stream is parseable as it arrives.
pub const CORRECTION_SYSTEM_PROMPT: &str = "You are proofreading a speech-to-text \
transcript of a medical consultation. The transcription sometimes mishears a phrase as \
fluent but wrong English — every word is a valid word, but the phrase is wrong in context \
(for example \"right down beforehand\" for \"right side of forehead\"). Find only these \
contextual mishearings. For each one, output a single JSON object on its own line, in this \
exact shape: {\"original\": \"<the text copied verbatim from the transcript>\", \
\"replacement\": \"<the corrected phrase>\"}. Copy the original text exactly as it appears in \
the transcript, including its spelling, so it can be located. Suggest a replacement only when \
the intended meaning is clear from context; do not rewrite wording that is merely informal or \
conversational, and never add, remove, or infer any symptom, finding, measurement, diagnosis, \
medication, or plan. Output only JSON lines — one per suggestion, nothing else, no commentary. \
If there are no likely mishearings, output nothing at all.";

/// A short worked example transcript (design §6.7). Contains one garbled phrase so
/// the paired [`EXAMPLE_OUTPUT`] teaches the exact record shape and the "copy the
/// span verbatim" behavior.
const EXAMPLE_TRANSCRIPT: &str = "Patient reports a throbbing headache, right down beforehand, \
for three days. Took some tie the null but it didn't help. BP one thirty over eighty.";

/// The ideal output for [`EXAMPLE_TRANSCRIPT`]: two records, one per line, each span
/// copied verbatim. Shows a two-word mishearing ("tie the null" → "Tylenol") and the
/// canonical "right down beforehand" → "right side of forehead".
const EXAMPLE_OUTPUT: &str = "{\"original\": \"right down beforehand\", \"replacement\": \"right side of forehead\"}\n\
{\"original\": \"tie the null\", \"replacement\": \"Tylenol\"}";

/// The user turn: the transcript to proofread, exactly as the clinician left it.
fn user_message(transcript: &str) -> String {
    format!("Transcript:\n\n{}", transcript.trim())
}

/// Wrap the system instruction, the one-shot example, and the real transcript in the
/// model's chat template — the same templating as note generation (§8.3). Correction
/// is a plain suggestion pass (no chain-of-thought), unlike note generation.
pub fn build_prompt(model: LlmModel, transcript: &str) -> String {
    let example_user = user_message(EXAMPLE_TRANSCRIPT);
    let real_user = user_message(transcript);
    match model {
        // Gemma has no system role, so the instruction rides in the first user turn;
        // the BOS is added by the tokenizer (`AddBos::Always`), not the string.
        LlmModel::Gemma => format!(
            "<start_of_turn>user\n{CORRECTION_SYSTEM_PROMPT}\n\n{example_user}<end_of_turn>\n\
             <start_of_turn>model\n{EXAMPLE_OUTPUT}<end_of_turn>\n\
             <start_of_turn>user\n{real_user}<end_of_turn>\n\
             <start_of_turn>model\n"
        ),
    }
}

/// One correction suggestion: a span to replace and its replacement. `original` is
/// copied verbatim from the transcript so the UI can locate the span; the wire event
/// (`correction-suggestion`) is exactly this shape (design §6.7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    pub original: String,
    pub replacement: String,
}

/// Parse one streamed line into a [`Suggestion`], or `None` when the line isn't a
/// complete, well-formed record. Tolerant by design (§6.7 "malformed/partial lines
/// mid-stream must be skipped, not crash the parser"): blank lines, partial JSON, or
/// records missing a field are silently dropped, and empty spans are rejected (an
/// empty `original` can't be located in the transcript).
pub fn parse_line(line: &str) -> Option<Suggestion> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let s: Suggestion = serde_json::from_str(line).ok()?;
    if s.original.is_empty() || s.replacement.is_empty() {
        return None;
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_scopes_to_mishearings_and_json_output() {
        assert!(CORRECTION_SYSTEM_PROMPT.contains("mishears"));
        assert!(CORRECTION_SYSTEM_PROMPT.contains("\"original\""));
        assert!(CORRECTION_SYSTEM_PROMPT.contains("\"replacement\""));
        // Anti-fabrication discipline mirrors §8.3.
        assert!(CORRECTION_SYSTEM_PROMPT.contains("never add, remove, or infer"));
        // The no-suggestions-is-valid rule.
        assert!(CORRECTION_SYSTEM_PROMPT.contains("output nothing"));
    }

    #[test]
    fn example_output_is_parseable_jsonl() {
        // The one-shot example must itself be valid records, line by line — it is
        // what teaches the model the exact shape.
        let records: Vec<_> = EXAMPLE_OUTPUT.lines().filter_map(parse_line).collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].original, "right down beforehand");
        assert_eq!(records[0].replacement, "right side of forehead");
        assert_eq!(records[1].replacement, "Tylenol");
    }

    #[test]
    fn build_prompt_uses_the_gemma_template() {
        let p = build_prompt(LlmModel::Gemma, "cough for two days");
        assert!(p.starts_with("<start_of_turn>user\n"));
        assert!(p.ends_with("<start_of_turn>model\n"));
        assert!(p.contains(CORRECTION_SYSTEM_PROMPT));
        assert!(p.contains(EXAMPLE_OUTPUT));
        assert!(p.contains("cough for two days"));
    }

    #[test]
    fn parse_line_tolerates_blank_partial_and_malformed_lines() {
        assert!(parse_line("").is_none());
        assert!(parse_line("   ").is_none());
        // A partial line mid-stream (JSON not yet closed) must be skipped.
        assert!(parse_line("{\"original\": \"tie the").is_none());
        // Prose the model might emit despite instructions.
        assert!(parse_line("Here are the suggestions:").is_none());
        // Missing a field.
        assert!(parse_line("{\"original\": \"x\"}").is_none());
        // Empty span can't be located → rejected.
        assert!(parse_line("{\"original\": \"\", \"replacement\": \"y\"}").is_none());
    }

    #[test]
    fn parse_line_reads_a_complete_record_with_surrounding_whitespace() {
        let s = parse_line("  {\"original\": \"tie the null\", \"replacement\": \"Tylenol\"}  ")
            .expect("a complete record parses");
        assert_eq!(s.original, "tie the null");
        assert_eq!(s.replacement, "Tylenol");
    }
}
