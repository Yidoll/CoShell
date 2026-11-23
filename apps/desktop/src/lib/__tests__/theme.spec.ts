import { afterEach, describe, expect, it, vi } from 'vitest';
import { initializeTheme, setThemeMode, themeMode } from '@/lib/theme';

const key = 'coshell.theme';
let stop: (() => void) | undefined;

afterEach(() => {
  stop?.();
  stop = undefined;
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  localStorage.clear();
});

function systemTheme(initial: boolean) {
  const media = new EventTarget() as EventTarget & { matches: boolean };
  media.matches = initial;
  vi.stubGlobal('matchMedia', () => media);
  return (dark: boolean) => {
    media.matches = dark;
    media.dispatchEvent(new Event('change'));
  };
}

describe('应用主题', () => {
  it('启动时恢复固定主题，忽略系统变化，切回跟随后再次响应', () => {
    const changeSystem = systemTheme(true);
    localStorage.setItem(key, 'light');
    stop = initializeTheme();
    expect(themeMode.value).toBe('light');
    expect(document.documentElement.dataset.theme).toBe('light');
    setThemeMode('dark');
    expect(localStorage.getItem(key)).toBe('dark');
    changeSystem(false);
    expect(document.documentElement.dataset.theme).toBe('dark');
    setThemeMode('system');
    expect(document.documentElement.dataset.theme).toBe('light');
    changeSystem(true);
    expect(document.documentElement.dataset.theme).toBe('dark');
    expect(localStorage.getItem(key)).toBe('system');
  });

  it('损坏、缺失或无法读取的偏好回退到系统，无系统接口时采用白色', () => {
    systemTheme(true);
    localStorage.setItem(key, 'orange');
    stop = initializeTheme();
    expect(themeMode.value).toBe('system');
    expect(document.documentElement.dataset.theme).toBe('dark');
    stop();
    vi.stubGlobal('matchMedia', undefined);
    vi.stubGlobal('localStorage', { getItem() { throw new Error('blocked'); } });
    stop = initializeTheme();
    expect(themeMode.value).toBe('system');
    expect(document.documentElement.dataset.theme).toBe('light');
  });

  it('写入失败仍立即切换，并返回仅本次有效的状态', () => {
    stop = initializeTheme();
    vi.stubGlobal('localStorage', { setItem() { throw new Error('full'); } });
    expect(setThemeMode('dark')).toBe(false);
    expect(themeMode.value).toBe('dark');
    expect(document.documentElement.dataset.theme).toBe('dark');
  });
});
