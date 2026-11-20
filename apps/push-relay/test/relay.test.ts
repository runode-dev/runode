import { afterEach, describe, expect, it, vi } from 'vitest';
import { reply as fcmReply } from '../src/fcm';
import { decodeBase64url } from '../src/handle';
import { handle, ipKey, type Relay } from '../src/index';
import { fcmMessage, notifyPayload, readNotification } from '../src/payload';
import { FCM_TOKEN, MACHINE, SEND_URL, TOKEN, TOKEN_URI, fixture, setup, type Sent } from './common';

const APNS = 'https://api.push.apple.com/3/device/';
const SANDBOX = 'https://api.sandbox.push.apple.com/3/device/';

function post(relay: Relay, path: string, body: unknown, ip = '203.0.113.9'): Promise<Response> {
  const text = typeof body === 'string' ? body : JSON.stringify(body);
  const request = new Request(`https://push.runode.dev${path}`, {
    method: 'POST',
    body: text,
    headers: { 'cf-connecting-ip': ip },
  });
  return handle(request, relay);
}

async function register(relay: Relay, change: object = {}): Promise<string> {
  const response = await post(relay, '/v1/register', { ...JSON.parse(fixture('relay_register.json')), ...change });
  expect(response.status).toBe(200);
  return ((await response.json()) as { handle: string }).handle;
}

function notifyBody(handle: string, change: object = {}) {
  return { ...JSON.parse(fixture('relay_notify.json')), handle, expiration: Math.floor(Date.now() / 1000) + 1800, ...change };
}

async function reason(response: Response): Promise<[number, string]> {
  return [response.status, ((await response.json()) as { reason: string }).reason];
}

const answer = (status: number, reason = '', headers: Record<string, string> = {}) =>
  new Response(reason ? JSON.stringify({ reason }) : null, { status, headers });

afterEach(() => {
  vi.useRealTimers();
});

describe('换句柄', () => {
  it('APNs 和 FCM 的都换得到，请求不对时回对应的原因', async () => {
    const { relay } = await setup();
    await register(relay);
    await register(relay, JSON.parse(fixture('relay_register_fcm.json')));
    const cases: [object | string, string][] = [
      ['not json', 'BadRequest'],
      [{ v: 1 }, 'BadRequest'],
      [{ v: 2 }, 'UnsupportedVersion'],
      [{ env: 'sandbox_2' }, 'BadEnvironment'],
      [{ token: 'abc' }, 'BadDeviceToken'],
      [{ token: 'ab'.repeat(8) + 'zz' }, 'BadDeviceToken'],
      [{ service: 'hms' }, 'BadService'],
      [{ service: 'fcm', token: 'has space' }, 'BadDeviceToken'],
      [{ machine: 'not-a-uuid' }, 'BadMachine'],
    ];
    const base = JSON.parse(fixture('relay_register.json'));
    for (const [change, expected] of cases) {
      const body = typeof change === 'string' ? change : 'v' in change && Object.keys(change).length === 1 && change.v === 1 ? { v: 1 } : { ...base, ...change };
      expect(await reason(await post(relay, '/v1/register', body)), JSON.stringify(change)).toEqual([400, expected]);
    }
  });

  it('没配 FCM 时 FCM 的登记回 FcmNotConfigured', async () => {
    const { relay } = await setup({ fcm: false });
    const response = await post(relay, '/v1/register', JSON.parse(fixture('relay_register_fcm.json')));
    expect(await reason(response)).toEqual([400, 'FcmNotConfigured']);
  });

  it('同一个地址超了限频回 429 带 Retry-After', async () => {
    const { relay, limits } = await setup();
    limits.register.allow = false;
    const response = await post(relay, '/v1/register', JSON.parse(fixture('relay_register.json')), '2001:db8:1:2::5');
    expect(await reason(response)).toEqual([429, 'TooManyRequests']);
    expect(response.headers.get('retry-after')).toBe('60');
    expect(limits.register.keys).toEqual(['2001:db8:1:2::/64']);
  });
});

describe('发通知', () => {
  it('APNs：签好 JWT，payload 和请求头照规矩，成功回 200', async () => {
    const { relay, net, apnsPublic } = await setup();
    const handle = await register(relay);
    const body = notifyBody(handle);
    const response = await post(relay, '/v1/notify', body);
    expect(response.status).toBe(200);
    expect(await response.text()).toBe('');
    const [sent] = net.to(APNS) as [Sent];
    expect(sent.url).toBe(APNS + TOKEN);
    expect(sent.body).toBe(notifyPayload(readNotification(body.notification)!));
    expect(Object.fromEntries(sent.headers)).toMatchObject({
      'apns-topic': 'dev.runode.mobile',
      'apns-push-type': 'alert',
      'apns-priority': '10',
      'apns-expiration': String(body.expiration),
      'apns-collapse-id': '0123456789abcdef0011223344556677',
    });
    const jwt = sent.headers.get('authorization')!.replace('bearer ', '');
    const [header, claims, signature] = jwt.split('.') as [string, string, string];
    const json = (part: string) => JSON.parse(new TextDecoder().decode(decodeBase64url(part)!));
    expect(json(header)).toEqual({ alg: 'ES256', kid: 'KEYID12345' });
    expect(json(claims).iss).toBe('TEAMID1234');
    const valid = await crypto.subtle.verify(
      { name: 'ECDSA', hash: 'SHA-256' },
      apnsPublic,
      decodeBase64url(signature)!,
      new TextEncoder().encode(`${header}.${claims}`),
    );
    expect(valid).toBe(true);
    // 缓存着的 JWT 下一条接着用。
    await post(relay, '/v1/notify', body);
    expect(net.to(APNS)[1]!.headers.get('authorization')).toBe(`bearer ${jwt}`);
  });

  it('过期时刻夹在现在和三十分钟之后之间', async () => {
    const { relay, net } = await setup();
    const handle = await register(relay);
    await post(relay, '/v1/notify', notifyBody(handle, { expiration: 0 }));
    await post(relay, '/v1/notify', notifyBody(handle, { expiration: 9_999_999_999 }));
    const [early, late] = net.to(APNS).map((sent) => Number(sent.headers.get('apns-expiration')) - Math.floor(Date.now() / 1000));
    expect(Math.abs(early!)).toBeLessThanOrEqual(1);
    expect(Math.abs(late! - 1800)).toBeLessThanOrEqual(1);
  });

  it('APNs 不认 device token 时换另一个环境再发，两边都不认才回错', async () => {
    const { relay, net } = await setup();
    const handle = await register(relay, { env: 'development' });
    net.answer = (sent) => (sent.url.startsWith(SANDBOX) ? answer(400, 'BadDeviceToken') : answer(200));
    expect((await post(relay, '/v1/notify', notifyBody(handle))).status).toBe(200);
    expect(net.sent.map((sent) => sent.url.slice(0, 30))).toEqual([SANDBOX.slice(0, 30), APNS.slice(0, 30)]);

    net.answer = () => answer(410, 'Unregistered');
    expect(await reason(await post(relay, '/v1/notify', notifyBody(handle)))).toEqual([410, 'Unregistered']);
    // 另一个环境那次没拿到回话时照没拿到回话处理。
    net.answer = (sent) => {
      if (sent.url.startsWith(APNS)) throw new Error('down');
      return answer(410, 'Unregistered');
    };
    expect(await reason(await post(relay, '/v1/notify', notifyBody(handle)))).toEqual([502, 'ApnsUnreachable']);
  });

  it('JWT 过期时重签再发一次', async () => {
    const { relay, net } = await setup();
    const handle = await register(relay);
    let first = true;
    net.answer = () => {
      const expired = first;
      first = false;
      return expired ? answer(403, 'ExpiredProviderToken') : answer(200);
    };
    expect((await post(relay, '/v1/notify', notifyBody(handle))).status).toBe(200);
    const [a, b] = net.to(APNS).map((sent) => sent.headers.get('authorization'));
    expect(a).not.toBe(b);
  });

  it('APNs 的错误原样带回，Retry-After 跟着', async () => {
    const { relay, net } = await setup();
    const handle = await register(relay);
    net.answer = () => answer(429, 'TooManyRequests', { 'retry-after': '30' });
    const response = await post(relay, '/v1/notify', notifyBody(handle));
    expect(await reason(response)).toEqual([429, 'TooManyRequests']);
    expect(response.headers.get('retry-after')).toBe('30');
  });

  it('APNs 迟迟不回时到点回 504', async () => {
    const { relay, net } = await setup();
    const handle = await register(relay);
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    const reached = new Promise<void>((resolve) => {
      net.answer = () => {
        resolve();
        return new Promise(() => {});
      };
    });
    const response = post(relay, '/v1/notify', notifyBody(handle));
    await reached;
    await vi.advanceTimersByTimeAsync(11_999);
    expect(await Promise.race([response.then(() => 'answered'), Promise.resolve('waiting')])).toBe('waiting');
    await vi.advanceTimersByTimeAsync(1);
    expect(await reason(await response)).toEqual([504, 'ApnsUnreachable']);
  });

  it('句柄不对、电脑对不上、请求不对时回对应的原因', async () => {
    const { relay } = await setup();
    const handle = await register(relay);
    const other = { ...JSON.parse(fixture('relay_notify.json')).notification, machine: '00000000-0000-0000-0000-000000000000' };
    const cases: [object | string, [number, string]][] = [
      ['{', [400, 'BadRequest']],
      [notifyBody(handle, { v: 2 }), [400, 'UnsupportedVersion']],
      [notifyBody('bogus'), [400, 'BadDeviceToken']],
      [notifyBody(handle, { notification: other }), [403, 'MachineMismatch']],
      [notifyBody(handle, { expiration: -1 }), [400, 'BadRequest']],
    ];
    for (const [body, expected] of cases) {
      expect(await reason(await post(relay, '/v1/notify', body)), JSON.stringify(body).slice(0, 60)).toEqual(expected);
    }
  });

  it('同一部手机超了限频回 429，计数的键不带 token', async () => {
    const { relay, limits } = await setup();
    const handle = await register(relay);
    limits.notifyHandle.allow = false;
    expect(await reason(await post(relay, '/v1/notify', notifyBody(handle)))).toEqual([429, 'TooManyRequests']);
    expect(limits.notifyHandle.keys[0]).toMatch(/^[0-9a-f]{64}$/);
    expect(limits.notifyHandle.keys[0]).not.toContain(TOKEN);
  });

  it('FCM：换 access token 后发，401 时换一个再发，回话换成 APNs 的状态', async () => {
    const { relay, net, fcmPublic } = await setup();
    const handle = await register(relay, JSON.parse(fixture('relay_register_fcm.json')));
    const body = notifyBody(handle);
    expect((await post(relay, '/v1/notify', body)).status).toBe(200);
    const [token] = net.to(TOKEN_URI) as [Sent];
    const assertion = new URLSearchParams(token.body);
    expect(assertion.get('grant_type')).toBe('urn:ietf:params:oauth:grant-type:jwt-bearer');
    const [header, claims, signature] = assertion.get('assertion')!.split('.') as [string, string, string];
    const valid = await crypto.subtle.verify(
      'RSASSA-PKCS1-v1_5',
      fcmPublic,
      decodeBase64url(signature)!,
      new TextEncoder().encode(`${header}.${claims}`),
    );
    expect(valid).toBe(true);
    const [sent] = net.to(SEND_URL) as [Sent];
    expect(sent.headers.get('authorization')).toBe('Bearer access1');
    const now = Math.floor(Date.now() / 1000);
    expect(sent.body).toBe(fcmMessage(FCM_TOKEN, readNotification(body.notification)!, body.expiration, now));

    let refused = true;
    net.answer = (sent) => {
      if (sent.url === TOKEN_URI) return Response.json({ access_token: 'access2', expires_in: 3599 });
      const refuse = refused;
      refused = false;
      return refuse ? new Response(null, { status: 401 }) : new Response('{}');
    };
    expect((await post(relay, '/v1/notify', body)).status).toBe(200);
    expect(net.to(SEND_URL).slice(1).map((sent) => sent.headers.get('authorization'))).toEqual([
      'Bearer access1',
      'Bearer access2',
    ]);

    net.answer = () =>
      Response.json(
        { error: { code: 404, status: 'NOT_FOUND', details: [{ '@type': 'type.googleapis.com/google.firebase.fcm.v1.FcmError', errorCode: 'UNREGISTERED' }] } },
        { status: 404 },
      );
    expect(await reason(await post(relay, '/v1/notify', body))).toEqual([410, 'Unregistered']);
  });

  it('换不到 access token 时退避一阵，期间不再去换', async () => {
    const { relay, net } = await setup();
    const handle = await register(relay, JSON.parse(fixture('relay_register_fcm.json')));
    net.answer = (sent) => (sent.url === TOKEN_URI ? Response.json({ error: 'invalid_grant' }, { status: 400 }) : new Response('{}'));
    expect(await reason(await post(relay, '/v1/notify', notifyBody(handle)))).toEqual([502, 'FcmUnreachable']);
    expect(await reason(await post(relay, '/v1/notify', notifyBody(handle)))).toEqual([502, 'FcmUnreachable']);
    expect(net.to(TOKEN_URI).length).toBe(1);
  });

  it('没配 FCM 时 FCM 的句柄回 FcmNotConfigured', async () => {
    const withFcm = await setup();
    const handle = await register(withFcm.relay, JSON.parse(fixture('relay_register_fcm.json')));
    const { relay } = await setup({ fcm: false });
    relay.keys = withFcm.relay.keys;
    expect(await reason(await post(relay, '/v1/notify', notifyBody(handle)))).toEqual([400, 'FcmNotConfigured']);
  });
});

describe('别的', () => {
  it('不认的路径回 404，不是 POST 回 405，请求体太大回 413', async () => {
    const { relay } = await setup();
    expect((await handle(new Request('https://push.runode.dev/'), relay)).status).toBe(404);
    expect((await handle(new Request('https://push.runode.dev/v1/notify'), relay)).status).toBe(405);
    expect((await post(relay, '/v1/notify', 'x'.repeat(9000))).status).toBe(413);
  });

  it('没有 Content-Length 的大请求体读过上限就停，不整份读进来', async () => {
    const { relay, limits } = await setup();
    let pulled = 0;
    const body = new ReadableStream<Uint8Array>({
      pull(controller) {
        pulled += 1;
        controller.enqueue(new Uint8Array(1024));
      },
    });
    const request = new Request('https://push.runode.dev/v1/notify', { method: 'POST', body, duplex: 'half' } as RequestInit);
    expect((await handle(request, relay)).status).toBe(413);
    expect(pulled).toBeLessThan(20);
    expect(limits.notifyIp.keys).toEqual([]);
  });

  it('Content-Length 超过上限时不读请求体', async () => {
    const { relay } = await setup();
    const request = new Request('https://push.runode.dev/v1/register', {
      method: 'POST',
      body: '{}',
      headers: { 'content-length': '100000000' },
    });
    expect((await handle(request, relay)).status).toBe(413);
    expect(request.bodyUsed).toBe(false);
  });

  it('IPv6 按 /64 算，映射的 IPv4 照 IPv4 算', () => {
    expect(ipKey('2001:db8:1:2:aaaa::1')).toBe(ipKey('2001:db8:1:2:bbbb::9'));
    expect(ipKey('2001:db8:1:2:aaaa::1')).not.toBe(ipKey('2001:db8:1:3::1'));
    expect(ipKey('2001:0db8:0001:0002:0000:0000:0000:0001')).toBe('2001:db8:1:2::/64');
    expect(ipKey('::1')).toBe('0:0:0:0::/64');
    expect(ipKey('::ffff:10.0.0.1')).toBe('10.0.0.1');
    expect(ipKey('203.0.113.9')).toBe('203.0.113.9');
  });

  it('FCM 的回话换成电脑认得的 APNs 回话', () => {
    const fcmError = (status: number, code: string, message: string) => ({
      error: {
        code: status,
        message,
        status: status === 404 ? 'NOT_FOUND' : 'INVALID_ARGUMENT',
        details: [{ '@type': 'type.googleapis.com/google.firebase.fcm.v1.FcmError', errorCode: code }],
      },
    });
    expect(fcmReply(200, { name: 'projects/p/messages/1' }, null)).toEqual({ status: 200, reason: '', retryAfter: null });
    expect(fcmReply(404, fcmError(404, 'UNREGISTERED', 'gone'), '5')).toEqual({ status: 410, reason: 'Unregistered', retryAfter: null });
    expect(fcmReply(403, fcmError(403, 'SENDER_ID_MISMATCH', 'x'), null).reason).toBe('BadDeviceToken');
    const badToken = fcmError(400, 'INVALID_ARGUMENT', 'The registration token is not a valid FCM registration token');
    expect(fcmReply(400, badToken, null)).toMatchObject({ status: 400, reason: 'BadDeviceToken' });
    const field = {
      error: {
        code: 400,
        status: 'INVALID_ARGUMENT',
        message: 'Invalid value',
        details: [{ '@type': 'type.googleapis.com/google.rpc.BadRequest', fieldViolations: [{ field: 'message.token' }] }],
      },
    };
    expect(fcmReply(400, field, null).reason).toBe('BadDeviceToken');
    expect(fcmReply(400, fcmError(400, 'INVALID_ARGUMENT', "Invalid value at 'message.android.ttl'"), null)).toMatchObject({
      status: 400,
      reason: 'INVALID_ARGUMENT',
    });
    expect(fcmReply(429, fcmError(429, 'QUOTA_EXCEEDED', 'quota'), '60')).toEqual({ status: 429, reason: 'TooManyRequests', retryAfter: '60' });
    expect(fcmReply(503, fcmError(503, 'UNAVAILABLE', 'busy'), '10')).toEqual({ status: 503, reason: 'UNAVAILABLE', retryAfter: '10' });
    expect(fcmReply(502, {}, null)).toMatchObject({ status: 502, reason: '' });
    expect(fcmReply(404, { error: { code: 404, status: 'NOT_FOUND' } }, null)).toMatchObject({ status: 404, reason: 'NOT_FOUND' });
  });
});
