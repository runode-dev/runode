// 句柄：发给谁（一个字节）、电脑的 UUID 和 token 用 AES-256-GCM 封起来，前面带上密钥编号和 nonce，整个
// base64url（不带填充）。解得开就是这个中转发出去的，内容没被改过。
//
// 电脑存着中转发出去的句柄，所以发给谁的那个字节只能追加新值，已有的取值和附加数据（`AAD`）不能改。

export type ApnsEnv = 'production' | 'development';

/** 推送发给谁：APNs 的这个环境，或者 FCM。 */
export type Target = { service: 'apns'; env: ApnsEnv } | { service: 'fcm' };

/** 封进句柄的东西。 */
export interface Sealed {
  target: Target;
  machine: Uint8Array;
  /** APNs 的 device token（解开十六进制后的字节），或者 FCM 的 registration token 原文的 UTF-8。 */
  token: Uint8Array;
}

/** 句柄和 device token 最多这么多个字符。 */
export const MAX_TOKEN_LEN = 512;
/** 绑在密文上的附加数据；换掉它，电脑存着的句柄就全都解不开了。 */
const AAD = new TextEncoder().encode('runode-push-v1');
const NONCE_LEN = 12;
const KEY_LEN = 32;

/** 封句柄的密钥。第一把用来封，所有的都能解：换密钥时把新的放在最前面，旧的留到手机都重新换过句柄。 */
export class Keys {
  private constructor(private readonly keys: [number, CryptoKey][]) {}

  /** `spec` 是逗号隔开的 `<编号>=<32 字节的 base64url>`，编号 0 到 255、不重复。 */
  static async parse(spec: string): Promise<Keys> {
    const keys: [number, CryptoKey][] = [];
    for (const item of spec.split(',').map((item) => item.trim()).filter((item) => item !== '')) {
      const at = item.indexOf('=');
      if (at < 0) throw new Error(`\`${item}\` is not <id>=<key>`);
      const idText = item.slice(0, at).trim();
      const id = /^\d{1,3}$/.test(idText) ? Number(idText) : NaN;
      if (!(id <= 255)) throw new Error(`key id \`${idText}\` is not 0 to 255`);
      if (keys.some(([known]) => known === id)) throw new Error(`key id ${id} appears twice`);
      const bytes = decodeBase64url(item.slice(at + 1).trim());
      if (!bytes) throw new Error(`key ${id} is not base64url`);
      if (bytes.length !== KEY_LEN) throw new Error(`key ${id} is ${bytes.length} bytes, not ${KEY_LEN}`);
      keys.push([id, await crypto.subtle.importKey('raw', bytes, 'AES-GCM', false, ['encrypt', 'decrypt'])]);
    }
    if (keys.length === 0) throw new Error('no handle key is given');
    return new Keys(keys);
  }

  /** 用第一把密钥封。 */
  async seal(sealed: Sealed): Promise<string> {
    const [id, key] = this.keys[0]!;
    const nonce = crypto.getRandomValues(new Uint8Array(NONCE_LEN));
    const plain = concat([Uint8Array.of(targetByte(sealed.target)), sealed.machine, sealed.token]);
    const data = await crypto.subtle.encrypt({ name: 'AES-GCM', iv: nonce, additionalData: AAD }, key, plain);
    return encodeBase64url(concat([Uint8Array.of(id), nonce, new Uint8Array(data)]));
  }

  /** 解句柄；不是 base64url、密钥编号不认识、被改过时为 `null`。 */
  async open(handle: string): Promise<Sealed | null> {
    if (handle.length > MAX_TOKEN_LEN) return null;
    const bytes = decodeBase64url(handle);
    if (!bytes || bytes.length < 1 + NONCE_LEN) return null;
    const key = this.keys.find(([known]) => known === bytes[0])?.[1];
    if (!key) return null;
    const iv = bytes.subarray(1, 1 + NONCE_LEN);
    let plain: Uint8Array;
    try {
      const data = bytes.subarray(1 + NONCE_LEN);
      plain = new Uint8Array(await crypto.subtle.decrypt({ name: 'AES-GCM', iv, additionalData: AAD }, key, data));
    } catch {
      return null;
    }
    if (plain.length < 1 + 16) return null;
    const target = TARGETS[plain[0]!];
    if (!target) return null;
    return { target, machine: plain.slice(1, 17), token: plain.slice(17) };
  }
}

/** 一把新的密钥，写成 `Keys.parse` 认的 base64url。 */
export function generateKey(): string {
  return encodeBase64url(crypto.getRandomValues(new Uint8Array(KEY_LEN)));
}

/** 封进句柄的那个字节是这张表的下标。 */
const TARGETS: Target[] = [
  { service: 'apns', env: 'production' },
  { service: 'apns', env: 'development' },
  { service: 'fcm' },
];

function targetByte(target: Target): number {
  if (target.service === 'fcm') return 2;
  return target.env === 'production' ? 0 : 1;
}

/** `6F9619FF-8B86-D011-B42D-00C04FC964FF` 这样带连字符的 UUID，不分大小写。 */
export function parseUuid(text: string): Uint8Array | null {
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(text)) return null;
  return decodeHex(text.replaceAll('-', ''));
}

/** 十六进制（不分大小写）解成字节；长度是奇数或有别的字符时为 `null`。 */
export function decodeHex(text: string): Uint8Array | null {
  if (text.length % 2 !== 0 || !/^[0-9a-f]*$/i.test(text)) return null;
  return Uint8Array.from({ length: text.length / 2 }, (_, i) => parseInt(text.slice(i * 2, i * 2 + 2), 16));
}

/** 小写十六进制。 */
export function encodeHex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

/** 不带填充的 base64url。 */
export function encodeBase64url(bytes: Uint8Array): string {
  let binary = '';
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
}

/** 不带填充的 base64url 解成字节；有别的字符或长度不可能时为 `null`。 */
export function decodeBase64url(text: string): Uint8Array | null {
  if (!/^[A-Za-z0-9_-]*$/.test(text) || text.length % 4 === 1) return null;
  const binary = atob(text.replaceAll('-', '+').replaceAll('_', '/'));
  return Uint8Array.from(binary, (c) => c.charCodeAt(0));
}

export function concat(parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((sum, part) => sum + part.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}
