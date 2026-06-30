//! Zero-shot SOAP prompt construction (design §8.3). Pure string work, kept out
//! of the native engine so the prompt — the part that shapes clinical safety —
//! is unit-testable without loading a model.

use super::LlmModel;

/// The system instruction. Three jobs (design §8.3): fix the four `##` SOAP
/// headers, forbid anything not in the transcript (the single most important
/// safety property — a fabricated clinical fact is the worst failure), and keep
/// an empty section as a bare header rather than dropping it.
pub const SOAP_SYSTEM_PROMPT: &str = "You are a clinical scribe. From the consultation transcript, \
write the encounter note in markdown using exactly these four section headers, in this order: \
\"## Subjective\", \"## Objective\", \"## Assessment\", \"## Plan\". \
Use only information explicitly stated in the transcript. Do not add, assume, or infer any \
symptom, finding, measurement, diagnosis, medication, or plan that is not stated. \
If the transcript has nothing for a section, write the header with no text under it. \
Output only the note: no preamble, no commentary, and no sections other than the four above.";

/// The user turn: the transcript exactly as the clinician left it (§8.1).
pub fn user_message(transcript: &str) -> String {
    format!("Consultation transcript:\n\n{}", transcript.trim())
}

/// Wrap the system + user content in the instruct template of the selected model
/// family. The two models ship different chat formats; using the wrong one
/// degrades adherence to the SOAP structure, so the template follows the model.
pub fn build_prompt(model: LlmModel, transcript: &str) -> String {
    let user = user_message(transcript);
    match model {
        // Mistral-Instruct: a single [INST] block carries both system and user
        // text (Mistral has no separate system role).
        LlmModel::Mistral => {
            format!("<s>[INST] {SOAP_SYSTEM_PROMPT}\n\n{user} [/INST]")
        }
        // Phi-3.5 (Q8 and Q4 are the same family/template): explicit
        // <|system|>/<|user|>/<|assistant|> turns.
        LlmModel::Phi | LlmModel::PhiQ4 => format!(
            "<|system|>\n{SOAP_SYSTEM_PROMPT}<|end|>\n<|user|>\n{user}<|end|>\n<|assistant|>\n"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_pins_structure_and_anti_hallucination() {
        for header in ["## Subjective", "## Objective", "## Assessment", "## Plan"] {
            assert!(SOAP_SYSTEM_PROMPT.contains(header), "missing {header}");
        }
        // The safety-critical instruction must be present and explicit.
        assert!(SOAP_SYSTEM_PROMPT.contains("only information explicitly stated"));
        assert!(SOAP_SYSTEM_PROMPT.contains("Do not add, assume, or infer"));
        // Empty-section rule (header kept, no body).
        assert!(SOAP_SYSTEM_PROMPT.contains("write the header with no text"));
    }

    #[test]
    fn user_message_trims_and_embeds_the_transcript() {
        let m = user_message("  patient reports a cough  ");
        assert!(m.contains("patient reports a cough"));
        assert!(!m.contains("  patient")); // trimmed
    }

    #[test]
    fn build_prompt_uses_the_right_template_per_model() {
        let mistral = build_prompt(LlmModel::Mistral, "cough for two days");
        assert!(mistral.starts_with("<s>[INST]"));
        assert!(mistral.ends_with("[/INST]"));
        assert!(mistral.contains("cough for two days"));
        assert!(mistral.contains("## Assessment"));

        let phi = build_prompt(LlmModel::Phi, "cough for two days");
        assert!(phi.contains("<|system|>"));
        assert!(phi.contains("<|user|>"));
        assert!(phi.ends_with("<|assistant|>\n"));
        assert!(phi.contains("cough for two days"));
    }
}
