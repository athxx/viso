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
let root = null;
let top = null;
let pending = [];
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
  emit(event, args) {
    pending.push([event, args]);
  },
  tr(m, a) {
    const id = typeof m === "string" ? MK.get(m) : Number(m);
    if (id === undefined) return m;
    const p = M[id];
    if (p === undefined) throw fault("message " + m + " is not in the catalog");
    if (typeof p === "string") return p;
    const out = [];
    pattern(p, a, null, out);
    return out.join("");
  },
};

let numbers = null;
const plurals = [null, null];
const number = (v) => {
  if (typeof v !== "bigint" && !Number.isFinite(v)) throw fault(v + " is not a finite number");
  numbers ??= new Intl.NumberFormat(L || undefined, { maximumFractionDigits: 20 });
  return numbers.format(typeof v === "bigint" ? v : $.f64(v));
};
const pattern = (ops, a, pound, out) => {
  for (const op of ops) {
    if (typeof op === "string") {
      out.push(op);
      continue;
    }
    switch (op[0]) {
      case 0: {
        const v = a[op[1]];
        out.push(typeof v === "string" ? v : number(v));
        break;
      }
      case 1: out.push(number(a[op[1]])); break;
      case 2: out.push(pound === null ? "#" : number(pound)); break;
      case 3: {
        const v = a[op[1]];
        if (typeof v !== "bigint" && typeof v !== "number") throw fault("a `plural` argument is a number");
        const exact = Number(v);
        const shown = op[3] === 0 ? v : exact - op[3];
        const hit = op[4].find((c) => c[0] === exact);
        let body = hit && hit[1];
        if (!body) {
          plurals[op[2]] ??= new Intl.PluralRules(L || undefined, { type: op[2] ? "ordinal" : "cardinal" });
          body = op[5][plurals[op[2]].select(Number(shown))] || op[6];
        }
        pattern(body, a, shown, out);
        break;
      }
      default: {
        const v = a[op[1]];
        if (typeof v !== "string") throw fault("a `select` argument is text");
        const hit = op[2].find((c) => c[0] === v);
        pattern(hit ? hit[1] : op[3], a, pound, out);
      }
    }
  }
};

const color = (v) => {
  if (v === null) return "transparent";
  const n = Number(BigInt.asUintN(32, v));
  return "rgba(" + (n >>> 24) + "," + ((n >>> 16) & 255) + "," + ((n >>> 8) & 255) + "," + ((n & 255) / 255) + ")";
};
const text = (v) => (v === null ? "" : typeof v === "string" ? v : String(v));
const attr = (el, name, v) => (v === null ? el.removeAttribute(name) : el.setAttribute(name, text(v)));
const fits = ["contain", "cover", "fill"];
const easings = ["linear", "ease-in", "ease-out", "ease-in-out"];
const reduced = matchMedia("(prefers-reduced-motion: reduce)");
const items = (el) => Array.from(el.children).filter((c) => c.tagName !== "TEMPLATE");
const select = (el, v, radio) => {
  const k = radio ? "aria-checked" : "aria-selected";
  items(el).forEach((c, i) => {
    const on = BigInt(i) === v;
    c.setAttribute(k, on ? "true" : "false");
    c.tabIndex = on ? 0 : -1;
  });
};
const url = (v) => {
  if (v === null || typeof v !== "object") return "";
  return A.get(String(BigInt.asUintN(64, v.f[0]))) || "";
};
const P = {
  text: (el, v) => { el.textContent = text(v); },
  color: (el, v) => { el.style.color = color(v); },
  wrap: (el, v) => { el.style.whiteSpace = truth(v) ? "pre-wrap" : ""; },
  visible: (el, v) => { el.style.display = truth(v) ? "" : "none"; },
  focusable: (el, v) => { el.tabIndex = truth(v) ? 0 : -1; },
  label: (el, v, alt) => attr(el, alt ? "alt" : "aria-label", v),
  hint: (el, v) => attr(el, "aria-description", v),
  spoken: (el, v) => attr(el, "aria-valuetext", v),
  hidden: (el, v) => attr(el, "aria-hidden", truth(v) ? "true" : null),
  level: (el, v) => attr(el, "aria-level", v),
  checked: (el, v) => { el.checked = truth(v); },
  range: (el, v, name) => { el[name] = v === null ? "" : String(v); },
  input: (el, v) => { const s = text(v); if (el.value !== s) el.value = s; },
  selected: (el, v, radio) => select(el, v === null ? 0n : v, radio),
  source(el, v, mask) {
    const u = url(v);
    if (!mask) el.src = u;
    else el.style.maskImage = el.style.webkitMaskImage = u ? 'url("' + u + '")' : "none";
  },
  fit(el, v, mask) {
    const fit = fits[Number(v ?? 0n)] || "contain";
    if (!mask) el.style.objectFit = fit;
    else el.style.maskSize = el.style.webkitMaskSize = fit === "fill" ? "100% 100%" : fit;
  },
  vcolor: (el, v, name) => el.style.setProperty(name, color(v)),
  vopacity: (el, v, name) => el.style.setProperty(name, String(Math.min(Math.max(v ?? 1, 0), 1))),
  vmove(el, v, name, property) {
    const f = v === null ? [] : v.f;
    const instant = reduced.matches && f[3] === 0n;
    const s = (x) => Math.max(Number(x ?? 0), 0) + "s";
    el.style.setProperty(name, property + " " + (instant ? "0s" : s(f[0])) + " " +
      (easings[Number(f[2] ?? 2n)] || "ease-out") + " " + (instant ? "0s" : s(f[1])));
  },
  cls: (el, v, name) => el.classList.toggle(name, truth(v)),
};

const hit = (deps, changed) => changed === null || deps === null || deps.some((d) => changed.has(d));
const refresh = (m, changed) => {
  for (const [deps, f] of m.u) {
    if (hit(deps, changed)) {
      try { f(); } catch (e) { console.error(e); }
    }
  }
  for (const r of m.r) {
    try { r.update(changed); } catch (e) { console.error(e); }
  }
};

const run = (handler) => {
  const before = S;
  W = S.slice();
  pending = [];
  try {
    handler();
  } catch (e) {
    W = S;
    pending = [];
    console.error(e);
    return;
  }
  S = W;
  const changed = new Set();
  for (let i = 0; i < S.length; i++) if (!Object.is(S[i], before[i])) changed.add(i);
  const events = pending;
  pending = [];
  if (changed.size) refresh(top, changed);
  for (const [event, args] of events) {
    root.dispatchEvent(new CustomEvent(E[event], { detail: args, bubbles: true }));
  }
};

const point = (e, el) => {
  const r = el.getBoundingClientRect();
  return rec([e.clientX - r.left, e.clientY - r.top]);
};
const mods = (e) => rec([bool(e.shiftKey), bool(e.ctrlKey), bool(e.altKey), bool(e.metaKey)]);
const kinds = { mouse: 0n, touch: 1n, pen: 2n };
const kind = (e) => kinds[e.pointerType] ?? 0n;
const buttons = (e) => rec([bool(e.buttons & 1), bool(e.buttons & 2), bool(e.buttons & 4)]);
const button = (e) => [0n, 2n, 1n][e.button] ?? 0n;
const keys = {
  Enter: 1n, Escape: 2n, Tab: 3n, Backspace: 4n, Delete: 5n, " ": 6n, ArrowUp: 7n,
  ArrowDown: 8n, ArrowLeft: 9n, ArrowRight: 10n, Home: 11n, End: 12n, PageUp: 13n, PageDown: 14n,
};
const key = (e) => {
  if (e.key in keys) return keys[e.key];
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(e.key)) return { t: 15, f: [BigInt(e.key.slice(1))] };
  if ([...e.key].length === 1) return { t: 0, f: [BigInt(e.key.codePointAt(0))] };
  return 16n;
};
const pointer = (route, e, el) => {
  switch (route) {
    case 0: return rec([point(e, el), mods(e)]);
    case 1: case 5: case 6: return rec([point(e, el), kind(e)]);
    default: return rec([point(e, el), button(e), buttons(e), kind(e), mods(e)]);
  }
};
const DOM = ["click", "click", "pointerdown", "pointermove", "pointerup", "pointerenter", "pointerleave", "keydown", "keyup"];

const on = (el, route, capture, control, handler) => {
  const fire = (payload) => {
    run(() => handler(payload));
    if (el.$sync) el.$sync();
  };
  if (route <= 6) {
    el.addEventListener(DOM[route], (e) => fire(pointer(route, e, el)), capture);
  } else if (route <= 8) {
    el.addEventListener(DOM[route], (e) => fire(rec([key(e), bool(e.repeat), mods(e)])), capture);
  } else if (route === 9) {
    if (control === 0) el.addEventListener("change", () => fire(rec([bool(el.checked)])), capture);
    else if (control === 1) el.addEventListener("input", () => fire(rec([Number(el.value)])), capture);
    else el.addEventListener("input", () => fire(rec([el.value])), capture);
  } else if (route === 10) {
    el.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && !e.repeat && !e.isComposing) fire(null);
    }, capture);
  } else {
    const current = () => items(el).findIndex((c) => c.getAttribute(el.$radio ? "aria-checked" : "aria-selected") === "true");
    const choose = (i) => {
      if (i < 0 || i === current()) return;
      fire(rec([BigInt(i)]));
      const next = items(el)[current()];
      if (next) next.focus();
    };
    el.addEventListener("click", (e) => {
      let c = e.target;
      while (c && c.parentElement !== el) c = c.parentElement;
      if (c) choose(items(el).indexOf(c));
    }, capture);
    el.addEventListener("keydown", (e) => {
      const n = items(el).length;
      const i = current();
      const to = { ArrowLeft: i - 1, ArrowUp: i - 1, ArrowRight: i + 1, ArrowDown: i + 1, Home: 0, End: n - 1 }[e.key];
      if (to === undefined || n === 0) return;
      e.preventDefault();
      choose(Math.min(Math.max(to, 0), n - 1));
    }, capture);
  }
};

const sync = (el, control, value) => {
  const v = value ? value() : null;
  if (control === 0) el.checked = truth(v);
  else if (control === 1) el.value = v === null ? el.min : String(v);
  else if (control === 2) select(el, v === null ? 0n : v, el.$radio);
  else if (v !== null) P.input(el, v);
};

const clone = (r, arm, scope) => {
  const frag = r.anchor.content.children[arm].content.cloneNode(true);
  const nodes = Array.from(frag.childNodes);
  const m = r.g.arms[arm](Array.from(frag.querySelectorAll("[data-v]")), scope);
  refresh(m, null);
  return { m, nodes, scope, key: null };
};
const place = (r, c) => {
  for (const n of c.nodes) r.anchor.parentNode.insertBefore(n, r.anchor);
};
const drop = (c) => {
  for (const n of c.nodes) n.remove();
};
const choice = (r, changed) => {
  const g = r.g;
  if (hit(g.deps, changed)) {
    const value = g.k === 1 ? g.scr(r.sc) : null;
    const arm = Number(g.k === 1 ? g.sel(r.sc, value) : g.sel(r.sc));
    if (arm !== r.arm) {
      if (r.c) {
        drop(r.c);
        if (g.keep[r.arm]) r.kept.set(r.arm, r.c);
      }
      r.arm = arm;
      r.c = null;
      if (arm < 0) return;
      let c = r.kept.get(arm);
      if (c) {
        r.kept.delete(arm);
        refresh(c.m, null);
      } else {
        c = clone(r, arm, g.k === 1 ? [...r.sc, value] : r.sc);
      }
      place(r, c);
      r.c = c;
      return;
    }
    if (g.k === 1 && r.c && !$.eq(r.c.scope[r.c.scope.length - 1], value)) {
      r.c.scope[r.c.scope.length - 1] = value;
      refresh(r.c.m, null);
      return;
    }
  }
  if (r.c) refresh(r.c.m, changed);
};
const repeat = (r, changed) => {
  const g = r.g;
  if (r.items && !hit(g.deps, changed)) {
    for (const it of r.items) refresh(it.m, changed);
    return;
  }
  let list = g.items(r.sc);
  if (!Array.isArray(list)) {
    const lo = list.f[0];
    const hi = list.t === 1 ? list.f[1] + 1n : list.f[1];
    if (hi - lo > MAX_ITEMS) throw fault("a `for` region mounts at most " + MAX_ITEMS + " items");
    list = [];
    for (let i = lo; i < hi; i++) list.push(i);
  }
  const ks = list.map((v, i) => (g.key ? $.key(g.key(r.sc, v)) : "#" + i));
  if (new Set(ks).size !== ks.length) throw fault("two items of a `for` region share a key");
  const old = new Map((r.items || []).map((it) => [it.key, it]));
  const next = list.map((v, i) => {
    let it = old.get(ks[i]);
    if (it) {
      old.delete(ks[i]);
      const s = it.scope;
      if (!$.eq(s[s.length - 1], v)) {
        s[s.length - 1] = v;
        refresh(it.m, null);
      } else {
        refresh(it.m, changed);
      }
    } else {
      it = clone(r, 0, [...r.sc, v]);
      it.key = ks[i];
    }
    return it;
  });
  for (const it of old.values()) drop(it);
  for (const it of next) place(r, it);
  r.items = next;
};
const region = (anchor, g, sc) => {
  const r = { anchor, g, sc, arm: -1, c: null, kept: new Map(), items: null };
  r.update = (changed) => (g.k === 2 ? repeat(r, changed) : choice(r, changed));
  return r;
};

const start = (mount) => {
  root = document.getElementById("viso-root");
  top = mount(Array.from(document.querySelectorAll("[data-v]")), []);
  refresh(top, null);
};
