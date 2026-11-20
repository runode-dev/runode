// 发给 FCM：用 Firebase 项目的服务账号签一个 JWT（RS256），向 Google 换 OAuth 的 access token，再拿它
// `POST` 到 HTTP v1 的 `messages:send`。FCM 的回话换成电脑认得的 APNs 状态（`reply`）。

import { isBadToken, nowUnix, pemDer, signJwt, type Fetch, type Reply } from './apns';

/** 发 FCM 的服务账号要的权限：只能发 FCM 消息。 */
const SCOPE = 'https://www.googleapis.com/auth/firebase.messaging';
/** 签给 Google 的 JWT 的有效期，秒；Google 收的最长就是一小时。 */
const JWT_LIFETIME = 60 * 60;
/** access token 离过期还有这么久就换新的，毫秒。 */
const RENEW_BEFORE = 5 * 60_000;
/** 换不到 access token 后隔这么久才再换，毫秒。 */
const FETCH_BACKOFF = 5_000;
/** 一次请求最多等这么久，毫秒。 */
const TIMEOUT = 5_000;

export class Fcm {
  private access: { token: string; expiresAt: number } | null = null;
  private failure: { why: string; retryAt: number } | null = null;

  private constructor(
    private readonly key: CryptoKey,
    private readonly clientEmail: string,
    private readonly tokenUri: string,
    private readonly sendUrl: string,
    private readonly fetch: Fetch,
  ) {}

  /** `json` 是 Firebase 控制台下载的服务账号 JSON 的内容。 */
  static async create(json: string, fetch: Fetch): Promise<Fcm> {
    let account: Record<string, unknown>;
    try {
      account = JSON.parse(json);
    } catch (err) {
      throw new Error(`FCM_SERVICE_ACCOUNT is not a service account JSON: ${err}`);
    }
    const field = (name: string) => {
      const value = account[name];
      if (typeof value !== 'string' || value === '') throw new Error(`FCM_SERVICE_ACCOUNT has no \`${name}\``);
      return value;
    };
    const der = pemDer(field('private_key'));
    if (!der) throw new Error('the private key in FCM_SERVICE_ACCOUNT is not PEM');
    const key = await crypto.subtle
      .importKey('pkcs8', der, { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' }, false, ['sign'])
      .catch((err: unknown) => {
        throw new Error(`the private key in FCM_SERVICE_ACCOUNT is not an RSA key: ${err}`);
      });
    const sendUrl = `https://fcm.googleapis.com/v1/projects/${field('project_id')}/messages:send`;
    return new Fcm(key, field('client_email'), field('token_uri'), sendUrl, fetch);
  }

  /** 发一条；access token 不认（401）时换一个再发一次。没拿到回话时抛错。 */
  async send(message: string): Promise<Reply> {
    const token = await this.accessToken(null);
    const reply = await this.post(message, token);
    if (reply.status === 401) return this.post(message, await this.accessToken(token));
    return reply;
  }

  /** 缓存着的离过期还远于 `RENEW_BEFORE`、也不是 FCM 刚不认的 `rejected` 时原样给，不然换一个新的；在上回
   * 没换到后的退避期里不换。换不到时缓存着的还没过期就拿它兜底。 */
  private async accessToken(rejected: string | null): Promise<string> {
    if (this.access && this.access.token === rejected) this.access = null;
    if (this.access && Date.now() + RENEW_BEFORE < this.access.expiresAt) return this.access.token;
    try {
      if (this.failure && Date.now() < this.failure.retryAt) throw new Error(`${this.failure.why} (backing off)`);
      try {
        this.access = await this.fetchAccessToken();
      } catch (err) {
        this.failure = { why: String(err), retryAt: Date.now() + FETCH_BACKOFF };
        throw err;
      }
      this.failure = null;
      return this.access.token;
    } catch (err) {
      if (this.access && Date.now() < this.access.expiresAt) return this.access.token;
      throw err;
    }
  }

  /** 向 Google 换一个 access token；过期时刻从发出请求时算起。 */
  private async fetchAccessToken(): Promise<{ token: string; expiresAt: number }> {
    const iat = nowUnix();
    const claims = { iss: this.clientEmail, scope: SCOPE, aud: this.tokenUri, iat, exp: iat + JWT_LIFETIME };
    const assertion = await signJwt({ alg: 'RS256', typ: 'JWT' }, claims, (input) =>
      crypto.subtle.sign('RSASSA-PKCS1-v1_5', this.key, input),
    );
    const sent = Date.now();
    let response: Response;
    try {
      response = await this.fetch(this.tokenUri, {
        method: 'POST',
        headers: { 'content-type': 'application/x-www-form-urlencoded' },
        body: `grant_type=${encodeURIComponent('urn:ietf:params:oauth:grant-type:jwt-bearer')}&assertion=${assertion}`,
        signal: AbortSignal.timeout(TIMEOUT),
      });
    } catch (err) {
      throw new Error(`cannot reach Google for an FCM access token: ${err}`);
    }
    const body = await jsonOf(response);
    if (typeof body.access_token === 'string' && Number.isSafeInteger(body.expires_in)) {
      return { token: body.access_token, expiresAt: sent + (body.expires_in as number) * 1000 };
    }
    const error = typeof body.error === 'string' ? body.error : '';
    const description = typeof body.error_description === 'string' ? `: ${body.error_description}` : '';
    throw new Error(`Google gave no FCM access token: HTTP ${response.status} ${error}${description}`);
  }

  private async post(message: string, token: string): Promise<Reply> {
    let response: Response;
    try {
      response = await this.fetch(this.sendUrl, {
        method: 'POST',
        headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
        body: message,
        signal: AbortSignal.timeout(TIMEOUT),
      });
    } catch (err) {
      throw new Error(`cannot reach FCM: ${err}`);
    }
    return reply(response.status, await jsonOf(response), response.headers.get('retry-after'));
  }
}

async function jsonOf(response: Response): Promise<Record<string, unknown>> {
  try {
    const body: unknown = JSON.parse(await response.text());
    return typeof body === 'object' && body !== null ? (body as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

/** FCM 的回话换成电脑认得的 APNs 回话：成功是 200、原因为空；token 作废了（`UNREGISTERED`）是 410
 * `Unregistered`，token 不对（针对 token 的 `INVALID_ARGUMENT`、`SENDER_ID_MISMATCH`）是 400
 * `BadDeviceToken`，电脑见了都删掉登记；限额（`QUOTA_EXCEEDED`）是 429 `TooManyRequests`。别的照 FCM 的状态
 * 带回，原因是它的错误码。成功的和电脑见了会删掉登记的不带 `Retry-After`。 */
export function reply(status: number, body: Record<string, unknown>, retryAfter: string | null): Reply {
  if (status >= 200 && status < 300) return { status: 200, reason: '', retryAfter: null };
  const error = (typeof body.error === 'object' && body.error !== null ? body.error : {}) as Record<string, unknown>;
  const details = (Array.isArray(error.details) ? error.details : []) as Record<string, unknown>[];
  const detailsOf = (type: string) => details.filter((detail) => detail?.['@type'] === `type.googleapis.com/${type}`);
  const errorCode = detailsOf('google.firebase.fcm.v1.FcmError')
    .map((detail) => detail.errorCode)
    .find((code) => typeof code === 'string');
  const code = (errorCode ?? (typeof error.status === 'string' ? error.status : '')) as string;
  const aboutToken = () => {
    const field = detailsOf('google.rpc.BadRequest').some((detail) =>
      (Array.isArray(detail.fieldViolations) ? detail.fieldViolations : []).some(
        (violation: { field?: unknown } | null) => violation?.field === 'message.token',
      ),
    );
    const message = typeof error.message === 'string' ? error.message.toLowerCase() : '';
    return field || message.includes('registration token');
  };
  let mapped: [number, string];
  if (code === 'UNREGISTERED') mapped = [410, 'Unregistered'];
  else if (code === 'SENDER_ID_MISMATCH') mapped = [400, 'BadDeviceToken'];
  else if (code === 'INVALID_ARGUMENT' && aboutToken()) mapped = [400, 'BadDeviceToken'];
  else if (code === 'QUOTA_EXCEEDED') mapped = [429, 'TooManyRequests'];
  else mapped = [status, code];
  const [mappedStatus, reason] = mapped;
  return { status: mappedStatus, reason, retryAfter: isBadToken(mappedStatus, reason) ? null : retryAfter };
}
