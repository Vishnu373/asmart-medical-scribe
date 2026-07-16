//! One-shot SOAP-R prompt construction (design §8.3). Pure string work, kept out
//! of the native engine so the prompt — the part that shapes clinical safety —
//! is unit-testable without loading a model.
//!
//! The prompt is laid out `[system + worked example] → [transcript] → [assistant]`
//! so the fixed prefix (system + example) can be prompt-cached (§8.2): the same
//! prefix is reused across every note, and only the transcript changes. A single
//! on-device model (Gemma) with its chat template; the worked example teaches the
//! format, so nothing model-specific is stated in the instruction itself.
//
// TODO(§5): replace the one-shot example below with the supplied few-shot +
// chain-of-thought prompt, and add the reasoning→note boundary handling.

use super::LlmModel;

/// The system instruction (design §8.3). Pins the five SOAP-R headers, states the
/// per-section placement rules, asks for concise bullets, forbids anything not in
/// the transcript (the single most important safety property — a fabricated
/// clinical fact is the worst failure), and fixes the empty-section wording.
pub const SOAP_SYSTEM_PROMPT: &str = "You are a clinical scribe. From the consultation transcript, \
write the encounter note in markdown using exactly these five section headers, in this order: \
\"## Subjective\", \"## Objective\", \"## Assessment\", \"## Plan\", \"## Response\". \
Placement: Subjective = what the patient reports (symptoms, history, concerns); \
Objective = only measured or observed data (vitals, exam findings, test results) — never \
patient-reported items; Assessment = your clinical interpretation or diagnosis, supported by the \
Subjective and Objective above; Plan = concrete next steps (treatment, medications, referrals, \
follow-up); Response = how the patient has responded since the last visit to previously prescribed \
treatment (symptom change, side effects, adherence). \
Write each section as concise bullet points, not paragraphs; clinical shorthand (pt, c/o, BP, hx) \
is fine. Use only information explicitly stated in the transcript. Do not add, assume, or infer any \
symptom, finding, measurement, diagnosis, medication, or plan that is not stated; where the \
transcript is garbled but the intended clinical meaning is clear from context, write the intended \
meaning. If a section has no material, write \"Not discussed\" under its header; for Response, if \
this is a first visit with no prior treatment, write \"First visit — no prior treatment\". \
Output only the note in one pass: no preamble, no commentary, no questions, and no sections other \
than the five above.";

/// The raw, messy consult transcript of the one-shot example (design §8.3). Kept
/// verbatim — disfluencies and a garbled ASR phrase ("right down beforehand")
/// included — so the paired note teaches the model to resolve it from context.
const EXAMPLE_TRANSCRIPT: &str = "So um what brings you in today? Yeah I've had this uh headache, \
like right down beforehand, for about three days now. It's kind of throbbing. Um did you take \
anything for it? Yeah I took some Tylenol two days ago, didn't really help much. Any nausea or \
vision changes? No, no nausea, vision's fine. Alright let me check your blood pressure. Okay BP is \
one thirty over eighty five, temp's normal, thirty six point eight. And last time we started you on \
the amitriptyline for the migraines, how's that going? Yeah it's been better actually, the migraines \
are less frequent, maybe once a week now instead of like three times. Any side effects? Uh a bit of \
dry mouth but that's it. Okay so this looks like a tension headache, different from your usual \
migraine. Keep taking the amitriptyline, and for this headache take ibuprofen four hundred \
milligrams, and if it's not better in a week come back in.";

/// The ideal SOAP-R note for [`EXAMPLE_TRANSCRIPT`] (design §8.3): concise bullets,
/// clinical shorthand, correct per-section placement, and — deliberately — the
/// garbled "right down beforehand" resolved to "right side of forehead" from
/// context rather than copied verbatim. This is a fixed prefix (prompt-cache
/// candidate, §8.2), placed before the real transcript in [`build_prompt`].
const EXAMPLE_NOTE: &str = "## Subjective\n\
- Headache x3 days, throbbing, right side of forehead\n\
- Took Tylenol 2 days ago, minimal relief\n\
- No nausea, no vision changes\n\
\n\
## Objective\n\
- BP 130/85\n\
- Temp 36.8 C\n\
\n\
## Assessment\n\
- Tension headache, distinct from usual migraine\n\
\n\
## Plan\n\
- Continue amitriptyline\n\
- Ibuprofen 400 mg PRN for this headache\n\
- Return in 1 week if not improved\n\
\n\
## Response\n\
- Migraines less frequent on amitriptyline: ~3x/week → ~1x/week\n\
- Mild dry mouth (side effect)";

/// The fixed lead-in of the user turn, before the transcript itself. Kept as a
/// constant so it can sit inside the cacheable [`prefix`] (it never changes) while
/// only the transcript that follows it varies note to note.
const USER_LEAD_IN: &str = "Consultation transcript:\n\n";

/// The user turn: the transcript exactly as the clinician left it (§8.1).
pub fn user_message(transcript: &str) -> String {
    format!("{USER_LEAD_IN}{}", transcript.trim())
}

/// The fixed prompt **prefix** for a model: system instruction, the one-shot
/// worked example, and the instruct-template scaffolding up to (and including) the
/// user lead-in — everything before the note's own transcript. This is byte-identical
/// across every note, which is exactly what the KV-cache reuse in §8.7 caches: it is
/// prefilled once and its state restored per note so the prefix is never re-decoded.
/// [`build_prompt`] is `prefix(model) + transcript_tail(model, transcript)`.
pub fn prefix(model: LlmModel) -> String {
    let example_user = user_message(EXAMPLE_TRANSCRIPT);
    match model {
        // Gemma has no system role, so the instruction rides in the first user turn.
        // The BOS is added by the tokenizer (`AddBos::Always`), so it is not in the
        // string. The one-shot example is a completed user→model turn pair before the
        // real transcript's user turn.
        LlmModel::Gemma => format!(
            "<start_of_turn>user\n{SOAP_SYSTEM_PROMPT}\n\n{example_user}<end_of_turn>\n\
             <start_of_turn>model\n{EXAMPLE_NOTE}<end_of_turn>\n\
             <start_of_turn>user\n{USER_LEAD_IN}"
        ),
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

/// Wrap the system, the one-shot example, and the real transcript in the model's
/// chat template. Split into [`prefix`] (fixed, cacheable §8.7) + [`transcript_tail`]
/// (changing); this is the concatenation and stays the single source of the exact
/// prompt for the fallback (uncached) path.
pub fn build_prompt(model: LlmModel, transcript: &str) -> String {
    format!("{}{}", prefix(model), transcript_tail(model, transcript))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_pins_structure_and_anti_hallucination() {
        for header in [
            "## Subjective",
            "## Objective",
            "## Assessment",
            "## Plan",
            "## Response",
        ] {
            assert!(SOAP_SYSTEM_PROMPT.contains(header), "missing {header}");
        }
        // The safety-critical instruction must be present and explicit.
        assert!(SOAP_SYSTEM_PROMPT.contains("only information explicitly stated"));
        assert!(SOAP_SYSTEM_PROMPT.contains("Do not add, assume, or infer"));
        // Empty-section rule ("Not discussed"), and the first-visit Response wording.
        assert!(SOAP_SYSTEM_PROMPT.contains("Not discussed"));
        assert!(SOAP_SYSTEM_PROMPT.contains("First visit"));
        // Concise-bullets instruction.
        assert!(SOAP_SYSTEM_PROMPT.contains("concise bullet points"));
    }

    #[test]
    fn one_shot_example_models_the_five_sections_and_asr_repair() {
        // The example note is the fixed prefix that teaches format/style; it must
        // carry all five headers and the deliberate garbled-ASR resolution.
        for header in [
            "## Subjective",
            "## Objective",
            "## Assessment",
            "## Plan",
            "## Response",
        ] {
            assert!(EXAMPLE_NOTE.contains(header), "example missing {header}");
        }
        // The raw transcript keeps the garble; the note resolves it from context.
        assert!(EXAMPLE_TRANSCRIPT.contains("right down beforehand"));
        assert!(EXAMPLE_NOTE.contains("right side of forehead"));
        assert!(!EXAMPLE_NOTE.contains("right down beforehand"));
    }

    #[test]
    fn user_message_trims_and_embeds_the_transcript() {
        let m = user_message("  patient reports a cough  ");
        assert!(m.contains("patient reports a cough"));
        assert!(!m.contains("  patient")); // trimmed
    }

    #[test]
    fn prefix_plus_tail_equals_build_prompt() {
        // The KV-cache reuse (§8.7) prefills `prefix` and appends `transcript_tail`;
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
    fn build_prompt_uses_the_gemma_template() {
        let p = build_prompt(LlmModel::Gemma, "cough for two days");
        assert!(p.starts_with("<start_of_turn>user\n"));
        // Ends on an open model turn so the model generates the note next.
        assert!(p.ends_with("<start_of_turn>model\n"));
        // The one-shot example is a completed model turn before the real transcript.
        assert!(p.contains(EXAMPLE_NOTE));
        assert!(p.contains("<end_of_turn>"));
        assert!(p.contains("right down beforehand"));
        assert!(p.contains("cough for two days"));
        // All five headers appear (via the example).
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
