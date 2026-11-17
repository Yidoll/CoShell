import type { DavVersionPage } from './types';

/** Check real IPC data rather than relying on invoke<T>'s compile-time cast. */
export function decodeVersionPage(value: unknown): DavVersionPage {
  const invalid = () => new Error('备份版本响应格式不兼容，请使用匹配的最新版客户端后重试。');
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw invalid();
  const page = value as Record<string, unknown>;
  if (!Array.isArray(page.versions) || page.versions.length > 50 || !(page.nextCursor === null || (typeof page.nextCursor === 'string' && page.nextCursor.length > 0 && page.nextCursor.length <= 1024))) throw invalid();
  for (const item of page.versions) {
    if (!item || typeof item !== 'object' || Array.isArray(item) || typeof item.id !== 'string' || !item.id || item.id.length > 256 || typeof item.latest !== 'boolean' || !(item.timestamp === null || (typeof item.timestamp === 'string' && /^\d{14}$/.test(item.timestamp)))) throw invalid();
  }
  return { versions: page.versions.map(v => ({id:v.id,timestamp:v.timestamp,latest:v.latest})), nextCursor: page.nextCursor as string | null };
}
