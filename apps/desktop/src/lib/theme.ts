import { readonly, ref } from 'vue';

export type ThemeMode = 'light' | 'dark' | 'system';
export type ResolvedTheme = Exclude<ThemeMode, 'system'>;

const storageKey = 'coshell.theme';
const mode = ref<ThemeMode>('system');
const resolved = ref<ResolvedTheme>('light');
let systemTheme: MediaQueryList | undefined;
export const themeMode = readonly(mode);
export const resolvedTheme = readonly(resolved);

function applyTheme() {
  resolved.value = mode.value === 'system'
    ? (systemTheme?.matches ? 'dark' : 'light')
    : mode.value;
  document.documentElement.dataset.theme = resolved.value;
  document.querySelector('meta[name="theme-color"]')?.setAttribute(
    'content', resolved.value === 'dark' ? '#131416' : '#ffffff',
  );
}

export function initializeTheme(): () => void {
  let saved: string | null = null;
  try {
    saved = window.localStorage.getItem(storageKey);
  } catch {
    // Storage may be disabled by the host.
  }
  mode.value = saved === 'light' || saved === 'dark' ? saved : 'system';

  try {
    systemTheme = window.matchMedia?.('(prefers-color-scheme: dark)');
  } catch {
    systemTheme = undefined;
  }
  const query = systemTheme;
  const onChange = () => {
    if (mode.value === 'system') applyTheme();
  };
  query?.addEventListener?.('change', onChange);
  applyTheme();
  return () => {
    query?.removeEventListener?.('change', onChange);
    if (systemTheme === query) systemTheme = undefined;
  };
}

export function setThemeMode(value: ThemeMode): boolean {
  mode.value = value;
  applyTheme();
  try {
    window.localStorage.setItem(storageKey, value);
    return true;
  } catch {
    return false;
  }
}
