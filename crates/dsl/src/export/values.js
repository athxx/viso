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
  T.numbers ??= new Intl.NumberFormat(T.R || T.L || undefined, { maximumFractionDigits: 20 });
  return T.numbers.format(typeof v === "bigint" ? v : $.f64(v));
};
// The three fields of a `Date` or a `Time`, or of a `DateTime`'s date or,
// for `time`, its time, as numbers.
const civil = (v, time) => {
  if (!v || !v.f) return null;
  if (v.f.length === 2 && v.f[0] && v.f[0].f) return civil(v.f[time ? 1 : 0], time);
  return v.f.length === 3 ? v.f.map(Number) : null;
};
// A date or time of day in the reader's language: a calendar instant at
// UTC formatted at UTC, so no time zone moves it.
const civilFormat = (T, key, options, at) => {
  T.dates ??= new Map();
  let format = T.dates.get(key);
  if (!format) {
    format = new Intl.DateTimeFormat(T.R || T.L || undefined, { ...options, timeZone: "UTC" });
    T.dates.set(key, format);
  }
  return format.format(at);
};
// Formats the pattern `ops` with the arguments `a`, its plurals by the
// rules of the language `lang` it is written in.
const pattern = (T, ops, a, pound, out, lang) => {
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
          T.plurals ??= new Map();
          const key = lang + (op[2] ? "/o" : "/c");
          let rules = T.plurals.get(key);
          if (!rules) {
            rules = new Intl.PluralRules(lang || undefined, { type: op[2] ? "ordinal" : "cardinal" });
            T.plurals.set(key, rules);
          }
          body = op[5][rules.select(Number(shown))] || op[6];
        }
        pattern(T, body, a, shown, out, lang);
        break;
      }
      case 5: {
        const d = civil(a[op[1]], false);
        if (!d) throw fault("a `date` argument is a `Date` or a `DateTime`");
        const at = new Date(Date.UTC(2000, d[1] - 1, d[2]));
        at.setUTCFullYear(d[0]);
        if (Number.isNaN(at.getTime()) || at.getUTCMonth() !== d[1] - 1) throw fault(d.join("-") + " is no date");
        out.push(civilFormat(T, "d" + op[2], { dateStyle: op[2] }, at));
        break;
      }
      case 6: {
        const t = civil(a[op[1]], true);
        if (!t || t[0] > 23 || t[1] > 59 || t[2] > 59 || t.some((x) => x < 0)) throw fault("a `time` argument is a time of day");
        const at = Date.UTC(1970, 0, 1, t[0], t[1], t[2]);
        out.push(civilFormat(T, "t" + op[2], { timeStyle: op[2] ? "medium" : "short" }, at));
        break;
      }
      case 7: {
        const v = a[op[1]];
        if (typeof v === "bigint") out.push(number(T, v));
        else {
          // Half to even, as ICU's `integer` style rounds.
          const f = Math.floor(v), r = v - f;
          out.push(number(T, r > 0.5 || (r === 0.5 && f % 2 !== 0) ? f + 1 : f));
        }
        break;
      }
      default: {
        const v = a[op[1]];
        if (typeof v !== "string") throw fault("a `select` argument is text");
        const hit = op[2].find((c) => c[0] === v);
        pattern(T, hit ? hit[1] : op[3], a, pound, out, lang);
      }
    }
  }
};
// The index of the locale of `tags` nearest the first of `wanted` that has
// one: the tag itself, then each shorter form of it and of its likely
// script and region, as a fallback chain runs; the source's, 0, when none.
const nearest = (tags, wanted) => {
  const lower = tags.map((t) => t.toLowerCase());
  for (const w of wanted) {
    const chain = [];
    const cut = (tag) => {
      for (let t = tag; t; t = t.includes("-") ? t.slice(0, t.lastIndexOf("-")) : "") chain.push(t.toLowerCase());
    };
    cut(w);
    try {
      const max = new Intl.Locale(w).maximize();
      if (max.script) cut(max.language + "-" + max.script);
    } catch (_) {}
    for (const t of chain) {
      const at = lower.indexOf(t);
      if (at >= 0) return at;
    }
  }
  return 0;
};
// Has the messages `T` speak the reader's language as near as they can,
// and write numbers as the reader's language `R` does.
const settle = (T) => {
  const nav = typeof navigator === "undefined" ? null : navigator;
  const wanted = !nav ? [] : nav.languages && nav.languages.length ? nav.languages : [nav.language || ""];
  const at = nearest(T.Ls, wanted);
  T.L = T.Ls[at];
  T.M = T.Ms[at];
  T.R = wanted[0] || T.L;
};
// Message `m` of the messages `T`, by id or key, with the arguments `a`.
const translate = (T, m, a) => {
  const id = typeof m === "string" ? T.MK.get(m) : Number(m);
  if (id === undefined) return m;
  const p = T.M[id];
  if (p === undefined) throw fault("message " + m + " is not in the catalog");
  if (typeof p === "string") return p;
  const out = [];
  // A pattern the locale leaves to its fallback is `{o, p}`: written in
  // locale `o`, whose plural rules it follows.
  if (Array.isArray(p)) pattern(T, p, a, null, out, T.L);
  else pattern(T, p.p, a, null, out, T.Ls[p.o]);
  return out.join("");
};
