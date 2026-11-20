// runode 官方 App 的推送中转。手机拿到推送的 token（iPhone 是 APNs 的 device token，Android 是 FCM 的
// registration token）后为每台电脑换一个句柄（`/v1/register`），电脑只拿得到句柄，推送时交回来
// （`/v1/notify`），中转解开句柄，签好名发给 APNs 或 FCM。请求体和错误码以 `runode_protocol::push` 为准。
//
// 出错时回 `{"reason":"…"}`，限频时回 429 带 `Retry-After`；APNs 的回话原样带回，FCM 的换成同样意思的
// APNs 状态带回，电脑按状态决定重试、删登记还是放弃。没配 FCM 时 FCM 的登记和通知都回 400
// `FcmNotConfigured`。中转不存状态，日志里不记 token、句柄和通知内容。

import { Apns, isBadToken, nowUnix, type Fetch, type Push, type Reply } from './apns';
import { Fcm } from './fcm';
import { Keys, concat, decodeHex, encodeHex, parseUuid, type ApnsEnv, type Target } from './handle';
import { EXPIRES_AFTER, fcmMessage, isFcmToken, notifyPayload, readNotification } from './payload';

/** Workers 的 Rate Limiting 绑定。 */
export interface Limiter {
  limit(options: { key: string }): Promise<{ success: boolean }>;
}

export interface Env {
  /** 逗号隔开的 `<编号>=<密钥>`，`Keys.parse` 的格式。 */
  RELAY_HANDLE_KEYS: string;
  /** .p8 文件的内容。 */
  APNS_KEY: string;
  APNS_KEY_ID: string;
  APNS_TEAM_ID: string;
  /** Firebase 服务账号 JSON 的内容；不设时不给 Android 推。 */
  FCM_SERVICE_ACCOUNT?: string;
  /** 同一个地址每分钟最多换 30 次句柄。 */
  REGISTER_PER_IP: Limiter;
  /** 同一个地址每分钟最多发 120 条通知（一台电脑给好几部手机推，每部算一条）。 */
  NOTIFY_PER_IP: Limiter;
  /** 同一部手机在同一台电脑上每分钟最多收 10 条。 */
  NOTIFY_PER_HANDLE: Limiter;
}

export interface Relay {
  keys: Keys;
  apns: Apns;
  /** 没配 FCM 的服务账号时为 `null`。 */
  fcm: Fcm | null;
  limits: { registerPerIp: Limiter; notifyPerIp: Limiter; notifyPerHandle: Limiter };
}

export const RELAY_REGISTER = '/v1/register';
export const RELAY_NOTIFY = '/v1/notify';
const RELAY_VERSION = 1;
/** 请求体最多这么多字节。 */
const MAX_BODY = 8 * 1024;
/** device token 解出来的字节数要在这个范围里（APNs 现在给 32 字节）。 */
const TOKEN_BYTES = [16, 100] as const;
/** 限频的窗口，秒；Rate Limiting 绑定不告诉还要等多久，`Retry-After` 回整个窗口。 */
const LIMIT_PERIOD = 60;
/** 一次通知连同换环境、重签 JWT、换 FCM 的 access token 在内，发给 APNs 或 FCM 最多花这么久，毫秒。要短于
 * 电脑等中转回话的时限，不然电脑判超时重发，手机会响两次。 */
const DELIVER_DEADLINE = 12_000;
const FCM_NOT_CONFIGURED = 'FcmNotConfigured';

let relay: Relay | undefined;

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    try {
      relay ??= await createRelay(env, (input, init) => fetch(input, init));
    } catch (err) {
      console.error(`cannot start the relay: ${err}`);
      return error(500, 'InternalServerError');
    }
    return handle(request, relay);
  },
} satisfies ExportedHandler<Env>;

export async function createRelay(env: Env, fetch: Fetch): Promise<Relay> {
  const missing = (name: string) => new Error(`${name} is not set`);
  if (!env.RELAY_HANDLE_KEYS) throw missing('RELAY_HANDLE_KEYS');
  if (!env.APNS_KEY) throw missing('APNS_KEY');
  if (!env.APNS_KEY_ID) throw missing('APNS_KEY_ID');
  if (!env.APNS_TEAM_ID) throw missing('APNS_TEAM_ID');
  return {
    keys: await Keys.parse(env.RELAY_HANDLE_KEYS),
    apns: await Apns.create(env.APNS_KEY, env.APNS_KEY_ID, env.APNS_TEAM_ID, fetch),
    fcm: env.FCM_SERVICE_ACCOUNT ? await Fcm.create(env.FCM_SERVICE_ACCOUNT, fetch) : null,
    limits: { registerPerIp: env.REGISTER_PER_IP, notifyPerIp: env.NOTIFY_PER_IP, notifyPerHandle: env.NOTIFY_PER_HANDLE },
  };
}

export async function handle(request: Request, relay: Relay): Promise<Response> {
  const path = new URL(request.url).pathname;
  const route = path === RELAY_REGISTER ? register : path === RELAY_NOTIFY ? notify : null;
  if (!route) return new Response(null, { status: 404 });
  if (request.method !== 'POST') return new Response(null, { status: 405, headers: { allow: 'POST' } });
  const body = await readBody(request);
  if (!body) return error(413, 'PayloadTooLarge');
  let json: unknown;
  try {
    json = JSON.parse(new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(body));
  } catch {
    json = undefined;
  }
  return route(relay, ipKey(request.headers.get('cf-connecting-ip') ?? ''), json);
}

/** 读整个请求体；超过 `MAX_BODY` 时读到超出就停，回 `null`。 */
async function readBody(request: Request): Promise<Uint8Array | null> {
  if (Number(request.headers.get('content-length')) > MAX_BODY) return null;
  const chunks: Uint8Array[] = [];
  let size = 0;
  const reader = request.body?.getReader();
  while (reader) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > MAX_BODY) {
      await reader.cancel();
      return null;
    }
    chunks.push(value);
  }
  return concat(chunks);
}

async function register(relay: Relay, ip: string, json: unknown): Promise<Response> {
  if (!(await relay.limits.registerPerIp.limit({ key: ip })).success) return tooMany();
  const request = json as Record<string, unknown> | null;
  if (
    typeof request !== 'object' ||
    request === null ||
    !Number.isSafeInteger(request.v) ||
    typeof request.env !== 'string' ||
    typeof request.token !== 'string' ||
    typeof request.machine !== 'string' ||
    !(request.service === undefined || typeof request.service === 'string')
  ) {
    return error(400, 'BadRequest');
  }
  if (request.v !== RELAY_VERSION) return error(400, 'UnsupportedVersion');
  let target: Target;
  let token: Uint8Array;
  const service = request.service ?? 'apns';
  if (service === 'apns') {
    if (request.env !== 'production' && request.env !== 'development') return error(400, 'BadEnvironment');
    const bytes = decodeHex(request.token);
    if (!bytes || bytes.length < TOKEN_BYTES[0] || bytes.length > TOKEN_BYTES[1]) return error(400, 'BadDeviceToken');
    target = { service: 'apns', env: request.env as ApnsEnv };
    token = bytes;
  } else if (service === 'fcm') {
    if (!relay.fcm) return error(400, FCM_NOT_CONFIGURED);
    if (!isFcmToken(request.token)) return error(400, 'BadDeviceToken');
    target = { service: 'fcm' };
    token = new TextEncoder().encode(request.token);
  } else {
    return error(400, 'BadService');
  }
  const machine = parseUuid(request.machine);
  if (!machine) return error(400, 'BadMachine');
  return Response.json({ handle: await relay.keys.seal({ target, machine, token }) });
}

async function notify(relay: Relay, ip: string, json: unknown): Promise<Response> {
  if (!(await relay.limits.notifyPerIp.limit({ key: ip })).success) return tooMany();
  const request = json as Record<string, unknown> | null;
  const notification = typeof request === 'object' && request !== null ? readNotification(request.notification) : null;
  if (
    !request ||
    !notification ||
    !Number.isSafeInteger(request.v) ||
    typeof request.handle !== 'string' ||
    !Number.isSafeInteger(request.expiration) ||
    (request.expiration as number) < 0
  ) {
    return error(400, 'BadRequest');
  }
  if (request.v !== RELAY_VERSION) return error(400, 'UnsupportedVersion');
  const sealed = await relay.keys.open(request.handle);
  if (!sealed) return error(400, 'BadDeviceToken');
  const machine = parseUuid(notification.machine);
  if (!machine || encodeHex(machine) !== encodeHex(sealed.machine)) return error(403, 'MachineMismatch');
  // 计数的键是 token 和电脑的摘要，token 不交给限频服务。
  const phone = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(encodeHex(sealed.token) + encodeHex(machine)));
  if (!(await relay.limits.notifyPerHandle.limit({ key: encodeHex(new Uint8Array(phone)) })).success) return tooMany();
  const now = nowUnix();
  const expiration = Math.min(Math.max(request.expiration as number, now), now + EXPIRES_AFTER);
  const started = Date.now();
  let service: string;
  let sending: Promise<Reply>;
  if (sealed.target.service === 'apns') {
    const payload = notifyPayload(notification);
    if (payload === null) return error(400, 'PayloadTooLarge');
    service = 'Apns';
    const push = { env: sealed.target.env, token: sealed.token, payload, expiration, collapseId: notification.session };
    sending = deliver(relay.apns, push);
  } else {
    if (!relay.fcm) return error(400, FCM_NOT_CONFIGURED);
    const message = fcmMessage(new TextDecoder().decode(sealed.token), notification, expiration, now);
    if (message === null) return error(400, 'PayloadTooLarge');
    service = 'Fcm';
    sending = relay.fcm.send(message);
  }
  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<'late'>((resolve) => {
    timer = setTimeout(() => resolve('late'), DELIVER_DEADLINE);
  });
  let reply: Reply | 'late';
  try {
    reply = await Promise.race([sending, deadline]);
  } catch (err) {
    console.warn(`push via ${service} failed after ${Date.now() - started}ms: ${err}`);
    return error(502, `${service}Unreachable`);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
  if (reply === 'late') {
    console.warn(`push via ${service} gave up after ${Date.now() - started}ms`);
    return error(504, `${service}Unreachable`);
  }
  console.log(`push via ${service} answered ${reply.status} ${reply.reason} after ${Date.now() - started}ms`);
  if (reply.status >= 200 && reply.status < 300) return new Response(null, { status: 200 });
  const response = error(reply.status, reply.reason);
  if (reply.retryAfter !== null) response.headers.set('retry-after', reply.retryAfter);
  return response;
}

/** 发给 APNs；不认 device token 时换另一个环境再发一次。两边都明说不认才算不认；另一个环境那次没拿到回话时
 * 照没拿到回话处理，不拿第一次的回话顶替：第一次不认可能只是环境不对。 */
async function deliver(apns: Apns, push: Push): Promise<Reply> {
  const reply = await apns.send(push);
  if (!isBadToken(reply.status, reply.reason)) return reply;
  return apns.send({ ...push, env: push.env === 'production' ? 'development' : 'production' });
}

function error(status: number, reason: string): Response {
  return Response.json({ reason }, { status });
}

function tooMany(): Response {
  const response = error(429, 'TooManyRequests');
  response.headers.set('retry-after', String(LIMIT_PERIOD));
  return response;
}

/** 按地址限频时用的键：IPv6 按 /64 算，一台机器通常分到整个 /64，换个地址绕不过去。 */
export function ipKey(ip: string): string {
  if (!ip.includes(':')) return ip;
  const mapped = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/i.exec(ip);
  if (mapped) return mapped[1]!;
  const [head, tail] = ip.toLowerCase().split('::');
  const left = head ? head.split(':') : [];
  const right = tail ? tail.split(':') : [];
  const groups = tail === undefined ? left : [...left, ...Array<string>(8 - left.length - right.length).fill('0'), ...right];
  return `${groups.slice(0, 4).map((group) => parseInt(group, 16).toString(16)).join(':')}::/64`;
}
