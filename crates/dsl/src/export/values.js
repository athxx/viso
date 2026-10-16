const fault = (m) => new Error("viso: " + m);
const LIM = {
  i8: [-128n, 127n], i16: [-32768n, 32767n], i32: [-2147483648n, 2147483647n],
  i64: [-(2n ** 63n), 2n ** 63n - 1n], u8: [0n, 255n], u16: [0n, 65535n],
  u32: [0n, 4294967295n], u64: [0n, 2n ** 64n - 1n],
};
const BITS = { i8: 8, i16: 16, i32: 32, i64: 64, u8: 8, u16: 16, u32: 32, u64: 64 };
const MAX_ITEMS = 1n << 20n;
const plain = (s) => {
  const at = s.indexOf("e");
  if (at < 0) return s;
  const neg = s[0] === "-";
  const [whole, part = ""] = s.slice(neg ? 1 : 0, at).split(".");
  const digits = whole + part;
  const point = whole.length + Number(s.slice(at + 1));
  const out = point <= 0 ? "0." + "0".repeat(-point) + digits
    : point >= digits.length ? digits + "0".repeat(point - digits.length)
    : digits.slice(0, point) + "." + digits.slice(point);
  return (neg ? "-" : "") + out;
};
const rec = (f) => ({ t: 0, f });
const bool = (b) => (b ? 1n : 0n);
const truth = (v) => v !== null && v !== 0n;
const $ = {
  ck(t, v) {
    const l = LIM[t];
    if (v < l[0] || v > l[1]) throw fault("an integer overflowed");
    return t === "u64" ? BigInt.asIntN(64, v) : v;
  },
  wr(t, v) {
    const b = BITS[t];
    const w = t[0] === "i" ? BigInt.asIntN(b, v) : BigInt.asUintN(b, v);
    return t === "u64" ? BigInt.asIntN(64, w) : w;
  },
  u: (v) => BigInt.asUintN(64, v),
  div(a, b) {
    if (b === 0n) throw fault("a division by zero");
    return a / b;
  },
  rem(t, a, b) {
    if (b === 0n) throw fault("a division by zero");
    $.ck(t, a / b);
    return a % b;
  },
  sh(t, b) {
    if (b < 0n || b >= BigInt(BITS[t])) throw fault("a shift out of range");
    return b;
  },
  sat(t, x) {
    if (Number.isNaN(x)) return 0n;
    const l = LIM[t];
    let v = x === Infinity ? l[1] : x === -Infinity ? l[0] : BigInt(Math.trunc(x));
    if (v < l[0]) v = l[0];
    if (v > l[1]) v = l[1];
    return t === "u64" ? BigInt.asIntN(64, v) : v;
  },
  eq(a, b) {
    if (a === b) return true;
    if (a === null || b === null || typeof a !== "object" || typeof b !== "object") return false;
    const x = Array.isArray(a) ? a : a.f;
    const y = Array.isArray(b) ? b : b.f;
    if (Array.isArray(a) !== Array.isArray(b) || a.t !== b.t || x.length !== y.length) return false;
    for (let i = 0; i < x.length; i++) if (!$.eq(x[i], y[i])) return false;
    return true;
  },
  tag: (v) => (typeof v === "bigint" ? v : BigInt(v.t)),
  ix(l, i) {
    if (i < 0n || i >= BigInt(l.length)) throw fault("an index out of bounds");
    return Number(i);
  },
  at: (l, i) => l[$.ix(l, i)],
  push(l, v) {
    const c = l.slice();
    c.push(v);
    return c;
  },
  insert(l, i, v) {
    if (i < 0n || i > BigInt(l.length)) throw fault("an index out of bounds");
    const c = l.slice();
    c.splice(Number(i), 0, v);
    return c;
  },
  remove(l, i) {
    const c = l.slice();
    const [v] = c.splice($.ix(l, i), 1);
    return [c, v];
  },
  trunc: (l, n) => (n < BigInt(l.length) ? l.slice(0, n < 0n ? 0 : Number(n)) : l),
  set(v, path, to, at = 0) {
    if (at === path.length) return to;
    const [kind, key] = path[at];
    if (kind === 0) {
      const c = { t: v.t, f: v.f.slice() };
      c.f[key] = $.set(v.f[key], path, to, at + 1);
      return c;
    }
    const i = $.ix(v, key);
    const c = v.slice();
    c[i] = $.set(v[i], path, to, at + 1);
    return c;
  },
  bind: (f, c) => (...a) => f(...a, ...c),
  f64(x) {
    if (Number.isNaN(x)) return "NaN";
    if (x === Infinity) return "inf";
    if (x === -Infinity) return "-inf";
    if (Object.is(x, -0)) return "-0";
    return plain(String(x));
  },
  f32(x) {
    if (!Number.isFinite(x) || x === 0) return $.f64(x);
    for (let p = 1; p <= 9; p++) {
      const n = Number(x.toPrecision(p));
      if (Math.fround(n) === x) return plain(String(n));
    }
    return plain(String(x));
  },
  show(kind, v) {
    switch (kind) {
      case "bool": return v !== 0n ? "true" : "false";
      case "signed": return String(v);
      case "unsigned": return String(BigInt.asUintN(64, v));
      case "f32": return $.f32(v);
      case "f64": return $.f64(v);
      case "char": return String.fromCodePoint(Number(v));
      default: return v;
    }
  },
  key(v) {
    if (v === null) return "n";
    if (typeof v === "bigint") return "i" + v;
    if (typeof v === "string") return "s" + JSON.stringify(v);
    if (typeof v === "object" && !Array.isArray(v)) return "a" + v.t + "(" + v.f.map($.key).join(",") + ")";
    throw fault("a `for` key is a float, a list, a closure or a handle, which has no stable identity");
  },
  m: {
    sqrt: Math.sqrt, abs: Math.abs, floor: Math.floor,
    round: (x) => (x < 0 ? -Math.round(-x) : Math.round(x)),
    min: (a, b) => (Number.isNaN(a) ? b : Number.isNaN(b) ? a : Math.min(a, b)),
    max: (a, b) => (Number.isNaN(a) ? b : Number.isNaN(b) ? a : Math.max(a, b)),
    clamp(x, lo, hi) {
      if (!(lo <= hi)) throw fault("the range " + lo + "..=" + hi + " is empty");
      return Math.min(Math.max(x, lo), hi);
    },
    sin: Math.sin, cos: Math.cos, tan: Math.tan, atan: Math.atan, atan2: Math.atan2,
    exp: Math.exp, log: Math.log, pow: Math.pow, hypot: Math.hypot,
  },
};

const number = (T, v) => {
  if (typeof v !== "bigint" && !Number.isFinite(v)) throw fault(v + " is not a finite number");
  T.numbers ??= new Intl.NumberFormat(T.L || undefined, { maximumFractionDigits: 20 });
  return T.numbers.format(typeof v === "bigint" ? v : $.f64(v));
};
const pattern = (T, ops, a, pound, out) => {
  for (const op of ops) {
    if (typeof op === "string") {
      out.push(op);
      continue;
    }
    switch (op[0]) {
      case 0: {
        const v = a[op[1]];
        out.push(typeof v === "string" ? v : number(T, v));
        break;
      }
      case 1: out.push(number(T, a[op[1]])); break;
      case 2: out.push(pound === null ? "#" : number(T, pound)); break;
      case 3: {
        const v = a[op[1]];
        if (typeof v !== "bigint" && typeof v !== "number") throw fault("a `plural` argument is a number");
        const exact = Number(v);
        const shown = op[3] === 0 ? v : exact - op[3];
        const hit = op[4].find((c) => c[0] === exact);
        let body = hit && hit[1];
        if (!body) {
          T.plurals ??= [null, null];
          T.plurals[op[2]] ??= new Intl.PluralRules(T.L || undefined, { type: op[2] ? "ordinal" : "cardinal" });
          body = op[5][T.plurals[op[2]].select(Number(shown))] || op[6];
        }
        pattern(T, body, a, shown, out);
        break;
      }
      default: {
        const v = a[op[1]];
        if (typeof v !== "string") throw fault("a `select` argument is text");
        const hit = op[2].find((c) => c[0] === v);
        pattern(T, hit ? hit[1] : op[3], a, pound, out);
      }
    }
  }
};
// Message `m` of the messages `T`, by id or key, with the arguments `a`.
const translate = (T, m, a) => {
  const id = typeof m === "string" ? T.MK.get(m) : Number(m);
  if (id === undefined) return m;
  const p = T.M[id];
  if (p === undefined) throw fault("message " + m + " is not in the catalog");
  if (typeof p === "string") return p;
  const out = [];
  pattern(T, p, a, null, out);
  return out.join("");
};
