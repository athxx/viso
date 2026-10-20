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
const stroke = (v) => {
  if (v === null) return "none";
  const w = typeof v.f[0] === "object" ? Number(v.f[0].f[0]) : Number(v.f[0]);
  return "inset 0 0 0 " + Math.max(w, 0) + "px " + color(v.f[1]);
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
// of kind `control`.
const listen = (el, route, capture, control, fire) => {
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

