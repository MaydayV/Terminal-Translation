import { describe, expect, it } from "vitest";
import { buildAlignedRows } from "./alignment";

describe("buildAlignedRows", () => {
  it("aligns source lines and translation blocks one by one when counts match", () => {
    const rows = buildAlignedRows("line 1\nline 2", "译文 1\n\n译文 2");

    expect(rows).toEqual([
      { id: 0, source: "line 1", translation: "译文 1" },
      { id: 1, source: "line 2", translation: "译文 2" },
    ]);
  });

  it("spreads fewer translation blocks across more source lines", () => {
    const rows = buildAlignedRows(
      "a\nb\nc\nd",
      "甲段\n\n乙段"
    );

    expect(rows).toEqual([
      { id: 0, source: "a", translation: "甲段" },
      { id: 1, source: "b", translation: "" },
      { id: 2, source: "c", translation: "乙段" },
      { id: 3, source: "d", translation: "" },
    ]);
  });

  it("spreads fewer source lines across more translation blocks", () => {
    const rows = buildAlignedRows(
      "s1\ns2",
      "t1\n\nt2\n\nt3\n\nt4"
    );

    expect(rows).toEqual([
      { id: 0, source: "s1", translation: "t1" },
      { id: 1, source: "", translation: "t2" },
      { id: 2, source: "s2", translation: "t3" },
      { id: 3, source: "", translation: "t4" },
    ]);
  });

  it("drops empty lines and blank translation blocks", () => {
    const rows = buildAlignedRows("\nfoo\n\nbar\n", "\n译一\n\n\n译二\n");

    expect(rows).toEqual([
      { id: 0, source: "foo", translation: "译一" },
      { id: 1, source: "bar", translation: "译二" },
    ]);
  });
});
