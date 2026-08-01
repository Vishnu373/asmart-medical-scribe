import { describe, it, expect } from "vitest";
import { parseSoap, serializeSoap, toPlainText } from "@/lib/soap";

const SAMPLE = `## Subjective
Patient reports a cough for three days.

## Objective
Temp 37.8.

## Assessment
Likely viral URI.

## Plan
Rest and fluids.

## Response
Cough improved on prior antibiotics.`;

describe("parseSoap", () => {
  it("splits markdown into the five sections", () => {
    const s = parseSoap(SAMPLE);
    expect(s.subjective).toBe("Patient reports a cough for three days.");
    expect(s.objective).toBe("Temp 37.8.");
    expect(s.assessment).toBe("Likely viral URI.");
    expect(s.plan).toBe("Rest and fluids.");
    expect(s.response).toBe("Cough improved on prior antibiotics.");
  });

  it("yields empty strings for a bare header", () => {
    expect(
      parseSoap("## Subjective\n## Objective\n## Assessment\n## Plan\n## Response").subjective,
    ).toBe("");
  });

  it("keeps an unrecognized ## line as body text instead of dropping it", () => {
    const md =
      "## Assessment\nViral URI.\n## Differential\nBacterial less likely.\n## Plan\nRest.";
    const s = parseSoap(md);
    expect(s.assessment).toBe("Viral URI.\n## Differential\nBacterial less likely.");
    expect(s.plan).toBe("Rest.");
  });
});

describe("toPlainText", () => {
  it("renders a full note the way the preview shows it", () => {
    const md = `## Subjective
- **Cough** for three days
- No *fever*

## Objective
Temp 37.8.

---

## Assessment
1. Likely viral URI`;
    expect(toPlainText(md)).toBe(`Subjective
- Cough for three days
- No fever

Objective
Temp 37.8.

Assessment
1. Likely viral URI`);
  });

  it("strips headers at every level", () => {
    expect(toPlainText("# A\n## B\n###### C")).toBe("A\nB\nC");
  });

  it("normalizes bullet markers and preserves nesting indent", () => {
    expect(toPlainText("* a\n  + b\n    - c")).toBe("- a\n  - b\n    - c");
  });

  it("keeps numbered prefixes — digits are clinical content", () => {
    expect(toPlainText("1. 500 mg BID\n2. Review in 7 days")).toBe(
      "1. 500 mg BID\n2. Review in 7 days",
    );
  });

  it("strips each inline form", () => {
    expect(toPlainText("**b** __b__ *i* _i_ `c` ~~s~~")).toBe("b b i i c s");
    expect(toPlainText("[text](http://x) and ![alt](http://y)")).toBe("text and alt");
  });

  it("leaves snake_case intact", () => {
    expect(toPlainText("check soap_data and record_id")).toBe("check soap_data and record_id");
  });

  it("leaves asterisks and underscores that are not emphasis", () => {
    expect(toPlainText("Dose 5*3 and 2*4 per day")).toBe("Dose 5*3 and 2*4 per day");
    expect(toPlainText("Range 5 * 3 * 2")).toBe("Range 5 * 3 * 2");
    expect(toPlainText("Range 5 _ 3 _ 1")).toBe("Range 5 _ 3 _ 1");
  });

  it("strips nested emphasis without leaving stray asterisks", () => {
    expect(toPlainText("**bold with *nested* text**")).toBe("bold with nested text");
    expect(toPlainText("***bold italic***")).toBe("bold italic");
  });

  it("drops unbalanced bold left by malformed model output", () => {
    expect(toPlainText("**Plan: rest")).toBe("Plan: rest");
  });

  it("unwraps blockquotes and drops horizontal rules", () => {
    expect(toPlainText("> quoted\n\n***\n\nnext")).toBe("quoted\n\nnext");
    expect(toPlainText("> ## Quoted header\n>> nested")).toBe("Quoted header\nnested");
  });

  it("handles links whose text or url contains brackets", () => {
    expect(toPlainText("[link](http://x/a(b)c)")).toBe("link");
    expect(toPlainText("[ref [1] here](http://x)")).toBe("ref [1] here");
  });

  it("collapses runs of blank lines to one", () => {
    expect(toPlainText("a\n\n\n\n\nb")).toBe("a\n\nb");
  });

  it("returns empty for empty or markup-only input", () => {
    expect(toPlainText("")).toBe("");
    expect(toPlainText("---\n\n")).toBe("");
  });
});

describe("serializeSoap", () => {
  it("round-trips parsed sections back to canonical markdown", () => {
    expect(serializeSoap(parseSoap(SAMPLE))).toBe(SAMPLE);
  });

  it("keeps an empty section as a bare header", () => {
    const md = serializeSoap({
      subjective: "x",
      objective: "",
      assessment: "",
      plan: "y",
      response: "",
    });
    expect(md).toBe(
      "## Subjective\nx\n\n## Objective\n\n## Assessment\n\n## Plan\ny\n\n## Response",
    );
  });
});
