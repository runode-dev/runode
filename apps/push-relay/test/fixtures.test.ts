import { describe, expect, it } from 'vitest';
import { MAX_APNS_PAYLOAD, MAX_FCM_DATA, fcmMessage, notifyPayload, readNotification } from '../src/payload';
import { FCM_TOKEN, NOW, fixture } from './common';

function notification(name: string, change: object = {}) {
  const json = JSON.parse(fixture(name));
  const read = readNotification({ ...(json.notification ?? json), ...change });
  expect(read).not.toBeNull();
  return read!;
}

describe('样例', () => {
  it('中转请求体拼出的 APNs payload 和 FCM 请求体和样例逐字节一样', () => {
    const cases: [string, string, string | null][] = [
      ['relay_notify.json', 'apns_notify.json', 'fcm_message.json'],
      ['relay_notify_todo_done.json', 'apns_todo_done.json', 'fcm_message_todo_done.json'],
      ['relay_notify_todo_failed.json', 'apns_todo_failed.json', null],
      ['notification.json', 'apns_notify.json', 'fcm_message.json'],
    ];
    for (const [request, apns, fcm] of cases) {
      const n = notification(request);
      expect(notifyPayload(n), request).toBe(fixture(apns));
      if (fcm) expect(fcmMessage(FCM_TOKEN, n, NOW + 1800, NOW), request).toBe(fixture(fcm));
    }
  });

  it('agent 做完、不知道是哪个 agent 时各有各的样子', () => {
    const done = notification('notification.json', { kind: 'agent_done', lines: [] });
    expect(notifyPayload(done)).toBe(fixture('apns_agent_done.json'));
    expect(fcmMessage(FCM_TOKEN, done, NOW + 1800, NOW)).toBe(fixture('fcm_message_agent_done.json'));
    const unnamed = notification('notification.json', { agent: '', lines: [] });
    expect(notifyPayload(unnamed)).toBe(fixture('apns_notify_unnamed.json'));
  });

  it('过了时的 ttl 是 0', () => {
    const late = JSON.parse(fcmMessage(FCM_TOKEN, notification('notification.json'), NOW, NOW + 5)!);
    expect(late.message.android.ttl).toBe('0s');
  });
});

describe('读通知', () => {
  it('会话标识换成小写，少字段、kind 不认识时读不了', () => {
    expect(notification('notification.json', { session: '0123456789ABCDEF0011223344556677' }).session).toBe(
      '0123456789abcdef0011223344556677',
    );
    const json = JSON.parse(fixture('notification.json'));
    expect(readNotification({ ...json, kind: 'future' })).toBeNull();
    expect(readNotification({ ...json, session: 'abc' })).toBeNull();
    expect(readNotification({ ...json, lines: [1] })).toBeNull();
    const { title: _, ...untitled } = json;
    expect(readNotification(untitled)).toBeNull();
    expect(readNotification({ ...json, extra: 1 })).not.toBeNull();
  });
});

describe('超长', () => {
  it('APNs payload 先从最上面一行起删，删光了还超才缩短标题', () => {
    const n = notification('notification.json', { lines: [0, 1, 2].map((i) => `${i}${'x'.repeat(1500)}`) });
    const lines = JSON.parse(notifyPayload(n)!).aps.alert.body.split('\n');
    expect(lines.length).toBe(2);
    expect(lines[0].startsWith('1') && lines[1].startsWith('2')).toBe(true);

    const long = { ...n, lines: ['x'.repeat(5000)], title: '标'.repeat(2000) };
    const payload = notifyPayload(long)!;
    expect(new TextEncoder().encode(payload).length).toBeLessThanOrEqual(MAX_APNS_PAYLOAD);
    const alert = JSON.parse(payload).aps.alert;
    expect(alert.body).toBeUndefined();
    const title: string = alert['subtitle-loc-args'][0];
    expect(title.endsWith('…') && Array.from(title).length < 2000).toBe(true);

    expect(notifyPayload({ ...long, machine_name: 'x'.repeat(5000) })).toBeNull();
  });

  it('FCM 的 data 照同样的规矩删减', () => {
    const data = (n: ReturnType<typeof notification>) => {
      const message = fcmMessage(FCM_TOKEN, n, NOW, NOW);
      return message === null ? null : JSON.parse(message).message.data;
    };
    const n = notification('notification.json', { lines: [0, 1, 2].map((i) => `${i}${'x'.repeat(1500)}`) });
    const fitted = data(n);
    expect(JSON.stringify(fitted).length).toBeLessThanOrEqual(MAX_FCM_DATA);
    expect(fitted.body.split('\n').map((line: string) => line[0])).toEqual(['1', '2']);

    const long = { ...n, lines: ['x'.repeat(5000)], title: '标'.repeat(2000) };
    const shortened = data(long);
    expect(shortened.body).toBe('');
    expect(shortened.title.endsWith('…') && Array.from(shortened.title as string).length < 2000).toBe(true);
    expect(data({ ...long, machine_name: 'x'.repeat(5000) })).toBeNull();
  });
});
