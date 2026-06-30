/**
 * SOAP note (de)serialization. The backend persists `soap_data` as markdown with
 * exactly four `##` headers in order (llm/prompt.rs); the four-section editor
 * needs them split apart and reassembled byte-compatibly. Pure string work.
 */
import type { SoapSection } from "@/bridge";

export const SOAP_ORDER: SoapSection[] = ["subjective", "objective", "assessment", "plan"];

/** Header label per section, matching the prompt's exact `## ` headers. */
const LABEL: { [K in SoapSection]: string } = {
  subjective: "Subjective",
  objective: "Objective",
  assessment: "Assessment",
  plan: "Plan",
};

export type SoapSections = { [K in SoapSection]: string };

/** Split SOAP markdown into its four sections; unknown/missing headers yield "". */
export function parseSoap(markdown: string): SoapSections {
  const buf: SoapSections = { subjective: "", objective: "", assessment: "", plan: "" };
  let current: SoapSection | null = null;
  for (const line of markdown.split("\n")) {
    const header = line.match(/^##\s+(\w+)/);
    const key = header
      ? SOAP_ORDER.find((s) => LABEL[s].toLowerCase() === header[1].toLowerCase())
      : undefined;
    // Only one of the four known headers opens a section. Any other `##` line
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

/** Reassemble the four sections into the canonical headered markdown. */
export function serializeSoap(sections: SoapSections): string {
  return SOAP_ORDER.map((s) => {
    const body = sections[s].trim();
    return body ? `## ${LABEL[s]}\n${body}` : `## ${LABEL[s]}`;
  }).join("\n\n");
}
