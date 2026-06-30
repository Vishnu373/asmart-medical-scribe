import { describe, it, expect } from "vitest";
import { parseSoap, serializeSoap } from "@/lib/soap";

const SAMPLE = `## Subjective
Patient reports a cough for three days.

## Objective
Temp 37.8.

## Assessment
Likely viral URI.

## Plan
Rest and fluids.`;

describe("parseSoap", () => {
  it("splits markdown into the four sections", () => {
    const s = parseSoap(SAMPLE);
    expect(s.subjective).toBe("Patient reports a cough for three days.");
    expect(s.objective).toBe("Temp 37.8.");
    expect(s.assessment).toBe("Likely viral URI.");
    expect(s.plan).toBe("Rest and fluids.");
  });

  it("yields empty strings for a bare header", () => {
    expect(parseSoap("## Subjective\n## Objective\n## Assessment\n## Plan").subjective).toBe("");
  });

  it("keeps an unrecognized ## line as body text instead of dropping it", () => {
    const md =
      "## Assessment\nViral URI.\n## Differential\nBacterial less likely.\n## Plan\nRest.";
    const s = parseSoap(md);
    expect(s.assessment).toBe("Viral URI.\n## Differential\nBacterial less likely.");
    expect(s.plan).toBe("Rest.");
  });
});

describe("serializeSoap", () => {
  it("round-trips parsed sections back to canonical markdown", () => {
    expect(serializeSoap(parseSoap(SAMPLE))).toBe(SAMPLE);
  });

  it("keeps an empty section as a bare header", () => {
    const md = serializeSoap({ subjective: "x", objective: "", assessment: "", plan: "y" });
    expect(md).toBe("## Subjective\nx\n\n## Objective\n\n## Assessment\n\n## Plan\ny");
  });
});
