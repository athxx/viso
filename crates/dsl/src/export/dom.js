const color = (v) => {
  if (v === null) return "transparent";
  const n = Number(BigInt.asUintN(32, v));
  return "rgba(" + (n >>> 24) + "," + ((n >>> 16) & 255) + "," + ((n >>> 8) & 255) + "," + ((n & 255) / 255) + ")";
};
const radius = (v) => {
  const f = v === null ? [] : v.f;
  const t = (i) => Number(f[i] ?? 0);
  const parts = [[t(0), "1px"], [t(1), "var(--viso-px)"], [t(2), "0.0625rem"], [t(3), "1em"]]
    .filter(([n]) => n !== 0).map(([n, u]) => n + " * " + u);
  if (t(4) >= 50) parts.push("9999px");
  else if (t(4) > 0) parts.push(t(4) + "%");
  return parts.length ? "calc(" + parts.join(" + ") + ")" : "0";
};
// A `MixedLength` as a CSS length.
const length = (v) => {
  const f = v === null ? [] : v.f;
  const parts = [[f[0], "1px"], [f[1], "var(--viso-px)"], [f[2], "0.0625rem"], [f[3], "1em"]]
    .map(([n, u]) => [Number(n ?? 0), u]).filter(([n]) => n !== 0).map(([n, u]) => n + " * " + u);
  return parts.length ? "calc(" + parts.join(" + ") + ")" : "0px";
};
// The CSS pixels a `MixedLength` folds to at `el`; a `%` term folds to none.
const fold = (el, v) => {
  const f = v === null ? [] : v.f;
  const n = (i) => Number(f[i] ?? 0);
  const rem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
  const em = parseFloat(getComputedStyle(el).fontSize) || rem;
  return n(0) + n(1) / (window.devicePixelRatio || 1) + (n(2) * rem) / 16 + n(3) * em;
};
// The element of named node `key` a call from `from` reaches: the nearest
// around `from`, so a list item's instance reaches its own, else the page's
// first.
const named = (key, from) => {
  const sel = '[data-node="' + key + '"]';
  for (let a = from; a; a = a.parentElement) {
    if (a.matches(sel)) return a;
    const hit = a.querySelector(sel);
    if (hit) return hit;
  }
  return document.querySelector(sel);
};
// The first element of each item a list mounts carries its key as `$key`.
const row = (list, k) => {
  if (!list) return null;
  for (const c of list.children) if (c.$key === k) return c;
  return null;
};
const rows = (list) => Array.from(list.children).filter((c) => c.$key !== undefined);
const mark = (out, k) => {
  const first = Array.isArray(out) ? out.find((n) => n instanceof Element) : out;
  if (first instanceof Element) first.$key = k;
  return out;
};
// Applies the requests a transaction made of named nodes, each
// `[action, key, args]`: focus, blur, scroll into view, scroll to, scroll by,
// and a list's scroll to a row by index or key.
const reach = (requests, from) => {
  if (!requests) return;
  for (const [action, key, args] of requests) {
    const el = named(key, from);
    if (!el) continue;
    const at = (v) => [fold(el, v.f[0]), fold(el, v.f[1])];
    switch (action) {
      case 0: el.focus({ focusVisible: false }); break;
      case 1: if (el.contains(document.activeElement)) document.activeElement.blur(); break;
      case 2: el.scrollIntoView({ block: "nearest", inline: "nearest" }); break;
      case 3: { const [x, y] = at(args[0]); el.scrollTo(x, y); break; }
      case 4: { const [x, y] = at(args[0]); el.scrollBy(x, y); break; }
      case 5: {
        const all = rows(el);
        const r = all[Math.max(0, Math.min(Number(args[0]), all.length - 1))];
        if (r) r.scrollIntoView({ block: "start", inline: "start" });
        break;
      }
      case 6: { const r = row(el, $.key(args[0])); if (r) r.scrollIntoView({ block: "start", inline: "start" }); break; }
    }
  }
};
// An `Offset` as a CSS `translate`.
const offset = (v) => (v === null ? "none" : length(v.f[0]) + " " + length(v.f[1]));
const stroke = (v) => {
  if (v === null) return "none";
  const w = typeof v.f[0] === "object" ? Number(v.f[0].f[0]) : Number(v.f[0]);
  return "inset 0 0 0 " + Math.max(w, 0) + "px " + color(v.f[1]);
};
const text = (v) => (v === null ? "" : typeof v === "string" ? v : String(v));
const attr = (el, name, v) => (v === null ? el.removeAttribute(name) : el.setAttribute(name, text(v)));
// Whether input does not reach `el`: it or an ancestor is disabled.
const off = (el) => el instanceof Element && el.closest("[data-viso-off]") !== null;
// Gives a focusable `el` its own tab stop, or none while it is off.
const tab = (el) => {
  if (el.$tab === undefined) {
    if (el.tabIndex < 0) return;
    el.$tab = el.tabIndex;
  }
  el.tabIndex = off(el) ? -1 : el.$tab;
};
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
  focusable: (el, v) => { el.$tab = truth(v) ? 0 : -1; tab(el); },
  enabled(el, v) {
    const on = truth(v);
    el.toggleAttribute("data-viso-off", !on);
    attr(el, "aria-disabled", on ? null : "true");
    if ("disabled" in el) el.disabled = !on;
    tab(el);
    el.querySelectorAll("*").forEach(tab);
  },
  autofocus(el, v) {
    if (el.$autofocus) return;
    el.$autofocus = true;
    if (!truth(v)) return;
    queueMicrotask(() => {
      const at = document.activeElement;
      if ((!at || at === document.body) && !off(el)) el.focus();
    });
  },
  clip: (el, v) => { el.style.overflow = truth(v) ? "hidden" : ""; },
  label: (el, v, alt) => attr(el, alt ? "alt" : "aria-label", v),
  hint: (el, v) => attr(el, "aria-description", v),
  spoken: (el, v) => attr(el, "aria-valuetext", v),
  hidden: (el, v) => attr(el, "aria-hidden", truth(v) ? "true" : null),
  level: (el, v) => attr(el, "aria-level", v),
  checked: (el, v) => { el.checked = truth(v); },
  range: (el, v, name) => { el[name] = v === null ? "" : String(v); },
  input: (el, v) => { const s = text(v); if (el.value !== s) el.value = s; },
  placeholder: (el, v) => attr(el, "placeholder", v),
  secure: (el, v) => { el.type = truth(v) ? "password" : "text"; },
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
  vradius: (el, v, name) => el.style.setProperty(name, radius(v)),
  vborder: (el, v, name) => el.style.setProperty(name, stroke(v)),
  vtranslate: (el, v, name) => el.style.setProperty(name, offset(v)),
  vmove(el, v, name, property) {
    const f = v === null ? [] : v.f;
    const instant = reduced.matches && f[3] === 0n;
    const s = (x) => Math.max(Number(x ?? 0), 0) + "s";
    el.style.setProperty(name, property + " " + (instant ? "0s" : s(f[0])) + " " +
      (easings[Number(f[2] ?? 2n)] || "ease-out") + " " + (instant ? "0s" : s(f[1])));
  },
  cls: (el, v, name) => el.classList.toggle(name, truth(v)),
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

// Calls `fire` with the payload of each `route` event of `el`, a control
// of kind `control`. An event aimed into a disabled subtree reaches no
// listener, but a pointer still leaves what it left.
const listen = (el, route, capture, control, fire) => {
  const live = (f) => (e) => { if (route === 6 || !off(e.target)) f(e); };
  if (route <= 6) {
    el.addEventListener(DOM[route], live((e) => fire(pointer(route, e, el))), capture);
  } else if (route <= 8) {
    el.addEventListener(DOM[route], live((e) => fire(rec([key(e), bool(e.repeat), mods(e)]))), capture);
  } else if (route === 9) {
    if (control === 0) el.addEventListener("change", () => fire(rec([bool(el.checked)])), capture);
    else if (control === 1) el.addEventListener("input", () => fire(rec([Number(el.value)])), capture);
    else el.addEventListener("input", () => fire(rec([el.value])), capture);
  } else if (route === 10) {
    el.addEventListener("keydown", live((e) => {
      if (e.key === "Enter" && !e.repeat && !e.isComposing) fire(null);
    }), capture);
  } else if (route === 12) {
    const len = (v) => rec([v, 0, 0, 0, 0]);
    el.addEventListener("scroll", () => fire(rec([
      rec([len(el.scrollLeft), len(el.scrollTop)]),
      rec([el.clientWidth, el.clientHeight]),
      rec([el.scrollWidth, el.scrollHeight]),
    ])), { passive: true, capture });
  } else if (route === 13) {
    let seen = "";
    const report = () => {
      const all = rows(el);
      if (!all.length || !el.clientHeight) return;
      const across = el.classList.contains("vx-across");
      const box = el.getBoundingClientRect();
      const start = across ? box.left : box.top;
      const end = start + (across ? el.clientWidth : el.clientHeight);
      const lo = (r) => { const b = r.getBoundingClientRect(); return across ? b.left : b.top; };
      const hi = (r) => { const b = r.getBoundingClientRect(); return across ? b.right : b.bottom; };
      // The first row ending past the start edge, the last starting before
      // the far one: rows run in order, so a search finds each.
      const find = (test) => {
        let a = 0, b = all.length;
        while (a < b) { const m = (a + b) >> 1; if (test(all[m])) b = m; else a = m + 1; }
        return a;
      };
      const first = Math.min(find((r) => hi(r) > start), all.length - 1);
      const last = Math.max(find((r) => lo(r) >= end) - 1, first);
      const now = first + ":" + last + ":" + all.length;
      if (now === seen) return;
      seen = now;
      fire(rec([BigInt(first), BigInt(last), BigInt(all.length)]));
    };
    el.addEventListener("scroll", report, { passive: true, capture });
    new ResizeObserver(report).observe(el);
    new MutationObserver(report).observe(el, { childList: true });
  } else {
    const current = () => items(el).findIndex((c) => c.getAttribute(el.$radio ? "aria-checked" : "aria-selected") === "true");
    const choose = (i) => {
      if (i < 0 || i === current()) return;
      fire(rec([BigInt(i)]));
      const next = items(el)[current()];
      if (next) next.focus();
    };
    el.addEventListener("click", live((e) => {
      let c = e.target;
      while (c && c.parentElement !== el) c = c.parentElement;
      if (c) choose(items(el).indexOf(c));
    }), capture);
    el.addEventListener("keydown", live((e) => {
      const n = items(el).length;
      const i = current();
      const to = { ArrowLeft: i - 1, ArrowUp: i - 1, ArrowRight: i + 1, ArrowDown: i + 1, Home: 0, End: n - 1 }[e.key];
      if (to === undefined || n === 0) return;
      e.preventDefault();
      choose(Math.min(Math.max(to, 0), n - 1));
    }), capture);
  }
};

const sync = (el, control, value) => {
  const v = value ? value() : null;
  if (control === 0) el.checked = truth(v);
  else if (control === 1) el.value = v === null ? el.min : String(v);
  else if (control === 2) select(el, v === null ? 0n : v, el.$radio);
  else if (v !== null) P.input(el, v);
};

