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

// Superseded by `toPlainText`: stripped only list markers and bold, so `#`
// headers reached the clipboard and the doctor deleted them by hand.
// export function stripMarkdown(text: string): string {
//   return text
//     .split("\n")
//     .map((line) => {
//       const content = line.trim().replace(/^[-*+] /, "");
//       return content.replaceAll("**", "").replaceAll("__", "");
//     })
//     .join("\n")
//     .trim();
// }

/** Horizontal rule: three or more of the same marker, alone on the line. */
const RULE = /^\s*([-*_])\1{2,}\s*$/;

/** Inline markup, stripped in order — `**` before `*`, or bold leaves stray asterisks. */
const INLINE: [RegExp, string][] = [
  // [/!\[([^\]]*)\]\([^)]*\)/g, "$1"], // ![alt](url)
  // [/\[([^\]]*)\]\([^)]*\)/g, "$1"], // [text](url) — stopped at the first inner
  // bracket or paren, so `[link](http://x/a(b)c)` truncated to `linkc)`.
  // One level of nesting on each side covers parenthesised URLs and `[ref [1]]`.
  [/!\[((?:[^[\]]|\[[^[\]]*\])*)\]\((?:[^()]|\([^()]*\))*\)/g, "$1"], // ![alt](url)
  [/\[((?:[^[\]]|\[[^[\]]*\])*)\]\((?:[^()]|\([^()]*\))*\)/g, "$1"], // [text](url)
  [/`([^`\n]+)`/g, "$1"], // `code`
  [/~~([^~\n]+)~~/g, "$1"], // ~~struck~~
  // [/\*\*([^*\n]+)\*\*/g, "$1"], // **bold** — body barred `*`, so `**a *b* c**` never matched
  [/\*\*(.+?)\*\*/g, "$1"], // **bold**, inner `*` allowed; the italic rule below cleans it up
  [/__([^_\n]+)__/g, "$1"], // __bold__
  // [/\*([^*\n]+)\*/g, "$1"], // *italic* — joined any two asterisks: `5*3 and 2*4` → `53 and 24`
  // Emphasis must open at a word boundary and hug non-space, so doses survive.
  [/(^|[^A-Za-z0-9*])\*(?!\s)([^*\n]+)(?<!\s)\*(?![A-Za-z0-9])/g, "$1$2"], // *italic*
  // `_italic_` only at word boundaries, so snake_case identifiers survive.
  // [/(^|[^A-Za-z0-9_])_([^_\n]+)_(?![A-Za-z0-9_])/g, "$1$2"], // ate spaced underscores: `5 _ 3 _ 1`
  [/(^|[^A-Za-z0-9_])_(?!\s)([^_\n]+)(?<!\s)_(?![A-Za-z0-9_])/g, "$1$2"],
  [/\*\*/g, ""], // unbalanced leftovers — model output isn't always well-formed
];

/**
 * Render markdown to plain text for the EMR clipboard — what the doctor sees in
 * Preview, minus the markup, so nothing has to be hand-deleted after pasting.
 * Bullets are kept (browsers draw them via CSS, but a plain-text field can't).
 */
export function toPlainText(markdown: string): string {
  const lines: string[] = [];
  for (const line of markdown.split("\n")) {
    if (RULE.test(line)) continue;
    // Quotes unwrap first (all levels), or `> ## Header` keeps its hashes.
    let out = line
      .replace(/^(\s*)(?:>\s?)+/, "$1")
      .replace(/^(\s*)#{1,6}\s+/, "$1")
      .replace(/^(\s*)[*+]\s+/, "$1- ");
    for (const [re, sub] of INLINE) out = out.replace(re, sub);
    lines.push(out.trimEnd());
  }
  return lines.join("\n").replace(/\n{3,}/g, "\n\n").trim();
}

/** Reassemble the five sections into the canonical headered markdown. */
export function serializeSoap(sections: SoapSections): string {
  return SOAP_ORDER.map((s) => {
    const body = sections[s].trim();
    return body ? `## ${LABEL[s]}\n${body}` : `## ${LABEL[s]}`;
  }).join("\n\n");
}
