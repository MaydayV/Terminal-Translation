export type TranslationEntry = {
  id: number;
  text: string;
  done: boolean;
  updatedAt: number;
};

export function prependEntryWithLimit(
  previous: TranslationEntry[],
  next: TranslationEntry,
  maxEntries = 200
): TranslationEntry[] {
  const normalizedLimit = Number.isFinite(maxEntries) ? Math.max(1, Math.floor(maxEntries)) : 200;
  const merged = [next, ...previous];
  return merged.length <= normalizedLimit ? merged : merged.slice(0, normalizedLimit);
}
