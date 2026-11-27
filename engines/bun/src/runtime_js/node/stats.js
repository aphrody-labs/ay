// fs.Stats, fs.BigIntStats and fs.Dirent: src/jsc/bindings/NodeFSStatBinding.cpp and NodeDirent.cpp, in JavaScript.
// Evaluated once per realm by stat.rs; the result is `{ Stats, BigIntStats, Dirent, stats, bigIntStats, dirent }`,
// the constructors and what Rust makes an instance with.
(function () {
  "use strict";
  const S_IFMT = 0o170000;
  const kinds = {
    isBlockDevice: 0o060000,
    isCharacterDevice: 0o020000,
    isDirectory: 0o040000,
    isFIFO: 0o010000,
    isFile: 0o100000,
    isSocket: 0o140000,
    isSymbolicLink: 0o120000,
  };
  const isObject = value => (typeof value === "object" && value !== null) || typeof value === "function";
  const define = (target, key, value) =>
    Object.defineProperty(target, key, { value, writable: true, enumerable: false, configurable: true });
  const defineToStringTag = (prototype, name) =>
    Object.defineProperty(prototype, Symbol.toStringTag, { value: name, configurable: true });

  function makeStats(name, isBigInt) {
    // `new Stats(dev, mode, nlink, uid, gid, rdev, blksize, ino, size, blocks, atime, mtime, ctime, birthtime)`, in
    // milliseconds, or in nanoseconds for BigIntStats, whose `*Ms` are numbers then. Without `new` it makes one too.
    const Stats = function (dev, mode, nlink, uid, gid, rdev, blksize, ino, size, blocks, atime, mtime, ctime, birthtime) {
      const self = new.target ? this : Object.create(Stats.prototype);
      Object.assign(self, { dev, mode, nlink, uid, gid, rdev, blksize, ino, size, blocks });
      const ms = isBigInt ? t => Number(BigInt.asIntN(64, BigInt(t))) / 1e6 : t => t;
      self.atimeMs = ms(atime);
      self.mtimeMs = ms(mtime);
      self.ctimeMs = ms(ctime);
      self.birthtimeMs = ms(birthtime);
      if (isBigInt) Object.assign(self, { atimeNs: atime, mtimeNs: mtime, ctimeNs: ctime, birthtimeNs: birthtime });
      return self;
    };
    Object.defineProperty(Stats, "name", { value: name });
    Object.defineProperty(Stats, "length", { value: 0 });
    Object.defineProperty(Stats, "prototype", { writable: false });
    const prototype = Stats.prototype;
    for (const [method, kind] of Object.entries(kinds))
      define(prototype, method, {
        [method]() {
          if (!isObject(this)) return undefined;
          const mode = isBigInt ? Number(BigInt.asIntN(64, BigInt(this.mode))) : this.mode | 0;
          return (mode & S_IFMT) === kind;
        },
      }[method]);
    for (const field of ["atime", "mtime", "ctime", "birthtime"]) {
      const msKey = field + "Ms";
      Object.defineProperty(prototype, field, {
        get() {
          if (!isObject(this)) return undefined;
          const date = new Date(Number(this[msKey]));
          if (this !== prototype) Object.defineProperty(this, field, { value: date, writable: true, enumerable: true, configurable: true });
          return date;
        },
        set(value) {
          if (!isObject(this)) return;
          Object.defineProperty(this, field, { value, writable: true, enumerable: true, configurable: true });
        },
        enumerable: false,
        configurable: true,
      });
    }
    defineToStringTag(prototype, name);
    return Stats;
  }

  const Stats = makeStats("Stats", false);
  const BigIntStats = makeStats("BigIntStats", true);

  const kType = Symbol("kType");
  const DirentTypes = {
    isBlockDevice: 7,
    isCharacterDevice: 6,
    isDirectory: 2,
    isFIFO: 4,
    isFile: 1,
    isSocket: 5,
    isSymbolicLink: 3,
  };
  function Dirent(name, type, path) {
    if (!new.target) {
      const error = new TypeError("Class constructor Dirent cannot be invoked without 'new'");
      error.code = "ERR_ILLEGAL_CONSTRUCTOR";
      throw error;
    }
    this.name = name;
    this.path = path;
    Object.defineProperty(this, kType, { value: type, writable: true, enumerable: false, configurable: true });
    this.parentPath = path;
  }
  Object.defineProperty(Dirent, "length", { value: 0 });
  Object.defineProperty(Dirent, "prototype", { writable: false });
  for (const [method, type] of Object.entries(DirentTypes))
    define(Dirent.prototype, method, {
      [method]() {
        const value = this?.[kType];
        return Number.isInteger(value) && (value | 0) === type;
      },
    }[method]);
  defineToStringTag(Dirent.prototype, "Dirent");

  // What a stat call returns, in the order of the structure C++ makes it with: `Bun__createJSStatsObject`.
  const stats = (dev, ino, mode, nlink, uid, gid, rdev, size, blksize, blocks, atimeMs, mtimeMs, ctimeMs, birthtimeMs) => {
    const self = Object.create(Stats.prototype);
    return Object.assign(self, {
      dev, ino, mode, nlink, uid, gid, rdev, size, blksize, blocks, atimeMs, mtimeMs, ctimeMs, birthtimeMs,
    });
  };
  // `Bun__createJSBigIntStatsObject`: the times in nanoseconds, `*Ms` in whole milliseconds.
  const bigIntStats = (dev, ino, mode, nlink, uid, gid, rdev, size, blksize, blocks, atimeNs, mtimeNs, ctimeNs, birthtimeNs) => {
    const self = Object.create(BigIntStats.prototype);
    const ms = ns => ns / 1000000n;
    return Object.assign(self, {
      dev, ino, mode, nlink, uid, gid, rdev, size, blksize, blocks,
      atimeMs: ms(atimeNs), mtimeMs: ms(mtimeNs), ctimeMs: ms(ctimeNs), birthtimeMs: ms(birthtimeNs),
      atimeNs, mtimeNs, ctimeNs, birthtimeNs,
    });
  };
  const dirent = (name, type, path) => new Dirent(name, type, path);
  return { Stats, BigIntStats, Dirent, stats, bigIntStats, dirent };
})();
