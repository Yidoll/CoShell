import { describe, expect, it } from 'vitest';
import { editTerminalLine, erasedCellWidth } from '../terminal-input';

describe('终端本地命令编辑', () => {
  it('在回车前只编辑本地内容，拒绝控制序列并支持退格', () => {
    expect(editTerminalLine('', 'echo 你好')).toEqual({ line: 'echo 你好', submitted: false });
    expect(editTerminalLine('echo 你好', '\x7f')).toEqual({ line: 'echo 你', submitted: false });
    expect(editTerminalLine('echo 你', '\x1b[A')).toEqual({ line: 'echo 你', submitted: false });
    expect(editTerminalLine('echo 你', '\r')).toEqual({ line: 'echo 你', submitted: true });
    expect(editTerminalLine('e\u0301', '\x7f').line).toBe('');
    expect(editTerminalLine('👩‍💻', '\x7f').line).toBe('');
  });
  it('使用终端单元格宽度擦除中英文和宽字符', () => {
    expect(erasedCellWidth(1, 1)).toBe(1);
    expect(erasedCellWidth(0, 2)).toBe(2);
    expect(erasedCellWidth(2, 1)).toBe(2);
  });
});
