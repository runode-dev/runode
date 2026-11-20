import { describe, expect, it } from 'vitest';
import { Keys, decodeBase64url, decodeHex, encodeBase64url, encodeHex, generateKey, parseUuid, type Sealed } from '../src/handle';
import { MAX_TOKEN_LEN } from '../src/handle';
import { MAX_FCM_TOKEN_LEN } from '../src/payload';

const sealed = (change: Partial<Sealed> = {}): Sealed => ({
  target: { service: 'apns', env: 'development' },
  machine: new Uint8Array(16).fill(7),
  token: Uint8Array.of(0x80, 0xf0, 0x0a, 0xff),
  ...change,
});

describe('句柄', () => {
  it('封了解得开，写成 base64url，每次封出来都不一样', async () => {
    const keys = await Keys.parse(`3=${generateKey()}`);
    const handle = await keys.seal(sealed());
    expect(handle).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(await keys.open(handle)).toEqual(sealed());
    expect(await keys.seal(sealed())).not.toBe(handle);
    for (const target of [{ service: 'apns', env: 'production' }, { service: 'fcm' }] as const) {
      expect(await keys.open(await keys.seal(sealed({ target })))).toEqual(sealed({ target }));
    }
  });

  it('线上那一版中转封的句柄照样解得开', async () => {
    const keys = await Keys.parse('1=AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8');
    const handle = 'AUAY3Q01jEjkrOlHBsDPLE1skW3NZ7OuuKCaGsOqxhHVbH1t920O7GpTRSyVbN72HMs';
    expect(await keys.open(handle)).toEqual(sealed());
  });

  it('最长的 FCM token 封出来也不超过电脑收的长度', async () => {
    const keys = await Keys.parse(`255=${generateKey()}`);
    const longest = sealed({ target: { service: 'fcm' }, token: new Uint8Array(MAX_FCM_TOKEN_LEN).fill(65) });
    const handle = await keys.seal(longest);
    expect(handle.length).toBeLessThanOrEqual(MAX_TOKEN_LEN);
    expect(await keys.open(handle)).toEqual(longest);
  });

  it('换密钥：旧密钥封的照样解得开，去掉旧密钥后就解不开了', async () => {
    const [old, next] = [generateKey(), generateKey()];
    const handle = await (await Keys.parse(`1=${old}`)).seal(sealed());
    expect(await (await Keys.parse(`2=${next},1=${old}`)).open(handle)).toEqual(sealed());
    expect(await (await Keys.parse(`2=${next}`)).open(handle)).toBeNull();
  });

  it('被改过的解不开', async () => {
    const keys = await Keys.parse(`0=${generateKey()}`);
    const bytes = decodeBase64url(await keys.seal(sealed()))!;
    bytes[bytes.length - 1]! ^= 1;
    expect(await keys.open(encodeBase64url(bytes))).toBeNull();
    expect(await keys.open('not a handle')).toBeNull();
    expect(await keys.open('')).toBeNull();
    expect(await keys.open('A'.repeat(MAX_TOKEN_LEN + 4))).toBeNull();
  });

  it('密钥的写法要对', async () => {
    const key = generateKey();
    await expect(Keys.parse('')).rejects.toThrow();
    await expect(Keys.parse(`1=${key},1=${key}`)).rejects.toThrow();
    await expect(Keys.parse(`256=${key}`)).rejects.toThrow();
    await expect(Keys.parse('1=AAAA')).rejects.toThrow();
    await expect(Keys.parse(` 1 = ${key} , `)).resolves.toBeInstanceOf(Keys);
  });

  it('UUID 和十六进制解得严', () => {
    const uuid = parseUuid('6F9619FF-8B86-D011-B42D-00C04FC964FF')!;
    expect(parseUuid('6f9619ff-8b86-d011-b42d-00c04fc964ff')).toEqual(uuid);
    expect(encodeHex(uuid)).toBe('6f9619ff8b86d011b42d00c04fc964ff');
    expect(parseUuid('6F9619FF8B86D011B42D00C04FC964FF')).toBeNull();
    expect(parseUuid('6F9619FF-8B86-D011-B42D-00C04FC964FG')).toBeNull();
    expect(decodeHex('80f00A')).toEqual(Uint8Array.of(0x80, 0xf0, 0x0a));
    expect(decodeHex('80f')).toBeNull();
    expect(decodeHex('+0')).toBeNull();
  });
});
