/** Edit a locally guarded line; escape/control sequences never reach the shell. */
export function editTerminalLine(line: string, input: string): { line: string; submitted: boolean } {
  if (input === '\r' || input === '\n') return { line, submitted: true };
  if (input === '\x7f' || input === '\b') {
    const segments = Array.from(new Intl.Segmenter(undefined, { granularity: 'grapheme' }).segment(line), (item) => item.segment);
    return { line: segments.slice(0, -1).join(''), submitted: false };
  }
  if (/[\x00-\x1f\x7f]/.test(input)) return { line, submitted: false };
  return { line: line + input, submitted: false };
}

/** A zero-width trailing cell belongs to the preceding wide glyph. */
export function erasedCellWidth(last: number, preceding: number): number {
  return last === 0 && preceding === 2 ? 2 : last === 2 ? 2 : 1;
}
