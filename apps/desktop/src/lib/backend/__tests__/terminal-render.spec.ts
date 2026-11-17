import { describe, expect, it } from 'vitest';
import { Terminal } from '@xterm/xterm';
import { TerminalRenderQueue } from '../terminal-render';

describe('xterm 写入队列', () => {
  it('连续输入和退格按顺序生效，并正确擦除宽字符', async () => {
    const term = new Terminal({ cols: 80, rows: 24, allowProposedApi: true });
    const queue = new TerminalRenderQueue(term);
    queue.write('ab'); queue.eraseLast('b'); queue.write('c');
    await queue.idle();
    expect(term.buffer.active.getLine(0)?.translateToString(true)).toBe('ac');
    queue.write('中'); queue.eraseLast('中'); queue.write('文');
    await queue.idle();
    expect(term.buffer.active.getLine(0)?.translateToString(true)).toBe('ac文');
    queue.write('e\u0301'); queue.eraseLast('e\u0301'); queue.write('x'); await queue.idle();
    expect(term.buffer.active.getLine(0)?.translateToString(true)).toBe('ac文x');
    queue.write('👩‍💻'); queue.eraseLast('👩‍💻'); queue.write('z'); await queue.idle();
    expect(term.buffer.active.getLine(0)?.translateToString(true)).toBe('ac文xz');
    queue.eraseLast('不同内容');
    expect(await queue.idle()).toBe(false);
    term.dispose();
  });
});
