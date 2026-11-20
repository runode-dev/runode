// 中转拼给 APNs 的 payload 和给 FCM 的请求体，以及读电脑交来的通知。线上格式以 `runode_protocol::push`
// 为准，测试对着它的 `tests/fixtures/push` 样例逐字节比对。

export type NotificationKind = 'blocked' | 'todo_done' | 'todo_failed' | 'agent_done';

/** 电脑交来的一条通知；`session` 已经换成小写。 */
export interface Notification {
  machine: string;
  machine_name: string;
  session: string;
  title: string;
  agent: string;
  lines: string[];
  kind: NotificationKind;
}

/** APNs payload 最多这么多字节。 */
export const MAX_APNS_PAYLOAD = 4096;
/** FCM 的 `data` 编成 JSON 最多这么多字节。 */
export const MAX_FCM_DATA = 4000;
/** FCM registration token 最多这么多个字符。 */
export const MAX_FCM_TOKEN_LEN = 320;
/** 通知在 APNs、FCM 那里最多留这么多秒。 */
export const EXPIRES_AFTER = 30 * 60;

const KINDS: NotificationKind[] = ['blocked', 'todo_done', 'todo_failed', 'agent_done'];
const SUBTITLE_KEY = 'agent.blocked.body';

/** 读请求体里的 `notification`；少字段、类型不对、`kind` 不认识时为 `null`。多出来的字段不管。 */
export function readNotification(value: unknown): Notification | null {
  if (typeof value !== 'object' || value === null) return null;
  const v = value as Record<string, unknown>;
  const strings = [v.machine, v.machine_name, v.session, v.title, v.agent];
  if (!strings.every((s) => typeof s === 'string')) return null;
  const session = v.session as string;
  if (!/^[0-9a-f]{32}$/i.test(session)) return null;
  if (!Array.isArray(v.lines) || !v.lines.every((line) => typeof line === 'string')) return null;
  const kind = v.kind ?? 'blocked';
  if (!KINDS.includes(kind as NotificationKind)) return null;
  return {
    machine: v.machine as string,
    machine_name: v.machine_name as string,
    session: session.toLowerCase(),
    title: v.title as string,
    agent: v.agent as string,
    lines: v.lines as string[],
    kind: kind as NotificationKind,
  };
}

/** FCM 的 registration token：1 到 `MAX_FCM_TOKEN_LEN` 个 `A-Z a-z 0-9 : _ -`。 */
export function isFcmToken(token: string): boolean {
  return token.length >= 1 && token.length <= MAX_FCM_TOKEN_LEN && /^[A-Za-z0-9:_-]*$/.test(token);
}

/** APNs payload；超过 `MAX_APNS_PAYLOAD` 字节时先从最上面一行起删 `lines`，删完还超就把标题一次次缩短一半，
 * 标题删光了还超时为 `null`。 */
export function notifyPayload(notification: Notification): string | null {
  return fitWithin(notification, MAX_APNS_PAYLOAD, encodeApns)?.[0] ?? null;
}

/** FCM HTTP v1 `messages:send` 的请求体，只带 `data`；`ttl` 是 `expiration` 减 `now`（Unix 秒，过了时是 0）。
 * `data` 超过 `MAX_FCM_DATA` 字节时照 `notifyPayload` 的规矩删减，删光了还超时为 `null`。 */
export function fcmMessage(token: string, notification: Notification, expiration: number, now: number): string | null {
  const fitted = fitWithin(notification, MAX_FCM_DATA, (n) => JSON.stringify(fcmData(n)));
  if (!fitted) return null;
  const [, n] = fitted;
  const android = { priority: 'HIGH', ttl: `${Math.max(0, expiration - now)}s`, collapse_key: n.session };
  return JSON.stringify({ message: { token, data: fcmData(n), android } });
}

function encodeApns(n: Notification): string {
  const [titleKey, titleArgs] = (() => {
    switch (n.kind) {
      case 'todo_done':
        return ['todo.done.title', []];
      case 'todo_failed':
        return ['todo.failed.title', []];
      case 'agent_done':
        return ['agent.done.title', [n.agent]];
      case 'blocked':
        return n.agent === '' ? ['agent.blocked.title.unnamed', []] : ['agent.blocked.title', [n.agent]];
    }
  })() as [string, string[]];
  const alert: Record<string, unknown> = { 'title-loc-key': titleKey };
  if (titleArgs.length > 0) alert['title-loc-args'] = titleArgs;
  alert['subtitle-loc-key'] = SUBTITLE_KEY;
  alert['subtitle-loc-args'] = [n.title, n.machine_name];
  if (n.lines.length > 0) alert.body = n.lines.join('\n');
  return JSON.stringify({
    aps: { alert, sound: 'default', 'thread-id': n.machine },
    machine: n.machine,
    session: n.session,
  });
}

function fcmData(n: Notification) {
  return {
    kind: n.kind,
    machine: n.machine,
    machine_name: n.machine_name,
    session: n.session,
    title: n.title,
    agent: n.agent,
    body: n.lines.join('\n'),
  };
}

const utf8 = new TextEncoder();

/** `encode` 编出来超过 `limit` 字节时删行、缩短标题；交回编好的和删减后的内容。 */
function fitWithin(
  notification: Notification,
  limit: number,
  encode: (n: Notification) => string,
): [string, Notification] | null {
  const n = { ...notification, lines: [...notification.lines] };
  for (;;) {
    const text = encode(n);
    if (utf8.encode(text).length <= limit) return [text, n];
    if (n.lines.length > 0) {
      n.lines.shift();
      continue;
    }
    const chars = Array.from(n.title).length;
    if (chars === 0) return null;
    n.title = clipped(n.title, Math.floor(chars / 2));
  }
}

/** 超过 `limit` 个字（按 Unicode 标量数）时留前 `limit - 1` 个加「…」。 */
function clipped(text: string, limit: number): string {
  const chars = Array.from(text);
  if (chars.length <= limit) return text;
  return chars.slice(0, Math.max(0, limit - 1)).join('') + (limit > 0 ? '…' : '');
}
