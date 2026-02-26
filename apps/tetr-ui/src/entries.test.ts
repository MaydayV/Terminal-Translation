import { describe, expect, it } from "vitest";
import { prependEntryWithLimit, type TranslationEntry } from "./entries";

function makeEntry(id: number): TranslationEntry {
  return {
    id,
    text: `entry-${id}`,
    done: false,
    updatedAt: id,
  };
}

describe("prependEntryWithLimit", () => {
  it("keeps only newest entries within limit", () => {
    const existing = Array.from({ length: 3 }, (_, index) => makeEntry(index + 1));
    const next = makeEntry(99);

    const result = prependEntryWithLimit(existing, next, 3);

    expect(result.map((entry) => entry.id)).toEqual([99, 1, 2]);
  });
});
