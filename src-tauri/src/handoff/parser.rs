//! Deterministic SOAP-section parser for EMR hand-off (design §8.6 / §8.3).
//!
//! The note's five fixed `## ` headers let us split it into per-section bodies
//! with plain string work — no AI, no grammar constraint. Markdown markers are
//! stripped so the EMR field (a plain-text box) receives clean text. Pure and
//! unit-tested; the native paste path in `mod.rs` calls into it.

/// The five SOAP-R sections, in note order (§8.3; Response = interval response to
/// prior treatment).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoapSection {
    Subjective,
    Objective,
    Assessment,
    Plan,
    Response,
}

impl SoapSection {
    pub const ALL: [SoapSection; 5] = [
        SoapSection::Subjective,
        SoapSection::Objective,
        SoapSection::Assessment,
        SoapSection::Plan,
        SoapSection::Response,
    ];

    /// The exact markdown header the generator emits (§8.3).
    pub fn header(self) -> &'static str {
        match self {
            SoapSection::Subjective => "## Subjective",
            SoapSection::Objective => "## Objective",
            SoapSection::Assessment => "## Assessment",
            SoapSection::Plan => "## Plan",
            SoapSection::Response => "## Response",
        }
    }

    /// The stable lowercase key crossing the Tauri boundary (the picker passes it
    /// to `paste_section`).
    pub fn key(self) -> &'static str {
        match self {
            SoapSection::Subjective => "subjective",
            SoapSection::Objective => "objective",
            SoapSection::Assessment => "assessment",
            SoapSection::Plan => "plan",
            SoapSection::Response => "response",
        }
    }

    /// Parse the picker's key back to a section; unknown keys yield `None` so a bad
    /// argument is rejected rather than mis-pasted.
    pub fn from_key(key: &str) -> Option<Self> {
        SoapSection::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// Extract one section's body as plain text: the lines under its `## ` header up
/// to the next `## ` header (or end), with markdown markers stripped and blank
/// edges trimmed. A missing or empty section yields an empty string (the caller
/// declines to paste nothing).
pub fn section_body(markdown: &str, section: SoapSection) -> String {
    let mut in_section = false;
    let mut body: Vec<String> = Vec::new();
    for line in markdown.lines() {
        if is_section_header(line) {
            if in_section {
                break; // reached the next section
            }
            in_section = line.trim().eq_ignore_ascii_case(section.header());
            continue;
        }
        if in_section {
            body.push(strip_markdown(line));
        }
    }
    body.join("\n").trim().to_string()
}

/// A second-level markdown header line (the section delimiter).
fn is_section_header(line: &str) -> bool {
    line.trim_start().starts_with("## ")
}

/// Strip the markdown the generator may emit so the EMR gets plain text: a leading
/// unordered-list marker and bold (`**`/`__`) emphasis. Numbered list prefixes are
/// kept (the digits are clinical content, not markup).
fn strip_markdown(line: &str) -> String {
    let content = line.trim();
    let content = content
        .strip_prefix("- ")
        .or_else(|| content.strip_prefix("* "))
        .or_else(|| content.strip_prefix("+ "))
        .unwrap_or(content);
    content.replace("**", "").replace("__", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "## Subjective\n\
        Patient reports a **sore throat** for 3 days.\n\
        \n\
        ## Objective\n\
        - Temp 38.1 C\n\
        - Throat erythematous\n\
        ## Assessment\n\
        Likely viral pharyngitis.\n\
        ## Plan\n\
        1. Rest and fluids\n\
        2. Recheck if worse\n\
        ## Response\n\
        - Cough improved on prior antibiotics";

    #[test]
    fn keys_round_trip() {
        for s in SoapSection::ALL {
            assert_eq!(SoapSection::from_key(s.key()), Some(s));
        }
        assert_eq!(SoapSection::from_key("nonsense"), None);
    }

    #[test]
    fn extracts_each_section_body() {
        assert_eq!(
            section_body(NOTE, SoapSection::Subjective),
            "Patient reports a sore throat for 3 days."
        );
        // Bullets stripped to plain lines; body stops at the next header.
        assert_eq!(
            section_body(NOTE, SoapSection::Objective),
            "Temp 38.1 C\nThroat erythematous"
        );
        assert_eq!(
            section_body(NOTE, SoapSection::Assessment),
            "Likely viral pharyngitis."
        );
        // Numbered prefixes are clinical content and kept; Plan stops at Response.
        assert_eq!(
            section_body(NOTE, SoapSection::Plan),
            "1. Rest and fluids\n2. Recheck if worse"
        );
        // The fifth section (SOAP-R) is extracted like the rest.
        assert_eq!(
            section_body(NOTE, SoapSection::Response),
            "Cough improved on prior antibiotics"
        );
    }

    #[test]
    fn missing_or_empty_section_yields_empty() {
        // Header present but no body (an empty section per §8.3).
        let note = "## Subjective\n## Objective\nfindings";
        assert_eq!(section_body(note, SoapSection::Subjective), "");
        // Section absent entirely.
        assert_eq!(section_body("## Plan\ndo x", SoapSection::Assessment), "");
    }

    #[test]
    fn header_match_tolerates_trailing_space_and_case() {
        // Exact headers are what the model emits; a trailing-space or lowercase
        // drift still matches so a minor format wobble doesn't drop the section.
        assert_eq!(section_body("## Subjective \nhi", SoapSection::Subjective), "hi");
        assert_eq!(section_body("## subjective\nhi", SoapSection::Subjective), "hi");
    }
}
