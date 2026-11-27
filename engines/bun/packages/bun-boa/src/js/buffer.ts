// `Buffer` and `node:buffer` on Boa: what src/jsc/bindings/JSBuffer.cpp and src/jsc/modules/NodeBufferModule.h make
// for JavaScriptCore, with their argument handling and errors, on the byte operations of bun_runtime_js
// (src/runtime_js/node/buffer.rs). `Buffer.from`, `Buffer.isBuffer`, `toJSON`, `parent` and `offset` are Bun's own
// builtin functions (src/js/builtins/JSBuffer*.ts). Read and write of numbers follow Node's lib/internal/buffer.js,
// which the C++ follows too.
//
// A script: it puts `installBuffer` on the native operations, which js/intrinsics.ts calls once its intrinsics exist.
(function (ops: { installBuffer?: (env: Env) => object }) {
  interface Natives {
    parseEncoding(name: string): number;
    byteLength(text: string, encoding: number): number;
    encode(text: string, encoding: number): Uint8Array;
    write(view: ArrayBufferView, text: string, offset: number, length: number, encoding: number): number;
    decode(view: ArrayBufferView, start: number, end: number, encoding: number): string;
    fill(view: ArrayBufferView, start: number, end: number, value: unknown, encoding: number): boolean;
    indexOf(
      view: ArrayBufferView,
      needle: unknown,
      byteOffset: number,
      end: number,
      last: boolean,
      enc: number,
    ): number;
    compare(a: ArrayBufferView, aStart: number, aEnd: number, b: ArrayBufferView, bStart: number, bEnd: number): number;
    copy(source: ArrayBufferView, sourceStart: number, target: ArrayBufferView, targetStart: number, n: number): number;
    swap(view: ArrayBufferView, size: number): void;
    isUtf8(view: ArrayBufferView | ArrayBufferLike): boolean;
    isAscii(view: ArrayBufferView | ArrayBufferLike): boolean;
    transcode(view: ArrayBufferView, from: string, to: string): Uint8Array | string;
  }
  interface Env {
    natives: Natives;
    /** `$ERR_<code>(…args)`: the message ErrorCode.cpp formats for the code. */
    makeError(code: string, ...args: unknown[]): Error;
    /** An error of the code with this message. */
    error(code: string, message: string): Error;
    /** determineSpecificType of ErrorCode.cpp. */
    specificType(value: unknown): string;
    brand(value: unknown): string | undefined;
    /** Makes `$Buffer` and `globalThis.Buffer` this one. */
    provide(Buffer: Function): void;
    builtinFunction(file: string, name: string): Function;
  }

  ops.installBuffer = function installBuffer(env: Env) {
    "use strict";
    const { natives, makeError, error, specificType, brand } = env;
    const G = globalThis as any;
    const { defineProperty, getOwnPropertyDescriptor, getPrototypeOf, setPrototypeOf } = Object;
    const ReflectApply = Reflect.apply;
    const ReflectConstruct = Reflect.construct;
    const MathTrunc = Math.trunc;
    const MathFloor = Math.floor;
    const MathMin = Math.min;
    const NumberIsNaN = Number.isNaN;
    const NumberIsFinite = Number.isFinite;
    const StringCtor = String;

    // Bun::Buffer: MAX_ARRAY_BUFFER_SIZE and WTF::String::MaxLength.
    const kMaxLength = 2 ** 32;
    const kStringMaxLength = 2 ** 31 - 1;

    // BufferEncodingType, as bun_core::NodeEncoding numbers them.
    const UTF8 = 0;
    const UTF16LE = 2;
    const LATIN1 = 3;
    const ASCII = 4;
    const BASE64 = 5;
    const BASE64URL = 6;
    const HEX = 7;

    // ───────────────────────── views ─────────────────────────
    const TypedArrayPrototype = getPrototypeOf(Uint8Array.prototype);
    const getter = (object: object, key: PropertyKey) => getOwnPropertyDescriptor(object, key)!.get!;
    const tagGetter = getter(TypedArrayPrototype, Symbol.toStringTag);
    const bufferGetter = getter(TypedArrayPrototype, "buffer");
    const byteOffsetGetter = getter(TypedArrayPrototype, "byteOffset");
    const byteLengthGetter = getter(TypedArrayPrototype, "byteLength");
    const lengthGetter = getter(TypedArrayPrototype, "length");
    const dataViewByteLength = getter(DataView.prototype, "byteLength");
    const dataViewBuffer = getter(DataView.prototype, "buffer");
    const dataViewByteOffset = getter(DataView.prototype, "byteOffset");
    const arrayBufferByteLength = getter(ArrayBuffer.prototype, "byteLength");
    const detachedGetter = getOwnPropertyDescriptor(ArrayBuffer.prototype, "detached")?.get;
    const sharedByteLength = getter(SharedArrayBuffer.prototype, "byteLength");
    const TypedArrayFill = TypedArrayPrototype.fill;
    const isView = ArrayBuffer.isView;

    /** The kind of a typed array ("Uint8Array"), `undefined` for anything else. */
    const tagOf = (value: unknown): string | undefined => ReflectApply(tagGetter, value, []);
    /** JSUint8Array: a Buffer is one, a Uint8ClampedArray is not. */
    const isUint8Array = (value: unknown): value is Uint8Array => tagOf(value) === "Uint8Array";
    const isDataView = (value: unknown) => isView(value) && tagOf(value) === undefined;
    const isArrayBuffer = (value: unknown) => brand(value) === "ArrayBuffer";
    const isSharedArrayBuffer = (value: unknown) => brand(value) === "SharedArrayBuffer";
    const isDetachedBuffer = (buffer: unknown) =>
      isArrayBuffer(buffer) && detachedGetter !== undefined && ReflectApply(detachedGetter, buffer, []) === true;
    const viewBuffer = (view: ArrayBufferView): ArrayBufferLike =>
      ReflectApply(tagOf(view) === undefined ? dataViewBuffer : bufferGetter, view, []);
    const isDetached = (view: ArrayBufferView) => isDetachedBuffer(viewBuffer(view));
    /** byteLength(): 0 for a view of a detached buffer. */
    function byteLengthOf(view: ArrayBufferView): number {
      if (tagOf(view) !== undefined) return ReflectApply(byteLengthGetter, view, []);
      try {
        return ReflectApply(dataViewByteLength, view, []);
      } catch {
        return 0;
      }
    }
    /** length(): of a typed array in elements, of a DataView in bytes. */
    const lengthOf = (view: ArrayBufferView): number =>
      tagOf(view) !== undefined ? ReflectApply(lengthGetter, view, []) : byteLengthOf(view);
    /** The bytes of a view, as a Uint8Array. */
    function bytesOf(view: ArrayBufferView): Uint8Array {
      if (isUint8Array(view)) return view;
      const typed = tagOf(view) !== undefined;
      const byteOffset: number = ReflectApply(typed ? byteOffsetGetter : dataViewByteOffset, view, []);
      return new Uint8Array(viewBuffer(view), byteOffset, byteLengthOf(view));
    }

    // ───────────────────────── errors ─────────────────────────
    const invalidArgType = (name: string, expected: unknown, value: unknown) =>
      makeError("ERR_INVALID_ARG_TYPE", name, expected, value);
    const invalidArgInstance = (name: string, expected: string[], value: unknown) =>
      makeError("ERR_INVALID_ARG_TYPE", name, expected, value);
    const outOfRange = (name: string, range: string, value: unknown) =>
      makeError("ERR_OUT_OF_RANGE", name, range, value);
    const outOfBounds = (name?: string) =>
      error(
        "ERR_BUFFER_OUT_OF_BOUNDS",
        name ? `"${name}" is outside of buffer bounds` : "Attempt to access memory outside buffer bounds",
      );
    const unknownEncoding = (encoding: unknown) => makeError("ERR_UNKNOWN_ENCODING", encoding);
    function nodeRangeError(message: string) {
      const e: any = new RangeError(message);
      e.code = "ERR_OUT_OF_RANGE";
      return e;
    }
    const BUFFER_OR_UINT8ARRAY = ["Buffer", "Uint8Array"];

    // validateNumber / validateInteger of NodeValidator.cpp.
    function validateNumber(value: unknown, name: string, min?: number, max?: number) {
      if (typeof value !== "number") throw invalidArgType(name, "number", value);
      const hasMin = min !== undefined;
      const hasMax = max !== undefined;
      if ((hasMin && value < min) || (hasMax && value > max) || ((hasMin || hasMax) && NumberIsNaN(value))) {
        if (hasMin && hasMax) throw outOfRange(name, `>= ${min} && <= ${max}`, value);
        if (hasMin) throw outOfRange(name, `>= ${min}`, value);
        if (hasMax) throw outOfRange(name, `<= ${max}`, value);
        throw outOfRange(name, "", value);
      }
    }
    function validateInteger(
      value: unknown,
      name: string,
      min = Number.MIN_SAFE_INTEGER,
      max = Number.MAX_SAFE_INTEGER,
    ) {
      if (typeof value !== "number") throw invalidArgType(name, "number", value);
      if (value % 1 !== 0) throw outOfRange(name, "an integer", value);
      if (value < min || value > max) throw outOfRange(name, `>= ${min} && <= ${max}`, value);
      return value;
    }
    const validateString = (value: unknown, name: string) => {
      if (typeof value !== "string") throw invalidArgType(name, "string", value);
    };
    /** ToLength. */
    function toLength(value: number) {
      const n = MathTrunc(value) || 0;
      return n <= 0 ? 0 : MathMin(n, Number.MAX_SAFE_INTEGER);
    }

    // ───────────────────────── encodings ─────────────────────────
    const parseEncoding = (name: string) => natives.parseEncoding(name);
    /** parseEncoding of JSBuffer.cpp: `text` is the encoding as a string, `value` what was passed. */
    function encodingOf(text: string, value: unknown, validateUnknown: boolean) {
      const encoding = parseEncoding(text);
      if (encoding < 0) {
        if (validateUnknown) validateString(value, "encoding");
        throw unknownEncoding(text);
      }
      return encoding;
    }
    /** resolveEncodingString: `undefined`, `null` and `""` are no encoding (utf8). */
    function resolveEncodingString(value: unknown): string | undefined {
      if (value == null) return undefined;
      if (typeof value === "string") return value.length ? value : undefined;
      return StringCtor(value);
    }

    // ───────────────────────── making buffers ─────────────────────────
    let BufferPrototype: object;
    /** A zeroed Buffer of `length` bytes. */
    const allocate = (length: number): Buffer => ReflectConstruct(Uint8Array, [length], Buffer);
    /** A Uint8Array the natives made, as a Buffer. */
    const adopt = (array: Uint8Array): Buffer => setPrototypeOf(array, BufferPrototype) as Buffer;
    const fromBytes = (view: ArrayBufferView): Buffer => {
      const out = allocate(byteLengthOf(view));
      natives.copy(view, 0, out, 0, out.length);
      return out;
    };

    function fromString(value: unknown, encodingValue: unknown) {
      const text = StringCtor(value);
      let encoding = UTF8;
      if (typeof encodingValue === "string") {
        encoding = parseEncoding(encodingValue);
        if (encoding < 0) throw unknownEncoding(encodingValue);
      }
      if (text.length === 0) return allocate(0);
      return adopt(natives.encode(text, encoding));
    }

    // constructBufferFromArrayBuffer
    function fromArrayBuffer(buffer: ArrayBufferLike, offsetValue: unknown, lengthValue: unknown) {
      if (isDetachedBuffer(buffer)) throw new TypeError("Buffer is detached");
      const byteLength: number = ReflectApply(
        isArrayBuffer(buffer) ? arrayBufferByteLength : sharedByteLength,
        buffer,
        [],
      );
      let offset = 0;
      let length = byteLength;
      let offsetD = 0;
      if (offsetValue !== undefined) {
        offsetD = +(offsetValue as number);
        if (NumberIsNaN(offsetD)) offsetD = 0;
        if (offsetD > byteLength || MathTrunc(offsetD) < 0) throw outOfBounds("offset");
        offset = MathTrunc(offsetD);
        length -= offset;
      }
      if (lengthValue !== undefined) {
        const lengthD = +(lengthValue as number);
        if (lengthD > 0) {
          if (lengthD > byteLength - offsetD) throw outOfBounds("length");
          length = MathTrunc(lengthD);
        } else {
          length = 0;
        }
      }
      const growable = (buffer as any).resizable === true || (buffer as any).growable === true;
      if (growable && lengthValue === undefined) return ReflectConstruct(Uint8Array, [buffer, offset], Buffer);
      return ReflectConstruct(Uint8Array, [buffer, offset, length], Buffer);
    }

    // constructBufferFromArray
    function fromArrayLike(value: unknown) {
      if (Array.isArray(value) && value.length === 0) return allocate(0);
      return ReflectConstruct(Uint8Array, [value], Buffer);
    }

    // createJSBufferFromJS: `Buffer(…)` and `new Buffer(…)`.
    function Buffer(this: unknown, value?: unknown, encodingOrOffset?: unknown, length?: unknown): Buffer {
      if (arguments.length === 0) return allocate(0);
      if (typeof value === "number") {
        // isAnyInt: an int52.
        if (Number.isInteger(value) && !Object.is(value, -0) && value >= -(2 ** 51) && value < 2 ** 51) {
          if (typeof encodingOrOffset === "string") throw invalidArgType("string", "string", value);
          if (value < 0 || value > kMaxLength) throw outOfRange("size", `>= 0 and <= ${kMaxLength}`, value);
          return allocate(value);
        }
        validateNumber(value, "size", 0, kMaxLength);
        return allocate(toLength(value));
      }
      if (value == null || typeof value === "boolean")
        throw new TypeError(
          "The first argument must be of type string or an instance of Buffer, ArrayBuffer, Array or an Array-like object. Received " +
            StringCtor(value),
        );
      if (typeof value === "string" || value instanceof String) return fromString(value, encodingOrOffset);
      if (isView(value)) {
        const tag = tagOf(value);
        if (tag === undefined || tag === "Uint8Array" || tag === "Uint8ClampedArray") {
          if (isDetached(value)) throw new RangeError("Buffer is detached");
          return fromBytes(value);
        }
        if (isDetached(value)) throw new RangeError("Buffer is detached");
        // A BigInt element does not convert to a byte, but an empty array has none to convert.
        if ((tag === "BigInt64Array" || tag === "BigUint64Array") && lengthOf(value) === 0) return allocate(0);
        return ReflectConstruct(Uint8Array, [value], Buffer);
      }
      if (isArrayBuffer(value) || isSharedArrayBuffer(value))
        return fromArrayBuffer(value as ArrayBufferLike, encodingOrOffset, length);
      return fromArrayLike(value);
    }
    BufferPrototype = Buffer.prototype = Object.create(Uint8Array.prototype);
    setPrototypeOf(Buffer, Uint8Array);
    env.provide(Buffer);

    const rename = (fn: Function, name: string, length: number) => {
      defineProperty(fn, "name", { value: name, configurable: true });
      defineProperty(fn, "length", { value: length, configurable: true });
      return fn;
    };
    /** `fn` as a host function is: one that `new` cannot construct. */
    const named = (fn: Function, name: string, length: number) =>
      rename(
        {
          host(this: unknown, ...args: unknown[]) {
            return ReflectApply(fn, this, args);
          },
        }.host,
        name,
        length,
      );
    function SlowBuffer(size: unknown) {
      return allocUnsafeSlow(size);
    }
    rename(SlowBuffer, "SlowBuffer", 0);
    defineProperty(SlowBuffer, "prototype", {
      value: BufferPrototype,
      writable: false,
      enumerable: false,
      configurable: false,
    });

    // ───────────────────────── Buffer.* ─────────────────────────
    // jsBufferConstructorFunction_allocBody
    function alloc(size: unknown, fill?: unknown, encodingValue?: unknown) {
      validateNumber(size, "size", 0, kMaxLength);
      const length = toLength(size as number);
      if (length === 0) return allocate(0);
      const buffer = allocate(length);
      if (arguments.length > 1) fillWith(buffer, fill, arguments.length > 2, encodingValue);
      return buffer;
    }
    function fillWith(buffer: Buffer, value: unknown, hasEncoding: boolean, encodingValue: unknown) {
      const length = buffer.length;
      if (typeof value === "string") {
        let encoding = UTF8;
        if (hasEncoding) {
          const name = resolveEncodingString(encodingValue);
          if (name !== undefined) encoding = encodingOf(name, encodingValue, true);
        }
        if (value.length === 0) return;
        if (!natives.fill(buffer, 0, length, value, encoding)) throw makeError("ERR_INVALID_ARG_VALUE", "value", value);
      } else if (isView(value)) {
        if (byteLengthOf(value) === 0) throw makeError("ERR_INVALID_ARG_VALUE", "value", value);
        natives.fill(buffer, 0, length, value, UTF8);
      } else {
        const byte = ((value as number) | 0) & 0xff;
        ReflectApply(TypedArrayFill, buffer, [byte]);
      }
    }
    // jsBufferConstructorFunction_allocUnsafeBody: Boa's buffers are always zeroed.
    function allocUnsafe(size: unknown) {
      validateNumber(size, "size", 0, kMaxLength);
      return allocate(toLength(size as number));
    }
    function allocUnsafeSlow(size: unknown) {
      validateNumber(size, "size", 0, kMaxLength);
      return allocate(toLength(size as number));
    }
    function byteLength(value: unknown, encodingValue?: unknown) {
      let encoding = UTF8;
      if (arguments.length > 1 && typeof encodingValue === "string") {
        const parsed = parseEncoding(encodingValue);
        if (parsed >= 0) encoding = parsed;
      }
      if (typeof value === "string") return natives.byteLength(value, encoding);
      if (isView(value)) return byteLengthOf(value);
      if (isArrayBuffer(value)) return ReflectApply(arrayBufferByteLength, value, []);
      if (isSharedArrayBuffer(value)) return ReflectApply(sharedByteLength, value, []);
      throw invalidArgType("string", "string or an instance of Buffer or ArrayBuffer", value);
    }
    function compare(buf1: unknown, buf2: unknown) {
      if (!isView(buf1)) throw invalidArgInstance("buf1", BUFFER_OR_UINT8ARRAY, buf1);
      if (!isView(buf2)) throw invalidArgInstance("buf2", BUFFER_OR_UINT8ARRAY, buf2);
      return natives.compare(buf1, 0, byteLengthOf(buf1), buf2, 0, byteLengthOf(buf2));
    }
    function concat(list: unknown, totalLength?: unknown) {
      if (arguments.length < 1) return allocate(0);
      if (!Array.isArray(list)) throw invalidArgInstance("list", ["Array"], list);
      const items: unknown[] = [];
      const count = list.length;
      for (let i = 0; i < count; i++) items.push(list[i]);
      if (items.length === 0) return allocate(0);
      let available = 0;
      for (let i = 0; i < items.length; i++) {
        const item = items[i];
        if (!isUint8Array(item)) throw invalidArgInstance(`list[${i}]`, BUFFER_OR_UINT8ARRAY, item);
        if (isDetached(item)) throw new TypeError("ArrayBufferView is detached");
        available += byteLengthOf(item);
      }
      let length = available;
      if (totalLength !== undefined) {
        if (typeof totalLength !== "number") throw invalidArgType("length", "number", totalLength);
        if (!NumberIsFinite(totalLength) || MathTrunc(totalLength) !== totalLength)
          throw outOfRange("length", "an integer", totalLength);
        if (totalLength < 0 || totalLength > kMaxLength)
          throw outOfRange("length", `>= 0 && <= ${kMaxLength}`, totalLength);
        length = totalLength;
      }
      if (length === 0) return allocate(0);
      const out = allocate(length);
      let position = 0;
      for (let i = 0; i < items.length && position < length; i++)
        position += natives.copy(items[i] as Uint8Array, 0, out, position, length - position);
      return out;
    }
    // jsBufferConstructorFunction_copyBytesFromBody
    function copyBytesFrom(view: unknown, offsetValue?: unknown, lengthValue?: unknown) {
      if (!isView(view)) throw invalidArgType("view", "TypedArray", view);
      const viewLength = lengthOf(view);
      if (viewLength === 0) return allocate(0);
      const elementSize = byteLengthOf(view) / viewLength || 1;
      let offset = 0;
      let end = viewLength;
      if (offsetValue !== undefined || lengthValue !== undefined) {
        if (offsetValue !== undefined) {
          offset = validateInteger(offsetValue, "offset", 0);
          if (offset >= viewLength) return allocate(0);
        }
        if (lengthValue !== undefined) end = offset + validateInteger(lengthValue, "length", 0);
        end = MathMin(end, viewLength);
      }
      const out = allocate((end - offset) * elementSize);
      natives.copy(view, offset * elementSize, out, 0, out.length);
      return out;
    }
    function isEncoding(encoding: unknown) {
      return typeof encoding === "string" && parseEncoding(encoding) >= 0;
    }

    // jsBufferConstructorTable, with its lengths: `new Buffer.alloc(n)` works, as `Constructable` there says.
    for (const [fn, length, constructable] of [
      [alloc, 1, true],
      [allocUnsafe, 1, true],
      [allocUnsafeSlow, 1, true],
      [byteLength, 2, false],
      [compare, 2, false],
      [concat, 2, false],
      [copyBytesFrom, 1, false],
      [isEncoding, 1, false],
    ] as const)
      defineProperty(Buffer, fn.name, {
        value: (constructable ? rename : named)(fn, fn.name, length),
        writable: true,
        enumerable: false,
        configurable: true,
      });
    for (const name of ["from", "isBuffer"])
      defineProperty(Buffer, name, {
        value: env.builtinFunction("JSBufferConstructor", name),
        writable: true,
        enumerable: false,
        configurable: true,
      });
    defineProperty(Buffer, "prototype", { writable: false, enumerable: false, configurable: false });
    defineProperty(Buffer, "poolSize", { value: 8192, writable: true, enumerable: true, configurable: true });

    // ───────────────────────── Buffer.prototype ─────────────────────────
    /** IDLOperation<JSArrayBufferView>::call: `this` is a Uint8Array. */
    function thisBuffer(self: unknown, name: string): Uint8Array {
      if (self == null) throw new TypeError("Cannot convert undefined or null to object");
      if (!isUint8Array(self)) throw new TypeError(`Can only call Buffer.${name} on instances of Buffer`);
      return self;
    }
    /** The receiver of the `<encoding>Slice`/`<encoding>Write` bindings: any view. */
    function thisView(self: unknown): ArrayBufferView {
      if (!isView(self)) throw new TypeError("Expected ArrayBufferView");
      return self;
    }

    const proto: Record<string, Function> = {};

    proto.compare = function compare(
      this: unknown,
      target: unknown,
      targetStart?: unknown,
      targetEnd?: unknown,
      sourceStart?: unknown,
      sourceEnd?: unknown,
    ) {
      const source = thisBuffer(this, "compare");
      if (!isUint8Array(target)) throw invalidArgInstance("target", BUFFER_OR_UINT8ARRAY, target);
      const targetLength = byteLengthOf(target);
      const sourceLength = byteLengthOf(source);
      let tStart = 0;
      let tEnd = targetLength;
      let sStart = 0;
      let sEnd = sourceLength;
      const count = arguments.length;
      if (count >= 5 && sourceEnd !== undefined) sEnd = validateInteger(sourceEnd, "sourceEnd", 0, kMaxLength);
      if (count >= 4 && sourceStart !== undefined) sStart = validateInteger(sourceStart, "sourceStart", 0, kMaxLength);
      if (count >= 3 && targetEnd !== undefined) tEnd = validateInteger(targetEnd, "targetEnd", 0, kMaxLength);
      if (count >= 2 && targetStart !== undefined) tStart = validateInteger(targetStart, "targetStart", 0, kMaxLength);
      if (tEnd > targetLength) throw outOfRange("targetEnd", `>= 0 and <= ${targetLength}`, targetEnd);
      if (sEnd > sourceLength) throw outOfRange("sourceEnd", `>= 0 and <= ${sourceLength}`, sourceEnd);
      if (sStart >= sEnd) return tStart >= tEnd ? 0 : -1;
      if (tStart >= tEnd) return 1;
      return natives.compare(source, sStart, sEnd, target, tStart, tEnd);
    };

    /** toCopyOffset: an integral number as it is, else toInteger(n, 0). */
    function toCopyOffset(value: unknown) {
      if (typeof value === "number" && NumberIsFinite(value) && MathTrunc(value) === value) return value;
      const n = +(value as number);
      if (NumberIsNaN(n) || n < Number.MIN_SAFE_INTEGER || n > Number.MAX_SAFE_INTEGER) return 0;
      return MathFloor(n);
    }
    proto.copy = function copy(
      this: unknown,
      target: unknown,
      targetStart?: unknown,
      sourceStart?: unknown,
      sourceEnd?: unknown,
    ) {
      if (!isUint8Array(this)) throw invalidArgInstance("source", BUFFER_OR_UINT8ARRAY, this);
      const source = this;
      if (!isView(target)) throw invalidArgInstance("target", BUFFER_OR_UINT8ARRAY, target);
      let tStart = 0;
      if (targetStart !== undefined) {
        tStart = toCopyOffset(targetStart);
        if (tStart < 0) throw outOfRange("targetStart", ">= 0", tStart);
      }
      let sStart = 0;
      if (sourceStart !== undefined) {
        sStart = toCopyOffset(sourceStart);
        const length = byteLengthOf(source);
        if (sStart < 0 || sStart > length) throw outOfRange("sourceStart", `>= 0 && <= ${length}`, sStart);
      }
      let sEnd = 0;
      const sourceEndGiven = sourceEnd !== undefined;
      if (sourceEndGiven) {
        sEnd = toCopyOffset(sourceEnd);
        if (sEnd < 0) throw outOfRange("sourceEnd", ">= 0", sEnd);
      }
      const sourceLength = byteLengthOf(source);
      const targetLength = byteLengthOf(target);
      if (!sourceEndGiven || sEnd > sourceLength) sEnd = sourceLength;
      if (tStart >= targetLength || sStart >= sEnd) return 0;
      if (sEnd - sStart > targetLength - tStart) sEnd = sStart + (targetLength - tStart);
      return natives.copy(source, sStart, target, tStart, sEnd - sStart);
    };

    proto.equals = function equals(this: unknown, otherBuffer: unknown) {
      const self = thisBuffer(this, "equals");
      if (arguments.length < 1) {
        const e: any = new TypeError("Not enough arguments");
        e.code = "ERR_MISSING_ARGS";
        throw e;
      }
      if (!isView(otherBuffer)) throw invalidArgInstance("otherBuffer", BUFFER_OR_UINT8ARRAY, otherBuffer);
      const a = byteLengthOf(self);
      const b = byteLengthOf(otherBuffer);
      return a === b && natives.compare(self, 0, a, otherBuffer, 0, b) === 0;
    };

    // jsBufferPrototypeFunction_fillBody
    proto.fill = function fill(
      this: unknown,
      value?: unknown,
      offsetValue?: unknown,
      endValue?: unknown,
      encodingValue?: unknown,
    ) {
      const self = thisBuffer(this, "fill");
      const limit = byteLengthOf(self);
      let offset = 0;
      let end = limit;
      let encoding = UTF8;
      if (typeof value === "string") {
        if (offsetValue === undefined || typeof offsetValue === "string") {
          encodingValue = offsetValue;
          offsetValue = undefined;
          endValue = undefined;
        } else if (typeof endValue === "string") {
          encodingValue = endValue;
          endValue = undefined;
        }
        const name = resolveEncodingString(encodingValue);
        if (name !== undefined) encoding = encodingOf(name, encodingValue, true);
      }
      if (offsetValue !== undefined) {
        offset = validateInteger(offsetValue, "offset", 0, kMaxLength);
        if (endValue !== undefined) end = validateInteger(endValue, "end", 0, limit);
      }
      if (offset >= end) return self;
      let byte = 0;
      if (typeof value === "string") {
        // A string is already one.
      } else if (isView(value)) {
        if (byteLengthOf(value) === 0) throw error("ERR_INVALID_ARG_VALUE", "Buffer cannot be empty");
      } else {
        byte = ((value as number) | 0) & 0xff;
      }
      const postLimit = byteLengthOf(self);
      if (offset > postLimit) offset = postLimit;
      if (end > postLimit) end = postLimit;
      if (offset >= end) return self;
      if (typeof value === "string") {
        if (value.length === 0) natives.fill(self, offset, end, 0, UTF8);
        else if (!natives.fill(self, offset, end, value, encoding))
          throw makeError("ERR_INVALID_ARG_VALUE", "value", value);
      } else if (isView(value)) {
        natives.fill(self, offset, end, value, UTF8);
      } else {
        natives.fill(self, offset, end, byte, UTF8);
      }
      return self;
    };

    // indexOf of JSBuffer.cpp
    function search(self: Uint8Array, args: IArguments, last: boolean): number {
      const byteLength = byteLengthOf(self);
      let value = args[0];
      let byteOffsetValue = args[1];
      let endValue = args[2];
      let encodingValue = args[3];
      if (typeof endValue === "string") {
        encodingValue = endValue;
        endValue = undefined;
      }
      let end = Infinity;
      if (endValue !== undefined) {
        end = +(endValue as number);
        if (NumberIsNaN(end)) end = Infinity;
      }
      let byteOffset: number;
      if (typeof byteOffsetValue === "string") {
        encodingValue = byteOffsetValue;
        byteOffset = NaN;
      } else {
        byteOffset = +(byteOffsetValue as number);
        if (byteOffset > 0x7fffffff) byteOffset = 0x7fffffff;
        if (byteOffset < -0x80000000) byteOffset = -0x80000000;
      }
      if (NumberIsNaN(byteOffset)) byteOffset = last ? byteLength : 0;
      if (typeof value === "number") {
        const byte = (value | 0) & 0xff;
        if (isDetached(self)) return -1;
        return natives.indexOf(self, byte, byteOffset, end, last, UTF8);
      }
      let encoding = UTF8;
      let encodingName = "";
      if (encodingValue !== undefined) {
        encodingName = StringCtor(encodingValue);
        encoding = parseEncoding(encodingName);
      }
      if (typeof value === "string") {
        if (encoding < 0) throw unknownEncoding(encodingName);
        if (isDetached(self)) return -1;
        return natives.indexOf(self, value, byteOffset, end, last, encoding);
      }
      if (isUint8Array(value)) {
        if (encoding < 0) encoding = UTF8;
        if (isDetached(self)) return -1;
        return natives.indexOf(self, value, byteOffset, end, last, encoding);
      }
      throw error(
        "ERR_INVALID_ARG_TYPE",
        'The "value" argument must be one of type number or string or an instance of Buffer or Uint8Array. Received ' +
          specificType(value),
      );
    }
    proto.includes = function includes(this: unknown, value: unknown, byteOffset?: unknown, encoding?: unknown) {
      return search(thisBuffer(this, "includes"), arguments, false) !== -1;
    };
    proto.indexOf = function indexOf(this: unknown, value: unknown, byteOffset?: unknown, encoding?: unknown) {
      return search(thisBuffer(this, "indexOf"), arguments, false);
    };
    proto.lastIndexOf = function lastIndexOf(this: unknown, value: unknown, byteOffset?: unknown, encoding?: unknown) {
      return search(thisBuffer(this, "lastIndexOf"), arguments, true);
    };

    let INSPECT_MAX_BYTES = 50;
    proto.inspect = function inspect(this: unknown, recurseTimes?: unknown, ctx?: any) {
      const self = thisBuffer(this, "inspect");
      const length = byteLengthOf(self);
      const shown = MathMin(INSPECT_MAX_BYTES, length);
      let result = "<Buffer";
      let any = false;
      if (shown > 0) {
        const hex = natives.decode(self, 0, shown, HEX);
        for (let i = 0; i < hex.length; i += 2) result += " " + hex.slice(i, i + 2);
        any = true;
      }
      if (length > INSPECT_MAX_BYTES) {
        const remaining = length - INSPECT_MAX_BYTES;
        result += ` ... ${remaining} more byte${remaining > 1 ? "s" : ""}`;
      }
      if (ctx) {
        const showHidden = ctx.showHidden;
        const keys = Reflect.ownKeys(self).filter(
          key =>
            !(typeof key === "string" && /^(0|[1-9]\d*)$/.test(key)) &&
            (showHidden || getOwnPropertyDescriptor(self, key)!.enumerable),
        );
        if (keys.length > 0) {
          any = true;
          if (length > 0) result += ",";
          result += " ";
          const inspect = typeof G.Bun?.inspect === "function" ? G.Bun.inspect : StringCtor;
          result += keys
            .map(key => `${typeof key === "symbol" ? key.toString() : key}: ${inspect((self as any)[key])}`)
            .join(", ");
        }
      }
      if (!any) result += " ";
      return result + ">";
    };

    /** adjustSliceOffset: relative to the end when negative, clamped to the buffer. */
    function sliceOffset(value: unknown, length: number) {
      let offset = +(value as number);
      if (NumberIsNaN(offset)) return 0;
      offset = MathTrunc(offset);
      if (offset < 0) return offset + length > 0 ? offset + length : 0;
      return offset < length ? offset : length;
    }
    const slicing = (name: string) =>
      function (this: unknown, start?: unknown, end?: unknown) {
        return slice(thisBuffer(this, name), arguments.length, start, end);
      };
    function slice(self: Uint8Array, count: number, start: unknown, end: unknown) {
      const byteLength = byteLengthOf(self);
      const byteOffset: number = ReflectApply(byteOffsetGetter, self, []);
      let startOffset = 0;
      let endOffset = byteLength;
      if (count > 0 && start !== undefined) startOffset = sliceOffset(start, byteLength);
      if (count > 1 && end !== undefined) endOffset = sliceOffset(end, byteLength);
      const length = endOffset > startOffset ? endOffset - startOffset : 0;
      if (isDetached(self)) throw new TypeError("Buffer is detached");
      return ReflectConstruct(
        Uint8Array,
        [ReflectApply(bufferGetter, self, []), byteOffset + startOffset, length],
        Buffer,
      );
    }
    proto.slice = slicing("slice");
    proto.subarray = slicing("subarray");

    function swapper(size: number) {
      return function (this: unknown) {
        const self = thisBuffer(this, `swap${size * 8}`);
        if (byteLengthOf(self) % size !== 0)
          throw error("ERR_INVALID_BUFFER_SIZE", `Buffer size must be a multiple of ${size * 8}-bits`);
        natives.swap(self, size);
        return self;
      };
    }
    proto.swap16 = swapper(2);
    proto.swap32 = swapper(4);
    proto.swap64 = swapper(8);

    /** jsBufferToString: the checks of jsBufferToStringFromBytes, then the natives. */
    function decode(view: ArrayBufferView, start: number, end: number, encoding: number) {
      const length = end - start;
      if (length <= 0) return "";
      if (
        length > kStringMaxLength ||
        (encoding === HEX && length > kStringMaxLength / 2) ||
        ((encoding === BASE64 || encoding === BASE64URL) && length > MathFloor(kStringMaxLength / 4) * 3)
      )
        throw error("ERR_STRING_TOO_LONG", `Cannot create a string longer than ${kStringMaxLength} characters`);
      return natives.decode(view, start, end, encoding);
    }
    proto.toString = function toString(this: unknown, encodingValue?: unknown, start?: unknown, end?: unknown) {
      const self = thisBuffer(this, "toString");
      const byteLength = byteLengthOf(self);
      if (byteLength === 0) return "";
      if (arguments.length === 0) return decode(self, 0, byteLength, UTF8);
      let encoding = UTF8;
      if (encodingValue !== undefined) encoding = encodingOf(StringCtor(encodingValue), encodingValue, false);
      let from: number;
      const fstart = +(start as number);
      if (!(fstart >= 0)) from = 0;
      else if (fstart > byteLength) return "";
      else from = MathTrunc(fstart);
      let to = byteLength;
      if (end !== undefined) {
        const lend = toLength(+(end as number));
        if (lend < byteLength) to = lend;
      }
      if (to <= from) return "";
      return decode(self, from, to, encoding);
    };

    /** parseArrayIndex of node_internals.h. */
    function arrayIndex(value: unknown, fallback: number) {
      if (value === undefined) return fallback;
      const n = +(value as number);
      const index = NumberIsNaN(n) ? 0 : MathTrunc(n);
      if (index < 0) throw nodeRangeError("Index out of range");
      return index;
    }
    function slicer(encoding: number) {
      return function (this: unknown, start?: unknown, end?: unknown) {
        const self = thisView(this);
        const length = byteLengthOf(self);
        if (length === 0) return "";
        const from = arrayIndex(start, 0);
        const to = arrayIndex(end, length);
        if (from >= to) return "";
        if (to > length) throw nodeRangeError("Index out of range");
        return decode(self, from, to, encoding);
      };
    }
    // jsBufferPrototypeFunction_writeEncodingBody: utf8Write, latin1Write and asciiWrite.
    function strictWriter(encoding: number) {
      return function (this: unknown, text: unknown, offsetValue?: unknown, lengthValue?: unknown) {
        const self = thisView(this);
        const offset = offsetValue === undefined ? 0 : +(offsetValue as number);
        const lengthUndefined = lengthValue === undefined;
        const length = lengthUndefined ? 0 : +(lengthValue as number);
        const byteLength = byteLengthOf(self);
        const offsetNaN = NumberIsNaN(offset);
        if (!offsetNaN && (offset < 0 || offset > byteLength)) throw outOfBounds("offset");
        const safeOffset = offsetNaN ? 0 : MathTrunc(offset);
        let maxLength: number;
        if (lengthUndefined) {
          maxLength = offsetNaN ? 0 : byteLength - safeOffset;
        } else {
          if (!offsetNaN && (length < 0 || length > byteLength - offset)) throw outOfBounds("length");
          const intLength = NumberIsNaN(length) || length < 0 ? 0 : MathTrunc(length);
          maxLength = MathMin(byteLength - safeOffset, intLength);
        }
        if (typeof text !== "string") throw error("ERR_INVALID_ARG_TYPE", "argument must be a string");
        if (text.length === 0) return 0;
        return natives.write(self, text, safeOffset, maxLength, encoding);
      };
    }
    // jsBufferPrototypeFunction_StringWriteWithEncoding: base64, base64url, hex and ucs2.
    function bindingWriter(encoding: number) {
      return function (this: unknown, text: unknown, offsetValue?: unknown, lengthValue?: unknown) {
        const self = thisView(this);
        if (typeof text !== "string") throw error("ERR_INVALID_ARG_TYPE", "argument must be a string");
        const offset = arrayIndex(offsetValue, 0);
        if (typeof offsetValue === "object" && offsetValue !== null && isDetached(self))
          throw new TypeError("ArrayBufferView is detached");
        let byteLength = byteLengthOf(self);
        if (offset > byteLength) throw outOfBounds("offset");
        let maxLength = arrayIndex(lengthValue, byteLength - offset);
        if (typeof lengthValue === "object" && lengthValue !== null) {
          if (isDetached(self)) throw new TypeError("ArrayBufferView is detached");
          byteLength = byteLengthOf(self);
        }
        maxLength = MathMin(offset < byteLength ? byteLength - offset : 0, maxLength);
        if (maxLength === 0 || text.length === 0) return 0;
        return natives.write(self, text, offset, maxLength, encoding);
      };
    }
    for (const [name, encoding] of [
      ["ascii", ASCII],
      ["base64", BASE64],
      ["base64url", BASE64URL],
      ["hex", HEX],
      ["latin1", LATIN1],
      ["ucs2", UTF16LE],
      ["utf16le", UTF16LE],
      ["utf8", UTF8],
    ] as const) {
      proto[`${name}Slice`] = named(slicer(encoding), `${name}Slice`, 2);
      const strict = encoding === UTF8 || encoding === LATIN1 || encoding === ASCII;
      proto[`${name}Write`] = named((strict ? strictWriter : bindingWriter)(encoding), `${name}Write`, 3);
    }

    // jsBufferPrototypeFunction_writeBody
    function validateOffset(value: unknown, name: string, min: number, max: number) {
      if (typeof value !== "number") throw invalidArgType(name, "number", value);
      if (value % 1 !== 0) throw outOfRange(name, "an integer", value);
      if (value < min || value > max) throw outOfRange(name, `>= ${min} && <= ${max}`, value);
      return value;
    }
    proto.write = function write(
      this: unknown,
      text: unknown,
      offsetValue?: unknown,
      lengthValue?: unknown,
      encodingValue?: unknown,
    ) {
      const self = thisBuffer(this, "write");
      let offset: number;
      let length: number;
      if (offsetValue === undefined) {
        validateString(text, "string");
        return natives.write(self, text as string, 0, byteLengthOf(self), UTF8);
      }
      if (lengthValue === undefined && typeof offsetValue === "string") {
        const string = StringCtor(text);
        const encoding = encodingOf(StringCtor(offsetValue), offsetValue, false);
        if (isDetached(self)) throw new TypeError("ArrayBufferView is detached");
        return natives.write(self, string, 0, byteLengthOf(self), encoding);
      }
      length = byteLengthOf(self);
      offset = validateOffset(offsetValue, "offset", 0, length);
      const remaining = byteLengthOf(self) - offset;
      if (lengthValue === undefined) {
        length = remaining;
      } else if (typeof lengthValue === "string") {
        encodingValue = lengthValue;
        length = remaining;
      } else {
        length = validateOffset(lengthValue, "length", 0, length);
        if (length > remaining) length = remaining;
      }
      validateString(text, "string");
      if (!encodingValue) return natives.write(self, text as string, offset, length, UTF8);
      const encoding = encodingOf(StringCtor(encodingValue), encodingValue, false);
      if (isDetached(self)) throw new TypeError("ArrayBufferView is detached");
      const current = byteLengthOf(self);
      if (offset >= current) return 0;
      if (length > current - offset) length = current - offset;
      return natives.write(self, text as string, offset, length, encoding);
    };

    // ───────────────────────── read and write numbers ─────────────────────────
    // As lib/internal/buffer.js: the offset's type, then the receiver, then its range.
    /** The bytes and the checked offset of an access of `size` bytes at `offsetValue`. */
    function access(self: unknown, offsetValue: unknown, size: number): [Uint8Array, number] {
      if (offsetValue === undefined) offsetValue = 0;
      if (typeof offsetValue !== "number") throw invalidArgType("offset", "number", offsetValue);
      if (!isView(self)) throw invalidArgType("buf", "Buffer", self);
      const offset = offsetValue;
      if (MathFloor(offset) !== offset) throw outOfRange("offset", "an integer", offsetValue);
      if (isDataView(self)) throw outOfRange("offset", ">= 0 and <= NaN", offsetValue);
      const length = lengthOf(self);
      if (length < size) throw outOfBounds();
      if (!(offset >= 0 && offset <= length - size))
        throw outOfRange("offset", `>= 0 and <= ${length - size}`, offsetValue);
      return [bytesOf(self), offset];
    }
    const scratch = new ArrayBuffer(8);
    const scratchBytes = new Uint8Array(scratch);
    const scratchFloat = new Float32Array(scratch);
    const scratchDouble = new Float64Array(scratch);
    /** The bytes at `offset`, least significant first. */
    function load(bytes: Uint8Array, offset: number, size: number, littleEndian: boolean) {
      for (let i = 0; i < size; i++) scratchBytes[i] = bytes[offset + (littleEndian ? i : size - 1 - i)];
    }
    function store(bytes: Uint8Array, offset: number, size: number, littleEndian: boolean) {
      for (let i = 0; i < size; i++) bytes[offset + (littleEndian ? i : size - 1 - i)] = scratchBytes[i];
    }
    /** The unsigned integer of `size` (<= 6) bytes in the scratch. */
    function unsigned(size: number) {
      let value = 0;
      for (let i = size - 1; i >= 0; i--) value = value * 256 + scratchBytes[i];
      return value;
    }
    type Kind = "int" | "uint" | "float" | "bigint" | "biguint";
    function reader(kind: Kind, size: number, littleEndian: boolean) {
      return function (this: unknown, offset?: unknown) {
        const [bytes, at] = access(this, offset, size);
        load(bytes, at, size, littleEndian);
        if (kind === "float") return size === 4 ? scratchFloat[0] : scratchDouble[0];
        if (kind === "bigint" || kind === "biguint") {
          const low = BigInt(unsigned(4));
          let high = 0;
          for (let i = 7; i >= 4; i--) high = high * 256 + scratchBytes[i];
          const value = (BigInt(high) << 32n) | low;
          return kind === "bigint" ? BigInt.asIntN(64, value) : value;
        }
        const value = unsigned(size);
        if (kind === "int" && value >= 2 ** (8 * size - 1)) return value - 2 ** (8 * size);
        return value;
      };
    }
    /** ToInt32 of `number`, its low `size` (<= 4) bytes into the scratch. */
    function put32(number: number, size: number, at = 0) {
      const bits = number | 0;
      for (let i = 0; i < size; i++) scratchBytes[at + i] = (bits >>> (8 * i)) & 0xff;
    }
    function checkValue(number: number, size: number, signed: boolean) {
      const min = signed ? -(2 ** (8 * size - 1)) : 0;
      const max = (signed ? 2 ** (8 * size - 1) : 2 ** (8 * size)) - 1;
      if (!(number > max || number < min)) return;
      if (size > 4) {
        if (signed) throw outOfRange("value", `>= -(2 ** ${8 * size - 1}) and < 2 ** ${8 * size - 1}`, number);
        throw outOfRange("value", `>= 0 and < 2 ** ${8 * size}`, number);
      }
      throw outOfRange("value", `>= ${min} and <= ${max}`, number);
    }
    /** The offset type check that a garbage receiver of a writer does not win over. */
    const checkOffsetType = (offsetValue: unknown) => {
      if (offsetValue !== undefined && typeof offsetValue !== "number")
        throw invalidArgType("offset", "number", offsetValue);
    };
    function writer(kind: Kind, size: number, littleEndian: boolean) {
      return function (this: unknown, value: unknown, offset?: unknown) {
        const number = +(value as number);
        if (kind === "float") {
          checkOffsetType(offset);
        } else if (size === 1) {
          checkOffsetType(offset);
          checkValue(number, size, kind === "int");
        } else {
          checkValue(number, size, kind === "int");
          checkOffsetType(offset);
        }
        const [bytes, at] = access(this, offset, size);
        if (kind === "float") {
          if (size === 4) scratchFloat[0] = number;
          else scratchDouble[0] = number;
        } else {
          put32(number, size);
        }
        store(bytes, at, size, littleEndian);
        return at + size;
      };
    }
    function bigWriter(signed: boolean, littleEndian: boolean) {
      return function (this: unknown, value: unknown, offset?: unknown) {
        if (typeof value !== "bigint") throw invalidArgType("value", "bigint", value);
        if (signed ? value < -(2n ** 63n) || value >= 2n ** 63n : value < 0n || value >= 2n ** 64n)
          throw outOfRange("value", signed ? ">= -(2n ** 63n) and < 2n ** 63n" : ">= 0n and < 2n ** 64n", value);
        checkOffsetType(offset);
        if (!isView(this)) throw invalidArgType("buf", "Buffer", this);
        // validateOffsetBigInt64: the type and integer-ness of the offset, a too short buffer, then its range.
        let at = 0;
        if (offset !== undefined) {
          at = offset as number;
          if (MathFloor(at) !== at) throw outOfRange("offset", "an integer", offset);
        }
        if (isDataView(this)) throw outOfRange("offset", ">= 0 and <= NaN", at);
        const length = lengthOf(this);
        if (length < 8) throw outOfBounds();
        if (at < 0 || at > length - 8) throw outOfRange("offset", `>= 0 and <= ${length - 8}`, offset);
        const bits = BigInt.asUintN(64, value);
        put32(Number(bits & 0xffffffffn), 4);
        put32(Number(bits >> 32n), 4, 4);
        store(bytesOf(this), at, 8, littleEndian);
        return at + 8;
      };
    }
    // read(U)Int{LE,BE}(offset, byteLength) and write(U)Int{LE,BE}(value, offset, byteLength): 1 to 6 bytes.
    function checkByteLength(byteLength: unknown) {
      if (typeof byteLength !== "number") throw invalidArgType("byteLength", "number", byteLength);
      if (!(byteLength >= 1 && byteLength <= 6 && MathFloor(byteLength) === byteLength)) {
        if (MathFloor(byteLength) !== byteLength) throw outOfRange("byteLength", "an integer", byteLength);
        throw outOfRange("byteLength", ">= 1 and <= 6", byteLength);
      }
      return byteLength;
    }
    function variableReader(signed: boolean, littleEndian: boolean) {
      return function (this: unknown, offset: unknown, byteLength: unknown) {
        if (offset === undefined) throw invalidArgType("offset", "number", offset);
        const size = checkByteLength(byteLength);
        const [bytes, at] = access(this, offset, size);
        load(bytes, at, size, littleEndian);
        const value = unsigned(size);
        if (signed && value >= 2 ** (8 * size - 1)) return value - 2 ** (8 * size);
        return value;
      };
    }
    function variableWriter(signed: boolean, littleEndian: boolean) {
      return function (this: unknown, value: unknown, offset: unknown, byteLength: unknown) {
        const size = checkByteLength(byteLength);
        const number = +(value as number);
        if (size === 1) {
          if (typeof offset !== "number") throw invalidArgType("offset", "number", offset);
          checkValue(number, size, signed);
        } else {
          checkValue(number, size, signed);
          if (typeof offset !== "number") throw invalidArgType("offset", "number", offset);
        }
        const [bytes, at] = access(this, offset, size);
        if (size === 3) {
          put32(number, 2);
          put32(MathFloor(number / 65536), 1, 2);
        } else if (size > 4) {
          put32(number, 4);
          put32(MathFloor(number / 4294967296), size - 4, 4);
        } else {
          put32(number, size);
        }
        store(bytes, at, size, littleEndian);
        return at + size;
      };
    }

    for (const [name, kind, size] of [
      ["Int8", "int", 1],
      ["UInt8", "uint", 1],
      ["Int16", "int", 2],
      ["UInt16", "uint", 2],
      ["Int32", "int", 4],
      ["UInt32", "uint", 4],
      ["Float", "float", 4],
      ["Double", "float", 8],
      ["BigInt64", "bigint", 8],
      ["BigUInt64", "biguint", 8],
    ] as const) {
      const endians: [string, boolean][] =
        size === 1
          ? [["", true]]
          : [
              ["LE", true],
              ["BE", false],
            ];
      for (const [suffix, littleEndian] of endians) {
        proto[`read${name}${suffix}`] = named(reader(kind, size, littleEndian), `read${name}${suffix}`, 1);
        const write =
          kind === "bigint" || kind === "biguint"
            ? bigWriter(kind === "bigint", littleEndian)
            : writer(kind, size, littleEndian);
        proto[`write${name}${suffix}`] = named(
          write,
          `write${name}${suffix}`,
          kind === "bigint" || kind === "biguint" ? 3 : 2,
        );
      }
    }
    // The little-endian accessors without a suffix are functions of their own, as JSBufferPrototypeTableValues has them.
    for (const [name, kind, size] of [
      ["BigInt64", "bigint", 8],
      ["BigUInt64", "biguint", 8],
      ["Double", "float", 8],
      ["Float", "float", 4],
      ["Int16", "int", 2],
      ["Int32", "int", 4],
    ] as const)
      proto[`read${name}`] = named(reader(kind, size, true), `read${name}`, 1);
    for (const [name, kind, size] of [
      ["Double", "float", 8],
      ["Float", "float", 4],
      ["UInt16", "uint", 2],
      ["UInt32", "uint", 4],
    ] as const)
      proto[`write${name}`] = named(writer(kind, size, true), `write${name}`, 2);
    proto.readIntLE = named(variableReader(true, true), "readIntLE", 2);
    proto.readIntBE = named(variableReader(true, false), "readIntBE", 2);
    proto.readUIntLE = named(variableReader(false, true), "readUIntLE", 2);
    proto.readUIntBE = named(variableReader(false, false), "readUIntBE", 2);
    proto.writeIntLE = named(variableWriter(true, true), "writeIntLE", 3);
    proto.writeIntBE = named(variableWriter(true, false), "writeIntBE", 3);
    proto.writeUIntLE = named(variableWriter(false, true), "writeUIntLE", 3);
    proto.writeUIntBE = named(variableWriter(false, false), "writeUIntBE", 3);

    // The names and lengths of JSBufferPrototypeTableValues for those not made above.
    for (const [name, length] of [
      ["compare", 5],
      ["copy", 4],
      ["equals", 1],
      ["fill", 4],
      ["includes", 3],
      ["indexOf", 3],
      ["inspect", 2],
      ["lastIndexOf", 3],
      ["slice", 2],
      ["subarray", 2],
      ["swap16", 0],
      ["swap32", 0],
      ["swap64", 0],
      ["toString", 4],
      ["write", 4],
    ] as const)
      proto[name] = named(proto[name], name, length);
    // JSBufferPrototypeTableValues: enumerable, as `PropertyAttribute::Function` alone leaves them.
    for (const name in proto)
      defineProperty(BufferPrototype, name, {
        value: proto[name],
        writable: true,
        enumerable: true,
        configurable: true,
      });
    // The aliases of JSBufferPrototype::finishCreation: the same functions under a second name.
    for (const [alias, original] of [
      ["toLocaleString", "toString"],
      ["readUintBE", "readUIntBE"],
      ["readUintLE", "readUIntLE"],
      ["readUint8", "readUInt8"],
      ["readUint16BE", "readUInt16BE"],
      ["readUint16LE", "readUInt16LE"],
      ["readUint32BE", "readUInt32BE"],
      ["readUint32LE", "readUInt32LE"],
      ["readBigUint64BE", "readBigUInt64BE"],
      ["readBigUint64LE", "readBigUInt64LE"],
      ["writeUintBE", "writeUIntBE"],
      ["writeUintLE", "writeUIntLE"],
      ["writeUint8", "writeUInt8"],
      ["writeUint16", "writeUInt16"],
      ["writeUint16BE", "writeUInt16BE"],
      ["writeUint16LE", "writeUInt16LE"],
      ["writeUint32", "writeUInt32"],
      ["writeUint32BE", "writeUInt32BE"],
      ["writeUint32LE", "writeUInt32LE"],
      ["writeBigUint64BE", "writeBigUInt64BE"],
      ["writeBigUint64LE", "writeBigUInt64LE"],
    ])
      defineProperty(BufferPrototype, alias, {
        value: proto[original],
        writable: true,
        enumerable: true,
        configurable: true,
      });
    defineProperty(BufferPrototype, "toJSON", {
      value: env.builtinFunction("JSBufferPrototype", "toJSON"),
      writable: true,
      enumerable: true,
      configurable: true,
    });
    for (const name of ["parent", "offset"])
      defineProperty(BufferPrototype, name, {
        get: env.builtinFunction("JSBufferPrototype", name) as () => unknown,
        enumerable: false,
        configurable: true,
      });
    defineProperty(BufferPrototype, Symbol.for("nodejs.util.inspect.custom"), {
      value: proto.inspect,
      writable: true,
      enumerable: false,
      configurable: true,
    });
    defineProperty(BufferPrototype, Symbol.toStringTag, {
      value: "Uint8Array",
      writable: false,
      enumerable: false,
      configurable: true,
    });
    defineProperty(BufferPrototype, Symbol.species, {
      value: Buffer,
      writable: false,
      enumerable: false,
      configurable: false,
    });
    defineProperty(BufferPrototype, "constructor", {
      value: Buffer,
      writable: true,
      enumerable: false,
      configurable: true,
    });

    // ───────────────────────── node:buffer ─────────────────────────
    function viewOrBuffer(input: unknown, detached: () => Error): ArrayBufferView | ArrayBufferLike | undefined {
      if (isView(input)) {
        if (isDetached(input)) throw detached();
        return input;
      }
      if (isArrayBuffer(input) || isSharedArrayBuffer(input)) {
        if (isDetachedBuffer(input)) throw makeError("ERR_INVALID_STATE", "Cannot validate on a detached buffer");
        return input as ArrayBufferLike;
      }
      throw error("ERR_INVALID_ARG_TYPE", "First argument must be an ArrayBufferView");
    }
    function isUtf8(input: unknown) {
      return natives.isUtf8(viewOrBuffer(input, () => new TypeError("ArrayBufferView is detached"))!);
    }
    function isAscii(input: unknown) {
      return natives.isAscii(
        viewOrBuffer(input, () => makeError("ERR_INVALID_STATE", "Cannot validate on a detached buffer"))!,
      );
    }
    // jsBufferTranscode
    function transcode(source: unknown, fromEncoding: unknown, toEncoding: unknown) {
      if (!isUint8Array(source)) throw invalidArgInstance("source", BUFFER_OR_UINT8ARRAY, source);
      if (isDetached(source) || byteLengthOf(source) === 0) return allocate(0);
      const name = (value: unknown) => (value == null ? "" : StringCtor(value));
      const from = name(fromEncoding);
      const to = name(toEncoding);
      if (isDetached(source) || byteLengthOf(source) === 0) return allocate(0);
      const result = natives.transcode(source, from, to);
      if (typeof result !== "string") return adopt(result);
      const e: any = new Error(`Unable to transcode Buffer [${result}]`);
      e.code = result;
      e.errno = result === "U_ILLEGAL_ARGUMENT_ERROR" ? 1 : 10;
      throw e;
    }
    // Boa has no object URLs (`URL.createObjectURL`), so none resolves.
    function resolveObjectURL(url: unknown) {
      return undefined;
    }

    const module: Record<string, unknown> = {
      Buffer,
      SlowBuffer,
      Blob: G.Blob,
      File: G.File,
      kMaxLength,
      kStringMaxLength,
      constants: { MAX_LENGTH: kMaxLength, MAX_STRING_LENGTH: kStringMaxLength },
      atob: G.atob,
      btoa: G.btoa,
      transcode: named(transcode, "transcode", 3),
      resolveObjectURL: named(resolveObjectURL, "resolveObjectURL", 1),
      isAscii: named(isAscii, "isAscii", 1),
      isUtf8: named(isUtf8, "isUtf8", 1),
    };
    defineProperty(module, "INSPECT_MAX_BYTES", {
      get: () => INSPECT_MAX_BYTES,
      set(value: unknown) {
        validateNumber(value, "INSPECT_MAX_BYTES", 0);
        INSPECT_MAX_BYTES = value as number;
      },
      enumerable: true,
      configurable: false,
    });
    return module;
  };
})((globalThis as any).__bun_intrinsics_ops__);
