import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRelay, type Env, type Limiter, type Relay } from '../src/index';

export const NOW = 1_760_000_000;
export const TOKEN = '80f0c8b3a4e2d1c0ffeeddccbbaa99887766554433221100aabbccddeeff0011';
export const FCM_TOKEN =
  'fGx3Q9kVTm2b7WcLr0pYnA:APA91bE7tq2Kc0VhS8xJm4RzL1wPdN6uYf3Hg5oBk9aTe2Ci7Xv0Mn4Qs8Dj1Ur6Wy3Pl5Zh2Ko9Ib7Ga4Fc0Ex_-';
export const MACHINE = '6F9619FF-8B86-D011-B42D-00C04FC964FF';
export const TOKEN_URI = 'https://oauth2.googleapis.com/token';
export const SEND_URL = 'https://fcm.googleapis.com/v1/projects/p/messages:send';

/** `runode_protocol` 的推送样例，去掉结尾的换行。 */
export function fixture(name: string): string {
  const path = join(dirname(fileURLToPath(import.meta.url)), '../../../crates/protocol/tests/fixtures/push', name);
  return readFileSync(path, 'utf8').replace(/\n+$/, '');
}

export async function pem(key: CryptoKey): Promise<string> {
  const base64 = Buffer.from(await crypto.subtle.exportKey('pkcs8', key)).toString('base64');
  return `-----BEGIN PRIVATE KEY-----\n${base64.match(/.{1,64}/g)!.join('\n')}\n-----END PRIVATE KEY-----\n`;
}

export interface Sent {
  url: string;
  headers: Headers;
  body: string;
}

/** 假的出站请求：记下每一次，交给 `answer` 回话；`answer` 抛错就是连不上。 */
export class FakeNet {
  sent: Sent[] = [];
  constructor(public answer: (sent: Sent) => Response | Promise<Response>) {}
  fetch = async (url: string, init: RequestInit): Promise<Response> => {
    const sent = { url, headers: new Headers(init.headers), body: String(init.body ?? '') };
    this.sent.push(sent);
    return this.answer(sent);
  };
  to(prefix: string): Sent[] {
    return this.sent.filter((sent) => sent.url.startsWith(prefix));
  }
}

/** 假的限频：`allow` 为假时一律不放行，记下用过的键。 */
export class FakeLimiter implements Limiter {
  allow = true;
  keys: string[] = [];
  async limit({ key }: { key: string }) {
    this.keys.push(key);
    return { success: this.allow };
  }
}

export interface Setup {
  relay: Relay;
  net: FakeNet;
  limits: { register: FakeLimiter; notifyIp: FakeLimiter; notifyHandle: FakeLimiter };
  apnsPublic: CryptoKey;
  fcmPublic: CryptoKey;
}

export const HANDLE_KEY = '1=AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8';

/** 用现生成的 APNs 和 FCM 密钥起一个中转；`fcm` 为假时不配 FCM。出站请求默认都回 200。 */
export async function setup({ fcm = true } = {}): Promise<Setup> {
  const ec = (await crypto.subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, true, [
    'sign',
    'verify',
  ])) as CryptoKeyPair;
  const rsa = (await crypto.subtle.generateKey(
    { name: 'RSASSA-PKCS1-v1_5', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' },
    true,
    ['sign', 'verify'],
  )) as CryptoKeyPair;
  const account = JSON.stringify({
    type: 'service_account',
    project_id: 'p',
    private_key: await pem(rsa.privateKey),
    client_email: 'relay@p.iam.gserviceaccount.com',
    token_uri: TOKEN_URI,
  });
  const limits = { register: new FakeLimiter(), notifyIp: new FakeLimiter(), notifyHandle: new FakeLimiter() };
  const env: Env = {
    RELAY_HANDLE_KEYS: HANDLE_KEY,
    APNS_KEY: await pem(ec.privateKey),
    APNS_KEY_ID: 'KEYID12345',
    APNS_TEAM_ID: 'TEAMID1234',
    FCM_SERVICE_ACCOUNT: fcm ? account : undefined,
    REGISTER_PER_IP: limits.register,
    NOTIFY_PER_IP: limits.notifyIp,
    NOTIFY_PER_HANDLE: limits.notifyHandle,
  };
  const net = new FakeNet((sent) =>
    sent.url === TOKEN_URI ? Response.json({ access_token: 'access1', expires_in: 3599 }) : new Response(null),
  );
  return { relay: await createRelay(env, net.fetch), net, limits, apnsPublic: ec.publicKey, fcmPublic: rsa.publicKey };
}
