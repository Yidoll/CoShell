import type { Terminal } from '@xterm/xterm';

/** Serializes *all* terminal writes, including reads of the rendered buffer. */
export class TerminalRenderQueue {
  private chain = Promise.resolve();
  private consistent = true;
  constructor(private readonly terminal: Terminal) {}
  private put(text: string): Promise<void> { return new Promise((resolve) => this.terminal.write(text, resolve)); }
  write(text: string): void { this.chain = this.chain.then(() => this.put(text)); }
  eraseLast(grapheme: string): void {
    this.chain = this.chain.then(async () => {
      const t = this.terminal; const b = t.buffer.active;
      const endY = b.baseY + b.cursorY; let y = endY; let x = b.cursorX;
      let found = ''; let width = 0;
      while (y >= 0 && found.length < grapheme.length) {
        if (x === 0) { y--; x = t.cols; continue; }
        x--;
        const cell = b.getLine(y)?.getCell(x);
        if (!cell) break;
        if (cell.getWidth() === 0) continue;
        const chars = cell.getChars();
        if (!chars) break;
        found = chars + found; width += cell.getWidth();
      }
      if (found !== grapheme || !width) { this.consistent = false; return; }
      const up = endY - y;
      const move = `${up ? `\x1b[${up}A` : ''}\r\x1b[${x + 1}G`;
      // All cells after this position are part of the local edit buffer.
      await this.put(move + '\x1b[J');
    });
  }
  async idle(): Promise<boolean> { await this.chain; return this.consistent; }
}
