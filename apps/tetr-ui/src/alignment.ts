export type AlignedRow = {
  id: number;
  source: string;
  translation: string;
};

function normalizeText(input: string): string {
  return input.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
}

export function splitSourceLines(input: string): string[] {
  return normalizeText(input)
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
}

export function splitTranslationBlocks(input: string): string[] {
  const blocks: string[] = [];
  const current: string[] = [];

  for (const line of normalizeText(input).split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) {
      if (current.length > 0) {
        blocks.push(current.join("\n"));
        current.length = 0;
      }
      continue;
    }

    current.push(trimmed);
  }

  if (current.length > 0) {
    blocks.push(current.join("\n"));
  }

  return blocks;
}

function bucketize(items: string[], bucketCount: number): string[][] {
  const buckets = Array.from({ length: bucketCount }, () => [] as string[]);
  if (items.length === 0 || bucketCount <= 0) {
    return buckets;
  }

  items.forEach((item, index) => {
    const bucketIndex = Math.min(
      bucketCount - 1,
      Math.floor((index * bucketCount) / items.length)
    );
    buckets[bucketIndex].push(item);
  });

  return buckets;
}

export function buildAlignedRows(source: string, translation: string): AlignedRow[] {
  const sourceLines = splitSourceLines(source);
  const translationBlocks = splitTranslationBlocks(translation);

  if (sourceLines.length === 0 && translationBlocks.length === 0) {
    return [];
  }

  if (sourceLines.length === 0) {
    return translationBlocks.map((block, id) => ({
      id,
      source: "",
      translation: block,
    }));
  }

  if (translationBlocks.length === 0) {
    return sourceLines.map((line, id) => ({
      id,
      source: line,
      translation: "",
    }));
  }

  const rowCount = Math.max(sourceLines.length, translationBlocks.length);
  const sourceBuckets = bucketize(sourceLines, rowCount);
  const translationBuckets = bucketize(translationBlocks, rowCount);

  return Array.from({ length: rowCount }, (_, id) => ({
    id,
    source: sourceBuckets[id].join("\n"),
    translation: translationBuckets[id].join("\n\n"),
  }));
}
