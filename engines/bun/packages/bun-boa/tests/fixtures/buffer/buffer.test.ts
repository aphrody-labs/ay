// Buffer and node:buffer on Boa: bun_runtime_js's encoders under js/buffer.ts, against what Bun on JavaScriptCore
// answers for the same calls.
import { Buffer as ModuleBuffer, SlowBuffer, isAscii, isUtf8, kMaxLength, transcode } from "node:buffer";
import { expect, test } from "bun:test";

const code = (fn: () => unknown) => {
  try {
    fn();
  } catch (error: any) {
    return `${error.name} ${error.code}: ${error.message}`;
  }
  return "no error";
};

test("the global is the module's, a Uint8Array", () => {
  expect(Buffer).toBe(ModuleBuffer);
  const buf = Buffer.from("hi");
  expect(buf instanceof Uint8Array).toBe(true);
  expect(Buffer.isBuffer(buf)).toBe(true);
  expect(Buffer.isBuffer(new Uint8Array(2))).toBe(false);
  expect(Object.prototype.toString.call(buf)).toBe("[object Uint8Array]");
  expect(kMaxLength).toBe(2 ** 32);
  expect(new SlowBuffer(3).length).toBe(3);
  expect(Buffer.poolSize).toBe(8192);
});

test("encodings", () => {
  expect(Buffer.from("héllo €").toString("hex")).toBe("68c3a96c6c6f20e282ac");
  expect(Buffer.from("aGVsbG8=", "base64").toString()).toBe("hello");
  expect(Buffer.from("hello").toString("base64url")).toBe("aGVsbG8");
  expect(Buffer.from("ab", "utf16le")).toEqual(Buffer.from([0x61, 0, 0x62, 0]));
  expect(Buffer.from("é", "latin1")[0]).toBe(0xe9);
  expect(Buffer.from([0xe9, 0x41]).toString("ascii")).toBe("iA");
  expect(Buffer.from([0xc3, 0x28]).toString()).toBe("�(");
  expect(Buffer.byteLength("€")).toBe(3);
  expect(Buffer.byteLength("aGVsbG8=", "base64")).toBe(5);
  expect(Buffer.isEncoding("UTF-8")).toBe(true);
  expect(Buffer.isEncoding("buffer")).toBe(false);
  expect(code(() => Buffer.from("x", "nope"))).toBe("TypeError ERR_UNKNOWN_ENCODING: Unknown encoding: nope");
});

test("alloc, fill, write", () => {
  expect(Buffer.alloc(5, "ab").toString()).toBe("ababa");
  expect(Buffer.alloc(4, 0x101)).toEqual(Buffer.from([1, 1, 1, 1]));
  expect(Buffer.alloc(3, "é").toString("hex")).toBe("c3a9c3");
  const buf = Buffer.alloc(6);
  expect(buf.write("€uro", 1)).toBe(5);
  expect(buf.toString("hex")).toBe("00e282ac7572");
  expect(buf.fill("z", 4).toString("latin1", 4)).toBe("zz");
  expect(code(() => Buffer.alloc(-1))).toBe(
    'RangeError ERR_OUT_OF_RANGE: The value of "size" is out of range. It must be >= 0 && <= 4294967296. Received -1',
  );
  expect(code(() => Buffer.alloc(2, "zz", "hex"))).toBe(
    "TypeError ERR_INVALID_ARG_VALUE: The argument 'value' is invalid. Received 'zz'",
  );
  expect(code(() => buf.write("x", 7))).toBe(
    'RangeError ERR_OUT_OF_RANGE: The value of "offset" is out of range. It must be >= 0 && <= 6. Received 7',
  );
});

test("compare, copy, concat, indexOf, slice", () => {
  const a = Buffer.from("abcabc");
  expect(a.indexOf("bc")).toBe(1);
  expect(a.lastIndexOf("bc")).toBe(4);
  expect(a.indexOf(0x63, 3)).toBe(5);
  expect(a.includes(Buffer.from("ca"))).toBe(true);
  expect(a.indexOf("b", -2)).toBe(4);
  expect(Buffer.compare(Buffer.from("a"), Buffer.from("b"))).toBe(-1);
  expect(a.equals(Buffer.from("abcabc"))).toBe(true);
  const target = Buffer.alloc(4);
  expect(a.copy(target, 1, 4)).toBe(2);
  expect(target.toString("latin1", 1, 3)).toBe("bc");
  expect(Buffer.concat([Buffer.from("ab"), new Uint8Array([0x63])]).toString()).toBe("abc");
  const slice = a.subarray(1, 3);
  expect(Buffer.isBuffer(slice) && slice.toString()).toBe("bc");
  slice[0] = 0x7a;
  expect(a.toString()).toBe("azcabc");
  expect(code(() => Buffer.concat([1 as any]))).toBe(
    'TypeError ERR_INVALID_ARG_TYPE: The "list[0]" argument must be an instance of Buffer or Uint8Array. Received type number (1)',
  );
});

test("read and write numbers", () => {
  const buf = Buffer.alloc(8);
  expect(buf.writeUInt32BE(0xdeadbeef)).toBe(4);
  expect(buf.readUInt32LE(0)).toBe(0xefbeadde);
  expect(buf.readInt16BE(0)).toBe(-8531);
  expect(buf.writeDoubleLE(1.5)).toBe(8);
  expect(buf.readDoubleLE()).toBe(1.5);
  buf.writeBigInt64BE(-2n);
  expect(buf.readBigInt64BE()).toBe(-2n);
  expect(buf.readBigUInt64LE()).toBe(0xfeffffffffffffffn);
  expect(buf.writeIntLE(-123456, 1, 3)).toBe(4);
  expect(buf.readIntLE(1, 3)).toBe(-123456);
  expect(buf.readUIntBE(0, 6)).toBe(0xffc01dfeffff);
  expect(buf.readUint8).toBe(buf.readUInt8);
  expect(code(() => buf.readUInt8(8))).toBe(
    'RangeError ERR_OUT_OF_RANGE: The value of "offset" is out of range. It must be >= 0 and <= 7. Received 8',
  );
  expect(code(() => buf.writeUInt8(256))).toBe(
    'RangeError ERR_OUT_OF_RANGE: The value of "value" is out of range. It must be >= 0 and <= 255. Received 256',
  );
  expect(code(() => buf.readInt32LE("1" as any))).toBe(
    `TypeError ERR_INVALID_ARG_TYPE: The "offset" argument must be of type number. Received type string ('1')`,
  );
  expect(code(() => Buffer.alloc(2).readUInt32LE())).toBe(
    "RangeError ERR_BUFFER_OUT_OF_BOUNDS: Attempt to access memory outside buffer bounds",
  );
});

test("toJSON, swap, module functions", () => {
  expect(JSON.stringify(Buffer.from([1, 2]))).toBe('{"type":"Buffer","data":[1,2]}');
  expect(Buffer.from([1, 2, 3, 4]).swap16()).toEqual(Buffer.from([2, 1, 4, 3]));
  expect(code(() => Buffer.from([1]).swap16())).toBe(
    "RangeError ERR_INVALID_BUFFER_SIZE: Buffer size must be a multiple of 16-bits",
  );
  const buf = Buffer.from("ab");
  expect(buf.parent).toBe(buf.buffer);
  expect(buf.offset).toBe(buf.byteOffset);
  expect(buf.inspect()).toBe("<Buffer 61 62>");
  expect(isUtf8(Buffer.from("é"))).toBe(true);
  expect(isAscii(Buffer.from("é"))).toBe(false);
  expect(transcode(Buffer.from("€"), "utf8", "ucs2")).toEqual(Buffer.from([0xac, 0x20]));
  expect(code(() => transcode(Buffer.from([0xff]), "utf8", "ucs2"))).toBe(
    "Error U_INVALID_CHAR_FOUND: Unable to transcode Buffer [U_INVALID_CHAR_FOUND]",
  );
});

test("TextEncoder and TextDecoder", () => {
  const encoder = new TextEncoder();
  expect(encoder.encoding).toBe("utf-8");
  expect([...encoder.encode("é\ud800")]).toEqual([0xc3, 0xa9, 0xef, 0xbf, 0xbd]);
  const into = new Uint8Array(3);
  expect(encoder.encodeInto("a€", into)).toEqual({ read: 1, written: 1 });
  expect(new TextDecoder().decode(new Uint8Array([0xef, 0xbb, 0xbf, 0x68, 0xc3]))).toBe("h�");
  const streaming = new TextDecoder("utf-16le");
  expect(streaming.decode(new Uint8Array([0xac]), { stream: true })).toBe("");
  expect(streaming.decode(new Uint8Array([0x20]))).toBe("€");
  expect(new TextDecoder("shift_jis").decode(new Uint8Array([0x82, 0xa0]))).toBe("あ");
  expect(new TextDecoder("latin1").encoding).toBe("windows-1252");
  expect(code(() => new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array([0xff])))).toBe(
    "TypeError ERR_ENCODING_INVALID_ENCODED_DATA: The encoded data was not valid for encoding utf-8",
  );
  expect(code(() => new TextDecoder("nope"))).toBe(
    'RangeError ERR_ENCODING_NOT_SUPPORTED: Unsupported encoding label "nope"',
  );
  expect(code(() => new TextDecoder("utf-8", 1 as any))).toBe(
    'TypeError ERR_INVALID_ARG_TYPE: The "options" argument must be of type object. Received type number (1)',
  );
});

test("node:string_decoder on Buffer", () => {
  const { StringDecoder } = require("node:string_decoder");
  const utf8 = new StringDecoder();
  expect(utf8.write(Buffer.from([0xe2, 0x82]))).toBe("");
  expect(Buffer.isBuffer(utf8.lastChar)).toBe(true);
  expect(utf8.write(Buffer.from([0xac]))).toBe("€");
  expect(StringDecoder("hex")).toBeInstanceOf(StringDecoder);
  const receiver: any = {};
  expect(StringDecoder.call(receiver, "latin1")).toBe(receiver);
  expect(receiver.encoding).toBe("latin1");
});

test("host functions are not constructors, and a SharedArrayBuffer is an ArrayBuffer", () => {
  expect(() => new (Buffer.concat as any)([])).toThrow(TypeError);
  expect(new (Buffer.alloc as any)(2)).toEqual(Buffer.alloc(2));
  expect(() => new (Buffer.prototype.toString as any)()).toThrow(TypeError);
  expect(() => new (isAscii as any)(Buffer.from("a"))).toThrow(TypeError);
  expect(Buffer.prototype.toLocaleString).toBe(Buffer.prototype.toString);
  expect(Object.keys(Buffer.prototype)).toContain("readUint8");
  expect(Buffer.from(new SharedArrayBuffer(4), 1).length).toBe(3);
  expect(Buffer.from(new BigInt64Array(0)).length).toBe(0);
  expect(() => Buffer.from(new BigInt64Array(1))).toThrow(TypeError);
  const readUInt8 = Buffer.prototype.readUInt8;
  expect(code(() => readUInt8(0))).toBe(
    'TypeError ERR_INVALID_ARG_TYPE: The "buf" argument must be of type Buffer. Received undefined',
  );
});
