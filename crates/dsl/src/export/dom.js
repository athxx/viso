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
// The animations `animate` started that run, by id, and the last id given.
const animations = new Map();
let lastAnimation = 0n;
// Queues the request `[action, key, args]` in `queue`, answering what the
// call returns: an `animate` the id of the `Animation` it starts, which rides
// along as a third argument; an `is_running` whether the animation `key`
// runs, as the requests queued so far leave it.
const request = (queue, action, key, args) => {
  if (action === 10) {
    for (let i = queue.length - 1; i >= 0; i--) {
      const [a, k, g] = queue[i];
      if (a === 7 && g[2] === key) return 1n;
      if ((a === 8 || a === 9) && k === key) return 0n;
    }
    return bool(animations.has(key));
  }
  if (action === 7) {
    const id = ++lastAnimation;
    queue.push([action, key, [args[0], args[1], id]]);
    return id;
  }
  queue.push([action, key, args]);
  return 0n;
};
// The CSS property and value an `Animate` moves toward.
const animated = (v) => {
  switch (v.t) {
    case 0: return ["translate", offset(v.f[0])];
    case 1: return ["scale", scaling(v.f[0])];
    case 2: return ["rotate", turning(v.f[0])];
    default: return ["opacity", String(Math.min(Math.max(v.f[0] ?? 1, 0), 1))];
  }
};
// Starts the animation `id` of `el` toward `target` over `spec`: the
// presented value moves there and the styled one shows again once it ends,
// which `el` hears as an `animation_end`. A newer one of the same property
// cancels it.
const animate = (el, target, spec, id) => {
  const [property, value] = animated(target);
  const f = spec === null ? [] : spec.f;
  const instant = reduced.matches && f[3] === 0n;
  const ms = (x) => (instant ? 0 : Math.max(Number(x ?? 0), 0) * 1000);
  const running = (el.$anims ??= {});
  if (running[property]) running[property].cancel();
  const a = el.animate([{ [property]: value }], {
    duration: ms(f[0]),
    delay: ms(f[1]),
    easing: easings[Number(f[2] ?? 2n)] || "ease-out",
    fill: "none",
  });
  running[property] = a;
  animations.set(id, a);
  const end = (finished) => () => {
    animations.delete(id);
    if (running[property] === a) delete running[property];
    el.dispatchEvent(new CustomEvent("viso-animationend", { detail: { target, finished } }));
  };
  a.onfinish = end(true);
  a.oncancel = end(false);
};
// Applies the requests a transaction made of named nodes, each
// `[action, key, args]`: focus, blur, scroll into view, scroll to, scroll by,
// a list's scroll to a row by index or key, and an animation's start; and
// those of animations, by id: cancel and finish.
const reach = (requests, from) => {
  if (!requests) return;
  for (const [action, key, args] of requests) {
    if (action === 8 || action === 9) {
      const a = animations.get(key);
      if (a) action === 8 ? a.cancel() : a.finish();
      continue;
    }
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
      case 7: animate(el, args[0], args[1], args[2]); break;
    }
  }
};
// An `Offset` as a CSS `translate`.
const offset = (v) => (v === null ? "none" : length(v.f[0]) + " " + length(v.f[1]));
// An `F32` as a CSS `scale`, an `Angle` as a `rotate`.
const scaling = (v) => String(v ?? 1);
const turning = (v) => (v ?? 0) + "deg";
// An `Alignment2D` as a CSS `transform-origin`, laid out right to left when `rtl`.
const pivot = (v, rtl) => {
  const t = Math.min(Math.max(Number(v ?? 4n), 0), 8);
  const across = t % 3 === 1 ? "center" : (t % 3 === 0) !== rtl ? "left" : "right";
  return across + " " + ["top", "center", "bottom"][Math.floor(t / 3)];
};
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
// The CSS `cursor` of each `Cursor`, by tag.
const cursors = [
  "default", "pointer", "text", "vertical-text", "crosshair", "move", "grab",
  "grabbing", "not-allowed", "wait", "progress", "help", "context-menu",
  "copy", "alias", "zoom-in", "zoom-out", "ew-resize", "ns-resize",
  "nesw-resize", "nwse-resize", "col-resize", "row-resize", "none",
];
const cursor = (v) => (v === null ? "" : cursors[Number(v)] || "");
// The CSS `font-weight` of each `FontWeight`, by tag.
const weights = [100, 300, 400, 500, 600, 700, 900];
const weight = (v) => (v === null ? "" : String(weights[Number(v)] || ""));
// A `FontFamily`'s CSS family list.
const family = (v) => {
  if (v === null) return "";
  if (typeof v === "object") return '"' + String(v.f[0]).replace(/["\\]/g, "") + '", system-ui, sans-serif';
  return ["system-ui, sans-serif", "serif", "monospace"][Number(v)] || "";
};
// A `MixedLength` a span sizes or raises by: its `%` of the font size.
const spanLength = (v) => {
  if (v === null) return "";
  const f = v.f;
  return length({ f: [f[0], f[1], f[2], Number(f[3] ?? 0) + Number(f[4] ?? 0) / 100] });
};
// A text's pieces, each a `<span>` styled its own way.
const pieces = (list) => (list || []).map((p) => {
  const [t, c, bg, w, it, fam, size, raise, under, through] = p.f;
  const s = document.createElement("span");
  s.textContent = text(t);
  if (c !== null) s.style.color = color(c);
  if (bg !== null) s.style.backgroundColor = color(bg);
  if (w !== null) s.style.fontWeight = weight(w);
  if (it !== null) s.style.fontStyle = Number(it) === 1 ? "italic" : "normal";
  if (fam !== null) s.style.fontFamily = family(fam);
  if (size !== null) s.style.fontSize = spanLength(size);
  if (raise !== null) s.style.verticalAlign = spanLength(raise);
  const lines = [truth(under) && "underline", truth(through) && "line-through"].filter(Boolean);
  if (lines.length) s.style.textDecorationLine = lines.join(" ");
  return s;
});
// What a text field holds, by tag: its `inputmode` and the `pattern` a
// number holds to.
const holds = [
  [null, null],
  ["numeric", "[0-9]*"],
  ["numeric", "-?[0-9]*"],
  ["decimal", "-?[0-9]*[.]?[0-9]*"],
  ["tel", null],
  ["email", null],
  ["url", null],
  ["search", null],
];
const holding = (v) => holds[Number(v ?? 0n)] || holds[0];
// The number a number field's text reads as, NaN for none.
const reading = (s) => {
  const t = s.trim().replace(",", ".");
  return /^[+-]?(\d+\.?\d*|\.\d+)$/.test(t) ? Number(t) : NaN;
};
// A number field's bounds.
const bounds = (el) => {
  const lo = el.getAttribute("aria-valuemin"), hi = el.getAttribute("aria-valuemax");
  return [lo === null ? -Infinity : Number(lo), hi === null ? Infinity : Number(hi)];
};
// A number field's `n` within its bounds at its places, and its text.
const settled = (el, n) => {
  const [lo, hi] = bounds(el);
  const s = Math.min(Math.max(n, lo), hi).toFixed(Number(el.dataset.places || 0));
  return [Number(s), s];
};
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
  cursor: (el, v) => { el.style.cursor = cursor(v); },
  weight: (el, v) => { el.style.fontWeight = weight(v); },
  italic: (el, v) => { el.style.fontStyle = Number(v ?? 0n) === 1 ? "italic" : ""; },
  spans: (el, v) => { el.replaceChildren(...pieces(v)); },
  label: (el, v, alt) => attr(el, alt ? "alt" : "aria-label", v),
  hint: (el, v) => attr(el, "aria-description", v),
  spoken: (el, v) => attr(el, "aria-valuetext", v),
  hidden: (el, v) => attr(el, "aria-hidden", truth(v) ? "true" : null),
  level: (el, v) => attr(el, "aria-level", v),
  checked: (el, v) => { el.checked = truth(v); },
  open: (el, v) => { el.open = truth(v); },
  // A dialog opens as a modal one, in the top layer with the page behind
  // it inert; a toast shows unless hidden.
  layer(el, v) {
    const on = truth(v);
    if (el.tagName !== "DIALOG") el.hidden = !on;
    else if (on && !el.open) el.showModal();
    else if (!on && el.open) el.close();
  },
  range: (el, v, name) => { el[name] = v === null ? "" : String(v); },
  input: (el, v) => { const s = text(v); if (el.value !== s) el.value = s; },
  placeholder: (el, v) => attr(el, "placeholder", v),
  secure: (el, v) => { el.type = truth(v) ? "password" : "text"; },
  kind(el, v) {
    const [mode, pattern] = holding(v);
    attr(el, "inputmode", mode);
    attr(el, "pattern", pattern);
  },
  maxlength: (el, v) => attr(el, "maxlength", v),
  // A number field keeps its text while it reads as the number.
  number(el, v) {
    if (v === null) return;
    const n = Number(v);
    if (reading(el.value) !== n) el.value = settled(el, n)[1];
    el.setAttribute("aria-valuenow", el.value);
  },
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
  vscale: (el, v, name) => el.style.setProperty(name, scaling(v)),
  vrotate: (el, v, name) => el.style.setProperty(name, turning(v)),
  vorigin(el, v, name) {
    el.style.setProperty(name, pivot(v, false));
    el.style.setProperty(name + "-rtl", pivot(v, true));
  },
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
const mixed = (v) => rec([v, 0, 0, 0, 0]);

// How far a press travels before it is no tap or long press.
const slop = (e) => (e.pointerType === "mouse" ? 4 : 8);

// The lift that ends a drag or a long press clicks nothing; a key's click
// is no lift.
let claimed = false;
addEventListener("pointerdown", () => { claimed = false; }, true);
addEventListener("click", (e) => {
  if (!claimed || e.detail === 0) return;
  claimed = false;
  e.stopPropagation();
  e.preventDefault();
}, true);

// Makes `el` take drags: a primary press at or under it that no nearer
// element took arms one, and travel past the slop starts it — `el` captures
// the pointer and is dragging until the lift. Each sample bubbles from `el`
// as a `viso-drag` event: its phase (0 start, 1 move, 2 end), the pointer
// event, and the travel since the last sample and since the press.
const draggable = (el) => {
  if (el.$drag) return;
  el.$drag = true;
  el.style.touchAction = "none";
  let press = null;
  const send = (phase, e) => {
    const x = e.clientX, y = e.clientY;
    const tx = x - press.x, ty = y - press.y;
    const detail = phase === 0
      ? { phase, e, dx: tx, dy: ty, tx, ty }
      : { phase, e, dx: x - press.lx, dy: y - press.ly, tx, ty };
    press.lx = x;
    press.ly = y;
    el.dispatchEvent(new CustomEvent("viso-drag", { bubbles: true, detail }));
  };
  el.addEventListener("pointerdown", (e) => {
    if (e.button !== 0 || e.$drag || off(e.target)) return;
    e.$drag = el;
    press = { id: e.pointerId, x: e.clientX, y: e.clientY, lx: e.clientX, ly: e.clientY, on: false };
  });
  el.addEventListener("pointermove", (e) => {
    if (!press || e.pointerId !== press.id) return;
    if (press.on) return send(1, e);
    if (Math.hypot(e.clientX - press.x, e.clientY - press.y) < slop(e)) return;
    press.on = true;
    el.setPointerCapture(e.pointerId);
    el.dataset.visoDragging = "";
    send(0, e);
  });
  const end = (e) => {
    if (!press || e.pointerId !== press.id) return;
    if (press.on) {
      send(2, e);
      delete el.dataset.visoDragging;
      claimed = true;
    }
    press = null;
  };
  el.addEventListener("pointerup", end);
  el.addEventListener("pointercancel", end);
};

// Calls `fire` with the payload of a primary press at or under `el` that no
// nearer element took, once it held still for the long-press delay.
const holdable = (el, fire) => {
  let hold = null;
  const drop = () => {
    if (hold) clearTimeout(hold.t);
    hold = null;
  };
  el.addEventListener("pointerdown", (e) => {
    if (e.button !== 0 || e.$hold || off(e.target)) return;
    e.$hold = el;
    drop();
    hold = {
      id: e.pointerId, x: e.clientX, y: e.clientY,
      t: setTimeout(() => {
        hold = null;
        claimed = true;
        fire(rec([point(e, el), kind(e)]));
      }, 500),
    };
  });
  el.addEventListener("pointermove", (e) => {
    if (hold && e.pointerId === hold.id && Math.hypot(e.clientX - hold.x, e.clientY - hold.y) >= slop(e)) drop();
  });
  el.addEventListener("pointerup", drop);
  el.addEventListener("pointercancel", drop);
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
    if (control === 0 || control === 8) el.addEventListener("change", () => fire(rec([bool(el.checked)])), capture);
    else if (control === 9) el.addEventListener("toggle", () => fire(rec([bool(el.open)])), capture);
    else if (control === 10) {
      // Escape and a press on the scrim, the dialog's own box, ask it to
      // close; the view decides.
      el.addEventListener("cancel", (e) => { e.preventDefault(); fire(rec([bool(false)])); }, capture);
      el.addEventListener("pointerdown", (e) => { if (e.target === el && el.open) fire(rec([bool(false)])); }, capture);
    }
    else if (control === 1) el.addEventListener("input", () => fire(rec([Number(el.value)])), capture);
    else if (control === 13) {
      // Each number the text reads as within the bounds; Enter and the
      // focus leaving settle the text, the arrows step it.
      const held = (n) => { const [lo, hi] = bounds(el); return n >= lo && n <= hi; };
      const show = (n) => { const [v, s] = settled(el, n); el.value = s; el.$settled = v; fire(rec([v])); };
      el.addEventListener("input", () => { const n = reading(el.value); if (held(n)) fire(rec([n])); }, capture);
      el.addEventListener("change", () => {
        const n = reading(el.value);
        show(Number.isFinite(n) ? n : el.$settled ?? 0);
      }, capture);
      el.addEventListener("keydown", (e) => {
        const dir = { ArrowUp: 1, ArrowDown: -1, PageUp: 10, PageDown: -10 }[e.key];
        if (dir === undefined || e.ctrlKey || e.metaKey) return;
        e.preventDefault();
        const scale = e.shiftKey ? 10 : e.altKey ? 0.1 : 1;
        const n = reading(el.value);
        show((Number.isFinite(n) ? n : el.$settled ?? 0) + dir * scale * Number(el.dataset.step || 1));
      }, capture);
    }
    else el.addEventListener("input", () => fire(rec([el.value])), capture);
  } else if (route === 10) {
    el.addEventListener("keydown", live((e) => {
      if (e.key === "Enter" && !e.repeat && !e.isComposing) fire(null);
    }), capture);
  } else if (route === 12) {
    el.addEventListener("scroll", () => fire(rec([
      rec([mixed(el.scrollLeft), mixed(el.scrollTop)]),
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
  } else if (route === 14) {
    el.addEventListener("viso-animationend", (e) => fire(rec([e.detail.target, bool(e.detail.finished)])), capture);
  } else if (route === 18) {
    holdable(el, fire);
  } else if (route >= 15) {
    draggable(el);
    el.addEventListener("viso-drag", live((e) => {
      const d = e.detail;
      if (d.phase === route - 15) fire(rec([point(d.e, el), rec([mixed(d.dx), mixed(d.dy)]), rec([mixed(d.tx), mixed(d.ty)])]));
    }), capture);
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
  if (control === 0 || control === 8) el.checked = truth(v);
  else if (control === 9) el.open = truth(v);
  else if (control === 10 || control === 11) P.layer(el, v);
  else if (control === 1) el.value = v === null ? el.min : String(v);
  else if (control === 13) P.number(el, v);
  else if (control === 2) select(el, v === null ? 0n : v, el.$radio);
  else if (v !== null) P.input(el, v);
};

