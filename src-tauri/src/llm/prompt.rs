//! Few-shot, chain-of-thought SOAP-R prompt construction (design §8.3). Pure string
//! work, kept out of the native engine so the prompt — the part that shapes clinical
//! safety — is unit-testable without loading a model.
//!
//! The verbatim prompt (system instruction + worked few-shot examples) lives in
//! `soap_r_prompt.md` and is embedded at compile time with `include_str!`, so the
//! shipped bytes are exactly the reviewed file — no hand-copied string can drift
//! from it. The examples run in two phases: a private `<think>…</think>` reasoning
//! block (Phase 1) followed by the note (Phase 2); [`REASONING_BOUNDARY`] marks the
//! split and `engine` streams/persists only the note.
//!
//! The prompt is laid out `[system + worked examples] → [transcript] → [assistant]`
//! so the fixed prefix (system + examples) can be prompt-cached (§8.6): the same
//! prefix is reused across every note, and only the transcript changes. A single
//! on-device model (Gemma) with its chat template; the examples teach the format,
//! so nothing model-specific is stated in the instruction itself.

use std::sync::OnceLock;

use super::LlmModel;

/// The verbatim prompt source (design §8.3), embedded at build time so the binary
/// carries exactly these bytes. Sections are delimited by `=== NAME ===` lines:
/// `SYSTEM`, then paired `EXAMPLE_TRANSCRIPT*` / `EXAMPLE_NOTE*` few-shot turns.
const RAW_PROMPT: &str = include_str!("soap_r_prompt.md");

/// The Phase-1 → Phase-2 delimiter used in the supplied prompt: the model first
/// emits its private `<think>…</think>` reasoning, then the note. Generation buffers
/// everything up to and including this marker and streams/persists only what follows
/// (design §8.3/§8.5) — the chain-of-thought is a scratchpad, never shown or stored.
pub const REASONING_BOUNDARY: &str = "</think>";

/// The opening of the Phase-1 reasoning block. Used only to tell two fallback cases
/// apart when [`REASONING_BOUNDARY`] never appears: output that opens a `<think>`
/// block but never closes it is pure scratchpad (must not become the note), whereas
/// output with no `<think>` at all is a model that skipped the two-phase format and
/// produced a plain note (safe to keep). See `engine::decode_and_generate`.
pub const REASONING_OPEN: &str = "<think>";

/// The fixed lead-in of the user turn, before the transcript itself. Kept as a
/// constant so it can sit inside the cacheable [`prefix`] (it never changes) while
/// only the transcript that follows it varies note to note; it is also the split
/// boundary between [`prefix`] and [`transcript_tail`].
const USER_LEAD_IN: &str = "Consultation transcript:\n\n";

/// The parsed `soap_r_prompt.md`: the system instruction and the ordered few-shot
/// `(transcript, note)` pairs.
struct SoapPrompt {
    system: String,
    examples: Vec<(String, String)>,
}

/// Parse [`RAW_PROMPT`] once. Splits on `=== NAME ===` marker lines and keeps the
/// content between them verbatim (only the whitespace hugging each marker is
/// trimmed, so blank lines *inside* a section survive). `EXAMPLE_TRANSCRIPT*` and
/// `EXAMPLE_NOTE*` sections are zipped in file order into few-shot pairs.
fn parsed() -> &'static SoapPrompt {
    static PARSED: OnceLock<SoapPrompt> = OnceLock::new();
    PARSED.get_or_init(|| {
        // Collect (marker, body) sections in file order; `.lines()` drops the CRLF.
        let mut sections: Vec<(String, String)> = Vec::new();
        let mut name: Option<String> = None;
        let mut body = String::new();
        for line in RAW_PROMPT.lines() {
            if let Some(sec) = line
                .strip_prefix("=== ")
                .and_then(|s| s.strip_suffix(" ==="))
            {
                if let Some(n) = name.take() {
                    sections.push((n, body.trim().to_string()));
                    body.clear();
                }
                name = Some(sec.to_string());
            } else {
                body.push_str(line);
                body.push('\n');
            }
        }
        if let Some(n) = name.take() {
            sections.push((n, body.trim().to_string()));
        }

        let mut system = String::new();
        let mut transcripts: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        for (n, b) in sections {
            if n == "SYSTEM" {
                system = b;
            } else if n.starts_with("EXAMPLE_TRANSCRIPT") {
                transcripts.push(b);
            } else if n.starts_with("EXAMPLE_NOTE") {
                notes.push(b);
            }
        }
        // The embedded file is a build-time constant: a malformed layout is a
        // programmer/asset error, so fail loudly rather than ship an empty prompt.
        assert!(
            !system.is_empty(),
            "soap_r_prompt.md is missing its SYSTEM section"
        );
        assert_eq!(
            transcripts.len(),
            notes.len(),
            "soap_r_prompt.md has unpaired EXAMPLE_TRANSCRIPT/EXAMPLE_NOTE sections"
        );
        let examples = transcripts.into_iter().zip(notes).collect();
        SoapPrompt { system, examples }
    })
}

/// The system instruction, verbatim from `soap_r_prompt.md`'s `SYSTEM` section
/// (design §8.3). Pins the five SOAP-R headers, the per-section placement rules, the
/// two-phase reason-then-note format, and — most importantly — forbids anything not
/// in the transcript (a fabricated clinical fact is the worst failure).
pub fn system_prompt() -> &'static str {
    &parsed().system
}

/// The user turn: the transcript exactly as the clinician left it (§8.1), behind the
/// fixed lead-in label.
pub fn user_message(transcript: &str) -> String {
    format!("{USER_LEAD_IN}{}", transcript.trim())
}

/// The fixed prompt **prefix** for a model: system instruction, the worked few-shot
/// examples, and the instruct-template scaffolding up to (and including) the user
/// lead-in — everything before the note's own transcript. This is byte-identical
/// across every note, which is exactly what the KV-cache reuse in §8.6 caches: it is
/// prefilled once and its state restored per note so the prefix is never re-decoded.
/// [`build_prompt`] is `prefix(model) + transcript_tail(model, transcript)`.
pub fn prefix(model: LlmModel) -> String {
    let p = parsed();
    match model {
        // Gemma has no system role, so the instruction rides in the first user turn.
        // The BOS is added by the tokenizer (`AddBos::Always`), so it is not in the
        // string. Each worked example is a completed user→model turn pair before the
        // real transcript's user turn; the final open user turn ends at the lead-in
        // so the transcript is appended as the (prefix-excluded) tail.
        LlmModel::Gemma => {
            let mut s = String::new();
            for (i, (transcript, note)) in p.examples.iter().enumerate() {
                s.push_str("<start_of_turn>user\n");
                if i == 0 {
                    s.push_str(&p.system);
                    s.push_str("\n\n");
                }
                s.push_str(&user_message(transcript));
                s.push_str("<end_of_turn>\n<start_of_turn>model\n");
                s.push_str(note);
                s.push_str("<end_of_turn>\n");
            }
            s.push_str("<start_of_turn>user\n");
            s.push_str(USER_LEAD_IN);
            s
        }
    }
}

/// The changing **tail**: this note's transcript plus the assistant opener that
/// hands the turn to the model. Appended after [`prefix`]; the split boundary sits
/// at the user lead-in (`\n\n`), so the tail begins with the transcript text.
pub fn transcript_tail(model: LlmModel, transcript: &str) -> String {
    let transcript = transcript.trim();
    match model {
        LlmModel::Gemma => format!("{transcript}<end_of_turn>\n<start_of_turn>model\n"),
    }
}

/// Wrap the system, the few-shot examples, and the real transcript in the model's
/// chat template. Split into [`prefix`] (fixed, cacheable §8.6) + [`transcript_tail`]
/// (changing); this is the concatenation and stays the single source of the exact
/// prompt for the fallback (uncached) path.
pub fn build_prompt(model: LlmModel, transcript: &str) -> String {
    format!("{}{}", prefix(model), transcript_tail(model, transcript))
}

/// Deterministically scrub any leftover model-control markers from a finished note
/// (design §8.5). The decode loop already stops the turn at `<end_of_turn>` and drops
/// the reasoning up to the *first* [`REASONING_BOUNDARY`], but a small quantized model
/// sometimes echoes the structural tags again after the note body — a stray trailing
/// `</think>`, or a chat-template control token. None of these may appear in a
/// clinical note, so this belt-and-suspenders pass — a plain string check-and-remove,
/// no model, one pass over the finished note — truncates at the first chat-template
/// turn marker, removes any complete `<think>…</think>` span, drops an unclosed
/// `<think>` to end-of-note, strips any orphan closing tag, and trims. Runs once on
/// the persisted note, so it adds no per-token latency.
pub fn sanitize_note(note: &str) -> String {
    // Anything from a turn marker onward is not part of the note (the model tried to
    // end/restart the turn) — cut it, and everything after.
    let mut note = note;
    for marker in ["<end_of_turn>", "<start_of_turn>"] {
        if let Some(i) = note.find(marker) {
            note = &note[..i];
        }
    }
    let mut out = String::with_capacity(note.len());
    let mut rest = note;
    // Remove complete <think>…</think> spans (a duplicate block after the note body).
    while let Some(open) = rest.find(REASONING_OPEN) {
        out.push_str(&rest[..open]);
        let after = &rest[open + REASONING_OPEN.len()..];
        match after.find(REASONING_BOUNDARY) {
            Some(close) => rest = &after[close + REASONING_BOUNDARY.len()..],
            None => rest = "", // unclosed <think> runs to the end: drop the remainder
        }
    }
    out.push_str(rest);
    // Strip any orphan closing tag with no opener (the reported trailing-tag case).
    out.replace(REASONING_BOUNDARY, "").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_pins_structure_and_anti_hallucination() {
        let sys = system_prompt();
        for header in [
            "## Subjective",
            "## Objective",
            "## Assessment",
            "## Plan",
            "## Response",
        ] {
            assert!(sys.contains(header), "missing {header}");
        }
        // The safety-critical instruction must be present and explicit.
        assert!(sys.contains("only information explicitly stated"));
        assert!(sys.contains("Do not add, assume, or infer"));
        // Empty-section rule ("Not discussed"), and the first-visit Response wording.
        assert!(sys.contains("Not discussed"));
        assert!(sys.contains("First visit"));
        // Concise-bullets instruction.
        assert!(sys.contains("concise bullet points"));
    }

    #[test]
    fn few_shot_examples_are_parsed_and_teach_cot_and_asr_repair() {
        // The supplied prompt carries at least two worked pairs; they ride in the
        // fixed prefix that teaches format/style.
        let p = parsed();
        assert!(
            p.examples.len() >= 2,
            "expected the supplied few-shot pairs"
        );
        let prefix = prefix(LlmModel::Gemma);
        // Two-phase format: each example carries a private reasoning block bounded by
        // the reasoning marker.
        assert!(prefix.contains("<think>"));
        assert!(prefix.contains(REASONING_BOUNDARY));
        // All five headers appear (taught by the example notes).
        for header in [
            "## Subjective",
            "## Objective",
            "## Assessment",
            "## Plan",
            "## Response",
        ] {
            assert!(prefix.contains(header), "example missing {header}");
        }
        // The raw transcript keeps the garble; the note resolves it from context.
        assert!(prefix.contains("right down beforehand"));
        assert!(prefix.contains("right side of forehead"));
        // The first-visit Response wording, from the second example.
        assert!(prefix.contains("First visit — no prior treatment"));
    }

    #[test]
    fn user_message_trims_and_embeds_the_transcript() {
        let m = user_message("  patient reports a cough  ");
        assert!(m.contains("patient reports a cough"));
        assert!(!m.contains("  patient")); // trimmed
    }

    #[test]
    fn prefix_plus_tail_equals_build_prompt() {
        // The KV-cache reuse (§8.6) prefills `prefix` and appends `transcript_tail`;
        // it must reconstruct exactly the fallback prompt or cached and uncached
        // notes would diverge. Guard the split for a few transcripts.
        let model = LlmModel::Gemma;
        for t in ["", "  cough for two days  ", "chest pain\nradiating to arm"] {
            assert_eq!(
                format!("{}{}", prefix(model), transcript_tail(model, t)),
                build_prompt(model, t),
                "prefix+tail != build_prompt for {t:?}"
            );
        }
        // The boundary sits at the user lead-in, so the prefix ends with it and the
        // tail carries no part of it.
        assert!(prefix(model).ends_with("Consultation transcript:\n\n"));
        assert!(!transcript_tail(model, "x").contains("Consultation transcript:"));
    }

    #[test]
    fn sanitize_note_strips_leftover_reasoning_markers() {
        // The reported case: a stray closing tag trailing the note body.
        assert_eq!(
            sanitize_note("## Subjective\n- cough\n</think>"),
            "## Subjective\n- cough"
        );
        // A duplicate <think>…</think> block echoed after the note is removed whole.
        assert_eq!(
            sanitize_note(
                "## Plan\n- rest\n<think>oops more reasoning</think>\n## Response\n- none"
            ),
            "## Plan\n- rest\n\n## Response\n- none"
        );
        // An unclosed <think> to end-of-note drops the trailing scratchpad.
        assert_eq!(
            sanitize_note("## Assessment\n- viral URI\n<think>still thinking"),
            "## Assessment\n- viral URI"
        );
        // A stray chat-template turn marker truncates the note there.
        assert_eq!(
            sanitize_note("## Plan\n- rest<end_of_turn>"),
            "## Plan\n- rest"
        );
        assert_eq!(
            sanitize_note("## Response\n- none</think><end_of_turn>garbage"),
            "## Response\n- none"
        );
        // A clean note is returned unchanged (aside from trimming).
        let clean = "## Subjective\n- headache\n\n## Plan\n- ibuprofen";
        assert_eq!(sanitize_note(clean), clean);
    }

    #[test]
    fn build_prompt_uses_the_gemma_template() {
        let p = build_prompt(LlmModel::Gemma, "cough for two days");
        assert!(p.starts_with("<start_of_turn>user\n"));
        // Ends on an open model turn so the model generates the note next.
        assert!(p.ends_with("<start_of_turn>model\n"));
        // The system instruction and the worked examples both ride in the prefix.
        assert!(p.contains(system_prompt()));
        assert!(p.contains("<end_of_turn>"));
        assert!(p.contains("cough for two days"));
        // All five headers appear (via the examples).
        for header in [
            "## Subjective",
            "## Objective",
            "## Assessment",
            "## Plan",
            "## Response",
        ] {
            assert!(p.contains(header), "missing {header}");
        }
    }
}
