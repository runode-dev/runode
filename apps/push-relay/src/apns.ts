// 发给 APNs：用 Apple 开发者后台下载的 .p8 密钥签 JWT（ES256），`POST /3/device/<token>`，开发环境的
// 主机名多一段 `.sandbox`。APNs 只说 HTTP/2，Worker 的出站 fetch 在 Cloudflare 上跑时会用 HTTP/2，本地
// 的 `wrangler dev` 连不上 APNs。

import { encodeBase64url, encodeHex, type ApnsEnv } from './handle';

export type Fetch = (input: string, init: RequestInit) => Promise<Response>;

/** 推送服务的回话，中转按它回电脑。 */
export interface Reply {
  status: number;
  /** 响应体里的 `reason`；成功时为空。 */
  reason: string;
  retryAfter: string | null;
}

/** 要发的一条推送。 */
export interface Push {
  env: ApnsEnv;
  token: Uint8Array;
  payload: string;
  /** `apns-expiration`，Unix 秒。 */
  expiration: number;
  collapseId: string;
}

/** 官方 App 的 bundle id。 */
export const OFFICIAL_BUNDLE = 'dev.runode.mobile';
/** JWT 的 `iat` 按这么长的格对齐，到下一格才换新的（APNs 要求 20 分钟到 1 小时之间换）。各个 isolate 各签
 * 各的，对齐后同一时段签出来的 `iat` 一样。 */
const JWT_WINDOW = 40 * 60;
/** 一次请求最多等这么久，毫秒。 */
const TIMEOUT = 5_000;

/** APNs 不认这个 device token（410，或 400 `BadDeviceToken`）：可能是封进句柄的环境不对。 */
export function isBadToken(status: number, reason: string): boolean {
  return status === 410 || (status === 400 && reason === 'BadDeviceToken');
}

export function nowUnix(): number {
  return Math.floor(Date.now() / 1000);
}

export class Apns {
  private jwt: { value: string; iat: number } | null = null;

  private constructor(
    private readonly key: CryptoKey,
    private readonly keyId: string,
    private readonly teamId: string,
    private readonly fetch: Fetch,
  ) {}

  /** `pem` 是 .p8 文件的内容（PEM 包着的 PKCS#8 P-256 私钥）。 */
  static async create(pem: string, keyId: string, teamId: string, fetch: Fetch): Promise<Apns> {
    const der = pemDer(pem);
    if (!der) throw new Error('APNS_KEY is not a PEM file');
    const key = await crypto.subtle
      .importKey('pkcs8', der, { name: 'ECDSA', namedCurve: 'P-256' }, false, ['sign'])
      .catch((err: unknown) => {
        throw new Error(`APNS_KEY is not a P-256 private key: ${err}`);
      });
    return new Apns(key, keyId, teamId, fetch);
  }

  /** 发一条；JWT 过期（403 `ExpiredProviderToken`）时重签再发一次。没拿到回话时抛错。 */
  async send(push: Push): Promise<Reply> {
    const jwt = await this.token(null);
    const reply = await this.post(push, jwt);
    if (reply.status === 403 && reply.reason === 'ExpiredProviderToken') return this.post(push, await this.token(jwt));
    return reply;
  }

  /** 签好的 JWT：缓存着的还在这一格、也不是 `expired` 时原样给，不然重签；`expired` 正是这一格的就按现在
   * 的时刻签。 */
  private async token(expired: string | null): Promise<string> {
    const now = nowUnix();
    const slot = now - (now % JWT_WINDOW);
    if (this.jwt && this.jwt.iat >= slot && this.jwt.value !== expired) return this.jwt.value;
    const iat = this.jwt?.value === expired && this.jwt.iat >= slot ? now : slot;
    const value = await signJwt({ alg: 'ES256', kid: this.keyId }, { iss: this.teamId, iat }, (input) =>
      crypto.subtle.sign({ name: 'ECDSA', hash: 'SHA-256' }, this.key, input),
    );
    this.jwt = { value, iat };
    return value;
  }

  private async post(push: Push, jwt: string): Promise<Reply> {
    const host = push.env === 'production' ? 'api.push.apple.com' : 'api.sandbox.push.apple.com';
    let response: Response;
    try {
      response = await this.fetch(`https://${host}/3/device/${encodeHex(push.token)}`, {
        method: 'POST',
        headers: {
          authorization: `bearer ${jwt}`,
          'apns-topic': OFFICIAL_BUNDLE,
          'apns-push-type': 'alert',
          'apns-priority': '10',
          'apns-expiration': String(push.expiration),
          'apns-collapse-id': push.collapseId,
        },
        body: push.payload,
        signal: AbortSignal.timeout(TIMEOUT),
      });
    } catch (err) {
      throw new Error(`cannot reach APNs: ${err}`);
    }
    const body = await response.text().catch(() => '');
    return { status: response.status, reason: reasonOf(body), retryAfter: response.headers.get('retry-after') };
  }
}

/** APNs 错误回话 `{"reason":"…"}` 里的原因；读不出时为空。 */
function reasonOf(body: string): string {
  try {
    const reason: unknown = JSON.parse(body)?.reason;
    return typeof reason === 'string' ? reason : '';
  } catch {
    return '';
  }
}

/** `header.claims` 交给 `sign` 签，再拼上签名。 */
export async function signJwt(
  header: object,
  claims: object,
  sign: (input: Uint8Array) => Promise<ArrayBuffer>,
): Promise<string> {
  const text = (value: object) => encodeBase64url(new TextEncoder().encode(JSON.stringify(value)));
  const input = `${text(header)}.${text(claims)}`;
  const signature = await sign(new TextEncoder().encode(input));
  return `${input}.${encodeBase64url(new Uint8Array(signature))}`;
}

/** PEM 里 `-----` 行之间的 base64 解出来的 DER；不是 PEM 时为 `null`。 */
export function pemDer(pem: string): Uint8Array | null {
  const text = pem
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => !line.startsWith('-----'))
    .join('');
  if (text === '' || !/^[A-Za-z0-9+/]*={0,2}$/.test(text)) return null;
  try {
    return Uint8Array.from(atob(text), (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
}
