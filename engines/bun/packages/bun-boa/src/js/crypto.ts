// Crypto on Boa, over the natives of bun_runtime_js's crypto module (RustCrypto, getrandom), which runtime.rs puts
// on globalThis[OPS].crypto: WebCore's Crypto and SubtleCrypto (digest, HMAC and AES-GCM keys), and of Bun,
// src/runtime/crypto/CryptoHasher.bind.ts (CryptoHasher, SHA1…, Bun.sha), src/runtime/api/HashObject.bind.ts
// (Bun.hash) and src/jsc/uuid.rs (Bun.randomUUIDv7, Bun.randomUUIDv5). A script: it runs before js/bun.ts, which
// puts OPS.cryptoBun on Bun.
(function install(ops: any) {
  const G = globalThis as any;
  const n = ops.crypto as {
    randomBytes(length: number): Uint8Array;
    uuidV4(): string;
    uuidV7(timestamp?: number): Uint8Array;
    uuidV5(name: Uint8Array, namespace: string | Uint8Array): Uint8Array | null;
    uuidString(bytes: Uint8Array): string;
    hash(kind: string, data: Uint8Array, seed: string): number | bigint;
    algorithms(): string[];
    canonicalName(name: string): string | null;
    hasherCreate(name: string, key?: Uint8Array, outputLength?: number): object | null | false;
    hasherUpdate(handle: object, bytes: Uint8Array): boolean;
    hasherDigest(handle: object, xofLength?: number): Uint8Array | null;
    hasherCopy(handle: object): object | null;
    hasherInfo(handle: object): [string, number, boolean] | null;
    digest(name: string, bytes: Uint8Array, outputLength?: number): Uint8Array | null;
    hmac(name: string, key: Uint8Array, bytes: Uint8Array): Uint8Array | null;
    aesGcm(encrypt: boolean, key: Uint8Array, iv: Uint8Array, data: Uint8Array, aad: Uint8Array, tag: number): Uint8Array | null;
    isXof(name: string): boolean;
  };
  const withCode = <E extends Error>(error: E, code: string): E => Object.assign(error, { code });
  const invalidArgs = (message: string) => withCode(new TypeError(message), "ERR_INVALID_ARG_TYPE");
  const invalidValue = (message: string) => withCode(new TypeError(message), "ERR_INVALID_ARG_VALUE");
  const domException = (message: string, name: string) => {
    if (typeof G.DOMException === "function") return new G.DOMException(message, name);
    const error = new Error(message);
    error.name = name;
    return error;
  };
  const isView = ArrayBuffer.isView;
  const isArrayBuffer = (v: unknown): v is ArrayBufferLike =>
    v instanceof ArrayBuffer || (typeof SharedArrayBuffer === "function" && v instanceof SharedArrayBuffer);
  const viewBytes = (v: ArrayBufferView) => new Uint8Array(v.buffer, v.byteOffset, v.byteLength);

  // ── bytes and encodings (node's Encoding) ──────────────────────────────────
  type Encoding = "utf8" | "hex" | "base64" | "base64url" | "latin1" | "ascii" | "utf16le" | "buffer";
  const encodingOf = (name: string): Encoding | undefined => {
    switch (name.toLowerCase()) {
      case "utf8":
      case "utf-8":
        return "utf8";
      case "hex":
        return "hex";
      case "base64":
        return "base64";
      case "base64url":
        return "base64url";
      case "latin1":
      case "binary":
        return "latin1";
      case "ascii":
        return "ascii";
      case "ucs2":
      case "ucs-2":
      case "utf16le":
      case "utf-16le":
        return "utf16le";
      case "buffer":
        return "buffer";
    }
  };
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  function stringBytes(text: string, encoding: Encoding): Uint8Array {
    switch (encoding) {
      case "hex": {
        const out = new Uint8Array(text.length >>> 1);
        let i = 0;
        for (; i < out.length; i++) {
          const byte = parseInt(text.slice(2 * i, 2 * i + 2), 16);
          if (!/^[0-9a-fA-F]{2}$/.test(text.slice(2 * i, 2 * i + 2))) break;
          out[i] = byte;
        }
        return out.subarray(0, i);
      }
      case "base64":
      case "base64url": {
        let clean = text.replace(/[^A-Za-z0-9+/\-_]/g, "").replace(/-/g, "+").replace(/_/g, "/");
        if (clean.length % 4 === 1) clean = clean.slice(0, -1);
        const binary = atob(clean.padEnd(Math.ceil(clean.length / 4) * 4, "="));
        return Uint8Array.from(binary, c => c.charCodeAt(0));
      }
      case "latin1":
      case "ascii":
        return Uint8Array.from(text, c => c.charCodeAt(0) & 0xff);
      case "utf16le": {
        const out = new Uint8Array(text.length * 2);
        for (let i = 0; i < text.length; i++) {
          const unit = text.charCodeAt(i);
          out[2 * i] = unit & 0xff;
          out[2 * i + 1] = unit >> 8;
        }
        return out;
      }
      default:
        return encoder.encode(text);
    }
  }
  function encode(bytes: Uint8Array, encoding: Encoding): string | Uint8Array {
    switch (encoding) {
      case "hex":
        return Array.from(bytes, b => b.toString(16).padStart(2, "0")).join("");
      case "base64":
        return btoa(String.fromCharCode(...bytes));
      case "base64url":
        return btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
      case "latin1":
        return String.fromCharCode(...bytes);
      case "ascii":
        return String.fromCharCode(...Array.from(bytes, b => b & 0x7f));
      case "utf16le": {
        let out = "";
        for (let i = 0; i + 1 < bytes.length; i += 2) out += String.fromCharCode(bytes[i] | (bytes[i + 1] << 8));
        return out;
      }
      case "buffer":
        return makeBuffer(bytes);
      default:
        return decoder.decode(bytes);
    }
  }
  const makeBuffer = (bytes: Uint8Array): Uint8Array =>
    typeof G.Buffer === "function" ? G.Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength) : bytes;
  /** `b.bytes({ from: ["string", "buffer", "blob"] })`. A Blob's bytes are not read synchronously on Boa yet. */
  function bytesOf(input: unknown, what = "input"): Uint8Array {
    if (typeof input === "string") return encoder.encode(input);
    if (isView(input)) return viewBytes(input);
    if (isArrayBuffer(input)) return new Uint8Array(input);
    if (typeof G.Blob === "function" && input instanceof G.Blob)
      throw invalidArgs(typeof (input as any).name === "string" ? "File blob cannot be used here" : "Blob cannot be used here on Boa yet");
    throw invalidArgs(`${what} must be a string, ArrayBuffer, ArrayBufferView or Blob`);
  }

  /** `DigestOutput`: an encoding's string, the TypedArray it is written into, or a new buffer. */
  function finish(digest: Uint8Array, output: unknown, newBuffer: (b: Uint8Array) => Uint8Array, atLeast = digest.length) {
    if (output === undefined || output === null) return newBuffer(digest);
    if (isView(output)) {
      if (output.byteLength < atLeast) throw invalidArgs(`TypedArray must be at least ${atLeast} bytes`);
      viewBytes(output).set(digest);
      return output;
    }
    if (typeof output === "string") {
      const encoding = encodingOf(output);
      if (!encoding) throw invalidValue(`Unknown encoding: ${output}`);
      return encode(digest, encoding);
    }
    throw invalidArgs("digest output must be a string or a TypedArray");
  }

  // ── Bun.CryptoHasher ──────────────────────────────────────────────────────
  const ONE_OF =
    "'blake2b256', 'blake2b512', 'blake2s256', 'md4', 'md5', 'ripemd160', 'sha1', 'sha224', 'sha256', 'sha384', " +
    "'sha512', 'sha512-224', 'sha512-256', 'sha3-224', 'sha3-256', 'sha3-384', 'sha3-512', 'shake128' or 'shake256'";
  const consumed = () => new Error("HMAC has been consumed and is no longer usable");
  const HANDLE = Symbol("handle");

  function updateWith(handle: object, input: unknown, encoding: unknown) {
    let bytes: Uint8Array;
    if (typeof input === "string" || input instanceof String) {
      const text = String(input);
      const name = encoding === undefined ? "utf8" : String(encoding);
      const resolved = encodingOf(name) ?? "utf8";
      if (resolved === "hex" && text.length % 2 !== 0)
        throw invalidValue(`The argument 'encoding' is invalid for data of length ${text.length}. Received '${name}'`);
      bytes = stringBytes(text, resolved);
    } else bytes = bytesOf(input);
    if (!n.hasherUpdate(handle, bytes)) throw consumed();
  }

  class CryptoHasher {
    [HANDLE]!: object;
    constructor(algorithm: unknown, hmacKey?: unknown) {
      if (algorithm === HANDLE) {
        this[HANDLE] = hmacKey as object;
        return;
      }
      if (typeof algorithm !== "string") throw invalidArgs(`The "algorithm" argument must be of type string`);
      if (algorithm === "") throw invalidArgs("Invalid algorithm name");
      if (hmacKey !== undefined && hmacKey !== null) {
        const key = typeof hmacKey === "string" ? encoder.encode(hmacKey) : bytesOf(hmacKey, "hmacKey");
        const handle = n.hasherCreate(algorithm, key);
        if (handle === null) throw invalidArgs(`algorithm must be one of ${ONE_OF}`);
        if (handle === false) {
          const error = new Error("HMAC is not supported for this algorithm yet");
          throw withCode(error, "ERR_NOT_IMPLEMENTED");
        }
        this[HANDLE] = handle;
        return;
      }
      const handle = n.hasherCreate(algorithm);
      if (!handle) throw invalidArgs(`Unsupported algorithm ${algorithm}`);
      this[HANDLE] = handle;
    }
    #info() {
      const info = n.hasherInfo(this[HANDLE]);
      if (!info) throw consumed();
      return info;
    }
    get algorithm() {
      return this.#info()[0];
    }
    get byteLength() {
      return this.#info()[1];
    }
    copy() {
      const handle = n.hasherCopy(this[HANDLE]);
      if (!handle) throw consumed();
      return new CryptoHasher(HANDLE, handle);
    }
    update(input: unknown, encoding?: unknown) {
      updateWith(this[HANDLE], input, encoding);
      return this;
    }
    digest(output?: unknown) {
      const info = this.#info();
      const xof = !info[2] && n.isXof(info[0]) && isView(output) ? output.byteLength : undefined;
      if (isView(output) && xof === undefined && output.byteLength < info[1])
        throw invalidArgs(`TypedArray must be at least ${info[1]} bytes`);
      const digest = n.hasherDigest(this[HANDLE], xof);
      if (!digest) throw consumed();
      return finish(digest, output, makeBuffer);
    }
    static get algorithms() {
      return n.algorithms();
    }
    static hash(algorithm: unknown, input: unknown, output?: unknown) {
      const name = String(algorithm);
      const bytes = bytesOf(input);
      const digest = n.digest(name, bytes);
      if (!digest) throw invalidArgs(`Unsupported algorithm "${name}"`);
      return finish(digest, output, makeBuffer);
    }
  }
  Object.defineProperty(CryptoHasher.prototype, Symbol.toStringTag, { value: "CryptoHasher", configurable: true });

  const staticHasher = (className: string, algorithm: string) => {
    const length = n.hasherInfo(n.hasherCreate(algorithm) as object)![1];
    const Hasher = class {
      [HANDLE]: object;
      constructor() {
        this[HANDLE] = n.hasherCreate(algorithm) as object;
      }
      get byteLength() {
        return length;
      }
      #digested = false;
      update(input: unknown) {
        if (this.#digested) throw withCode(new Error(`${className} hasher already digested, create a new instance to update`), "ERR_INVALID_STATE");
        n.hasherUpdate(this[HANDLE], bytesOf(input));
        return this;
      }
      digest(output?: unknown) {
        if (this.#digested) throw withCode(new Error(`${className} hasher already digested, create a new instance to digest again`), "ERR_INVALID_STATE");
        if (isView(output) && output.byteLength < length) throw invalidArgs(`TypedArray must be at least ${length} bytes`);
        this.#digested = true;
        return finish(n.hasherDigest(this[HANDLE])!, output, b => b);
      }
      static get byteLength() {
        return length;
      }
      static hash(input: unknown, output?: unknown) {
        return finish(n.digest(algorithm, bytesOf(input))!, output, b => b);
      }
    };
    Object.defineProperty(Hasher, "name", { value: className });
    return Hasher;
  };
  const hashers = {
    MD4: staticHasher("MD4", "md4"),
    MD5: staticHasher("MD5", "md5"),
    SHA1: staticHasher("SHA1", "sha1"),
    SHA224: staticHasher("SHA224", "sha224"),
    SHA256: staticHasher("SHA256", "sha256"),
    SHA384: staticHasher("SHA384", "sha384"),
    SHA512: staticHasher("SHA512", "sha512"),
    SHA512_256: staticHasher("SHA512_256", "sha512-256"),
  };
  const sha = function sha(input: unknown, output?: unknown) {
    return hashers.SHA512_256.hash(input, output);
  };

  // ── Bun.hash ──────────────────────────────────────────────────────────────
  /** `toUInt64NoTruncate` of a number or a BigInt, as decimal digits; anything else is 0. */
  const seedOf = (seed: unknown): string => {
    if (typeof seed === "bigint") return BigInt.asUintN(64, seed).toString();
    if (typeof seed !== "number" || Number.isNaN(seed)) return "0";
    if (seed >= 18446744073709551616) return "18446744073709551615";
    if (!Number.isFinite(seed)) return "0";
    return BigInt.asUintN(64, BigInt(Math.trunc(seed))).toString();
  };
  const hashData = (data: unknown): Uint8Array => {
    if (data === undefined || data === null) throw invalidArgs("Expected a string, Buffer or Blob");
    if (typeof data === "string" || isView(data) || isArrayBuffer(data) || (typeof G.Blob === "function" && data instanceof G.Blob))
      return bytesOf(data);
    return encoder.encode(String(data));
  };
  const seeded = (kind: string) =>
    ({
      [kind](data: unknown, seed?: unknown) {
        return n.hash(kind, hashData(data), seedOf(seed));
      },
    })[kind];
  const kinds = ["wyhash", "adler32", "crc32", "cityHash32", "cityHash64", "xxHash32", "xxHash64", "xxHash3"];
  const hash = Object.assign(
    function hash(data: unknown, seed?: unknown) {
      return n.hash("wyhash", hashData(data), seedOf(seed));
    },
    Object.fromEntries([...kinds, "murmur32v2", "murmur32v3", "murmur64v2", "rapidhash"].map(k => [k, seeded(k)])),
  );

  // ── Bun.randomUUIDv7, Bun.randomUUIDv5 ────────────────────────────────────
  const uuidOutput = (bytes: Uint8Array, encoding: unknown) => {
    if (encoding === undefined || encoding === "hex") return n.uuidString(bytes);
    const resolved = typeof encoding === "string" ? encodingOf(encoding) : undefined;
    if (resolved === "buffer") return makeBuffer(bytes);
    if (resolved === "base64" || resolved === "base64url") return encode(bytes, resolved);
    throw withCode(new TypeError("Encoding must be one of base64, base64url, hex, or buffer"), "ERR_UNKNOWN_ENCODING");
  };
  function randomUUIDv7(encoding?: unknown, timestamp?: unknown) {
    if (typeof encoding === "number" || encoding instanceof Date) [encoding, timestamp] = [undefined, encoding];
    let ms: number | undefined;
    if (timestamp instanceof Date) ms = timestamp.getTime();
    else if (typeof timestamp === "number") ms = timestamp;
    else if (timestamp !== undefined) throw invalidArgs("timestamp must be a number or a Date");
    if (ms !== undefined && (!Number.isFinite(ms) || ms < 0)) throw withCode(new RangeError("timestamp must be a finite, non-negative number"), "ERR_OUT_OF_RANGE");
    return uuidOutput(n.uuidV7(ms === undefined ? undefined : Math.min(Math.trunc(ms), 2 ** 48 - 1)), encoding);
  }
  function randomUUIDv5(name: unknown, namespace: unknown, encoding?: unknown) {
    const nameBytes = typeof name === "string" ? encoder.encode(name) : bytesOf(name, "name");
    let ns: string | Uint8Array;
    if (typeof namespace === "string") ns = namespace;
    else if (isView(namespace) || isArrayBuffer(namespace)) ns = bytesOf(namespace, "namespace");
    else throw invalidArgs("namespace must be a string or a buffer");
    if (typeof ns !== "string" && ns.byteLength !== 16) throw invalidValue("Namespace must be exactly 16 bytes");
    const bytes = n.uuidV5(nameBytes, ns);
    if (!bytes) throw invalidValue("Invalid UUID format for namespace");
    return uuidOutput(bytes, encoding);
  }

  // ── Crypto and SubtleCrypto ───────────────────────────────────────────────
  const ILLEGAL = Symbol("illegal");
  const illegal = () => new TypeError("Illegal constructor");
  const toStringTag = (target: object, tag: string) =>
    Object.defineProperty(target, Symbol.toStringTag, { value: tag, configurable: true });

  type KeyData = { type: "secret"; algorithm: any; extractable: boolean; usages: string[]; bytes: Uint8Array };
  const KEY = Symbol("key");
  class CryptoKey {
    [KEY]!: KeyData;
    constructor(token?: symbol) {
      if (token !== ILLEGAL) throw illegal();
    }
    get type() {
      return this[KEY].type;
    }
    get extractable() {
      return this[KEY].extractable;
    }
    get algorithm() {
      return this[KEY].algorithm;
    }
    get usages() {
      return [...this[KEY].usages];
    }
  }
  toStringTag(CryptoKey.prototype, "CryptoKey");
  const makeKey = (data: KeyData) => {
    const key = new CryptoKey(ILLEGAL);
    key[KEY] = data;
    return key;
  };

  const DIGESTS: Record<string, string> = { "SHA-1": "sha1", "SHA-256": "sha256", "SHA-384": "sha384", "SHA-512": "sha512" };
  const normalize = (algorithm: unknown): { name: string; [k: string]: any } => {
    if (typeof algorithm === "string") return { name: algorithm };
    if (typeof algorithm === "object" && algorithm !== null && "name" in algorithm) return { ...(algorithm as any), name: String((algorithm as any).name) };
    throw new TypeError("Algorithm must be a string or an object with a name");
  };
  const upper = (name: string) => name.toUpperCase();
  const hashName = (hash: unknown) => {
    const name = upper(normalize(hash).name);
    if (!(name in DIGESTS)) throw domException("Unrecognized hash algorithm", "NotSupportedError");
    return name;
  };
  const bufferSource = (data: unknown): Uint8Array => {
    if (isView(data)) return viewBytes(data).slice();
    if (isArrayBuffer(data)) return new Uint8Array(data).slice();
    throw new TypeError("Argument must be an ArrayBuffer or an ArrayBufferView");
  };
  const toArrayBuffer = (bytes: Uint8Array) => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
  /** A promise of what `run` returns or throws. */
  const later = <T>(run: () => T): Promise<T> => new Promise(resolve => resolve(run()));
  const usable = (key: unknown, usage: string, name: string) => {
    if (!(key instanceof CryptoKey)) throw new TypeError("Argument 2 must be a CryptoKey");
    const data = key[KEY];
    if (upper(data.algorithm.name) !== upper(name)) throw domException("CryptoKey doesn't match AlgorithmIdentifier", "InvalidAccessError");
    if (!data.usages.includes(usage)) throw domException("CryptoKey doesn't support " + usage, "InvalidAccessError");
    return data;
  };
  const SECRET = new Set(["HMAC", "AES-GCM"]);
  const secretKey = (algorithm: { name: string; [k: string]: any }, bytes: Uint8Array, extractable: boolean, usages: string[]) => {
    const name = upper(algorithm.name);
    if (name === "HMAC") {
      const hash = hashName(algorithm.hash);
      return makeKey({ type: "secret", algorithm: { name: "HMAC", hash: { name: hash }, length: bytes.length * 8 }, extractable, usages, bytes });
    }
    if (![16, 24, 32].includes(bytes.length)) throw domException("AES key data must be 128, 192 or 256 bits", "DataError");
    return makeKey({ type: "secret", algorithm: { name: "AES-GCM", length: bytes.length * 8 }, extractable, usages, bytes });
  };

  class SubtleCrypto {
    constructor(token?: symbol) {
      if (token !== ILLEGAL) throw illegal();
    }
    digest(algorithm: unknown, data: unknown): Promise<ArrayBufferLike> {
      return later(() => {
        const name = upper(normalize(algorithm).name);
        const bytes = bufferSource(data);
        const native = DIGESTS[name] ?? (name.startsWith("SHA3-") ? name.toLowerCase() : undefined);
        const digest = native && n.digest(native, bytes);
        if (!digest) throw domException("Unrecognized algorithm name", "NotSupportedError");
        return toArrayBuffer(digest);
      });
    }
    importKey(format: string, keyData: unknown, algorithm: unknown, extractable: boolean, usages: string[]) {
      return later(() => {
        const normalized = normalize(algorithm);
        if (!SECRET.has(upper(normalized.name))) throw domException("Unsupported algorithm on Boa", "NotSupportedError");
        let bytes: Uint8Array;
        if (format === "raw") bytes = bufferSource(keyData);
        else if (format === "jwk" && typeof keyData === "object" && keyData && (keyData as any).kty === "oct")
          bytes = stringBytes(String((keyData as any).k), "base64url");
        else throw domException(`The format '${format}' is not supported for this key`, "NotSupportedError");
        return secretKey(normalized, bytes, !!extractable, [...usages]);
      });
    }
    generateKey(algorithm: unknown, extractable: boolean, usages: string[]) {
      return later(() => {
        const normalized = normalize(algorithm);
        const name = upper(normalized.name);
        if (!SECRET.has(name)) throw domException("Unsupported algorithm on Boa", "NotSupportedError");
        const bits = name === "HMAC" ? (normalized.length ?? (hashName(normalized.hash) === "SHA-384" || hashName(normalized.hash) === "SHA-512" ? 1024 : 512)) : normalized.length;
        if (name === "AES-GCM" && ![128, 192, 256].includes(bits)) throw domException("AES key length must be 128, 192 or 256 bits", "OperationError");
        return secretKey(normalized, n.randomBytes(bits / 8), !!extractable, [...usages]);
      });
    }
    exportKey(format: string, key: unknown) {
      return later(() => {
        if (!(key instanceof CryptoKey)) throw new TypeError("Argument 2 must be a CryptoKey");
        const data = key[KEY];
        if (!data.extractable) throw domException("The CryptoKey is nonextractable", "InvalidAccessError");
        if (format === "raw") return toArrayBuffer(data.bytes.slice());
        if (format === "jwk") {
          const alg = data.algorithm.name === "HMAC" ? "HS" + data.algorithm.hash.name.slice(4) : `A${data.algorithm.length}GCM`;
          return { alg, ext: data.extractable, k: encode(data.bytes, "base64url"), key_ops: [...data.usages], kty: "oct" };
        }
        throw domException(`The format '${format}' is not supported for this key`, "NotSupportedError");
      });
    }
    sign(algorithm: unknown, key: unknown, data: unknown) {
      return later(() => {
        const name = normalize(algorithm).name;
        const secret = usable(key, "sign", name);
        if (upper(name) !== "HMAC") throw domException("Unsupported algorithm on Boa", "NotSupportedError");
        return toArrayBuffer(n.hmac(DIGESTS[secret.algorithm.hash.name], secret.bytes, bufferSource(data))!);
      });
    }
    verify(algorithm: unknown, key: unknown, signature: unknown, data: unknown) {
      return later(() => {
        const name = normalize(algorithm).name;
        const secret = usable(key, "verify", name);
        if (upper(name) !== "HMAC") throw domException("Unsupported algorithm on Boa", "NotSupportedError");
        const mac = n.hmac(DIGESTS[secret.algorithm.hash.name], secret.bytes, bufferSource(data))!;
        return (n as any).timingSafeEqual(mac, bufferSource(signature));
      });
    }
    encrypt(algorithm: unknown, key: unknown, data: unknown) {
      return this.#aes(true, algorithm, key, data);
    }
    decrypt(algorithm: unknown, key: unknown, data: unknown) {
      return this.#aes(false, algorithm, key, data);
    }
    #aes(encrypt: boolean, algorithm: unknown, key: unknown, data: unknown) {
      return later(() => {
        const params = normalize(algorithm);
        const secret = usable(key, encrypt ? "encrypt" : "decrypt", params.name);
        const iv = bufferSource(params.iv);
        const aad = params.additionalData === undefined ? new Uint8Array(0) : bufferSource(params.additionalData);
        const out = n.aesGcm(encrypt, secret.bytes, iv, bufferSource(data), aad, (params.tagLength ?? 128) / 8);
        if (!out) throw domException("The operation failed for an operation-specific reason", "OperationError");
        return toArrayBuffer(out);
      });
    }
  }
  toStringTag(SubtleCrypto.prototype, "SubtleCrypto");

  const subtle = new SubtleCrypto(ILLEGAL);
  class Crypto {
    constructor(token?: symbol) {
      if (token !== ILLEGAL) throw illegal();
    }
    get subtle() {
      return subtle;
    }
    getRandomValues<T extends ArrayBufferView | null>(array: T): T {
      if (!isView(array))
        throw new TypeError("Failed to execute 'getRandomValues' on 'Crypto': 1st argument is not an ArrayBufferView.");
      if (array instanceof DataView || array instanceof Float32Array || array instanceof Float64Array || array.constructor?.name === "Float16Array")
        throw domException("The data argument must be an integer-type TypedArray", "TypeMismatchError");
      viewBytes(array).set(n.randomBytes(array.byteLength));
      return array;
    }
    randomUUID() {
      return n.uuidV4();
    }
  }
  toStringTag(Crypto.prototype, "Crypto");
  const crypto = new Crypto(ILLEGAL);

  Object.defineProperties(G, {
    crypto: { value: crypto, writable: true, configurable: true },
    Crypto: { value: Crypto, writable: true, configurable: true },
    SubtleCrypto: { value: SubtleCrypto, writable: true, configurable: true },
    CryptoKey: { value: CryptoKey, writable: true, configurable: true },
  });

  const cryptoBun = { CryptoHasher, sha, hash, randomUUIDv7, randomUUIDv5, ...hashers };
  ops.cryptoBun = cryptoBun;
  ops.cryptoNode = { bytesOf, stringBytes, encode, encodingOf, makeBuffer, finish, natives: n };
})((globalThis as any).__bun_native_ops__);
