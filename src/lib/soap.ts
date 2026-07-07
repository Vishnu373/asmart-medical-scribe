/**
 * SOAP-R note (de)serialization. The backend persists `soap_data` as markdown with
 * exactly five `##` headers in order (llm/prompt.rs); splitting them apart and
 * reassembling byte-compatibly is pure string work.
 */
import type { SoapSection } from "@/bridge";

export const SOAP_ORDER: SoapSection[] = [
  "subjective",
  "objective",
  "assessment",
  "plan",
  "response",
];

/** Header label per section, matching the prompt's exact `## ` headers. */
const LABEL: { [K in SoapSection]: string } = {
  subjective: "Subjective",
  objective: "Objective",
  assessment: "Assessment",
  plan: "Plan",
  response: "Response",
};

export type SoapSections = { [K in SoapSection]: string };

/** Split SOAP-R markdown into its five sections; unknown/missing headers yield "". */
export function parseSoap(markdown: string): SoapSections {
  const buf: SoapSections = {
    subjective: "",
    objective: "",
    assessment: "",
    plan: "",
    response: "",
  };
  let current: SoapSection | null = null;
  for (const line of markdown.split("\n")) {
    const header = line.match(/^##\s+(\w+)/);
    const key = header
      ? SOAP_ORDER.find((s) => LABEL[s].toLowerCase() === header[1].toLowerCase())
      : undefined;
    // Only one of the five known headers opens a section. Any other `##` line
    // (e.g. a model-emitted `## Differential` sub-heading) is kept as body text
    // of the current section — never a boundary — so unrecognized markdown isn't
    // silently dropped mid-note.
    if (key) {
      current = key;
      continue;
    }
    if (current) buf[current] += buf[current] ? `\n${line}` : line;
  }
  for (const s of SOAP_ORDER) buf[s] = buf[s].trim();
  return buf;
}

/**
 * Strip the markdown the generator may emit so the EMR (a plain-text field) gets
 * clean text. Mirrors the backend `handoff::parser::strip_markdown` line-for-line:
 * a leading unordered-list marker and bold (`**`/`__`) emphasis are removed;
 * numbered prefixes are kept (clinical content, not markup). Manual Copy and the
 * dormant native paste path must produce identical text (design §8.6 / §8.3).
 */
export function stripMarkdown(text: string): string {
  return text
    .split("\n")
    .map((line) => {
      const content = line.trim().replace(/^[-*+] /, "");
      return content.replaceAll("**", "").replaceAll("__", "");
    })
    .join("\n")
    .trim();
}

/** Reassemble the five sections into the canonical headered markdown. */
export function serializeSoap(sections: SoapSections): string {
  return SOAP_ORDER.map((s) => {
    const body = sections[s].trim();
    return body ? `## ${LABEL[s]}\n${body}` : `## ${LABEL[s]}`;
  }).join("\n\n");
}
