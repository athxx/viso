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
// call returns: an `animate` or a `play` the id of the `Animation` it
// starts, which rides along as a last argument; an `is_running` whether the
// animation `key` runs, as the requests queued so far leave it.
const request = (queue, action, key, args) => {
  if (action === 10) {
    for (let i = queue.length - 1; i >= 0; i--) {
      const [a, k, g] = queue[i];
      if ((a === 7 || a === 11) && g[g.length - 1] === key) return 1n;
      if ((a === 8 || a === 9) && k === key) return 0n;
    }
    return bool(animations.has(key));
  }
  if (action === 7 || action === 11) {
    const id = ++lastAnimation;
    queue.push([action, key, [...args, id]]);
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
  run(el, property, target, id, () => el.animate([{ [property]: value }], {
    duration: ms(easeSeconds(f)),
    delay: ms(f[1]),
    easing: easing(f[2]),
    fill: "none",
  }));
};
// Plays the `Keyframes` `track` on `el` as the animation `id`: its stops of
// the first's property, in order of `at`, from the value shown where the
// first is past the start and holding the last where it falls short of the
// end, each eased in from the one before.
const play = (el, track, id) => {
  const f = track.f;
  const all = (f[0] ?? []).filter((s) => Number.isFinite(Number(s.f[0])));
  if (!all.length) return;
  const tag = all[0].f[1].t;
  const stops = all.filter((s) => s.f[1].t === tag)
    .map((s) => ({ at: Math.min(Math.max(Number(s.f[0]), 0), 1), value: s.f[1], easing: s.f[2] }))
    .sort((a, b) => a.at - b.at);
  const [property] = animated(stops[0].value);
  const frames = [];
  if (stops[0].at > 0) frames.push({ offset: 0, [property]: getComputedStyle(el)[property] });
  for (const s of stops) {
    if (frames.length) frames[frames.length - 1].easing = stopEasing(s.easing);
    frames.push({ offset: s.at, [property]: animated(s.value)[1] });
  }
  if (stops[stops.length - 1].at < 1) frames.push({ ...frames[frames.length - 1], offset: 1, easing: "linear" });
  const instant = reduced.matches && f[5] === 0n;
  const ms = (x) => Math.max(Number(x ?? 0), 0) * 1000;
  const repeat = f[3];
  const iterations = instant ? 0 : repeat === 1n ? Infinity : repeat && repeat.f ? Number(repeat.f[0]) : 1;
  const directions = ["normal", "reverse", "alternate", "alternate-reverse"];
  const target = stops[stops.length - 1].value;
  run(el, property, target, id, () => el.animate(frames, {
    duration: ms(f[1]),
    delay: instant ? 0 : ms(f[2]),
    iterations,
    direction: directions[Number(f[4] ?? 0n)] ?? "normal",
    fill: "none",
  }));
};
// A stop's easing: a spring has no time of its own there, and eases out.
const stopEasing = (e) => (e && e.t === 35 ? easeNamed(2) : easing(e ?? 0n));
// Runs the animation `start` makes of `el`'s `property` as `id` toward
// `target`: a newer one of the same property cancels it, and `el` hears its
// end as an `animation_end`.
const run = (el, property, target, id, start) => {
  const running = (el.$anims ??= {});
  if (running[property]) running[property].cancel();
  const a = start();
  running[property] = a;
  animations.set(id, a);
  a.$end = (finished) => {
    a.onfinish = a.oncancel = null;
    if (finished && a.playState !== "finished") a.cancel();
    animations.delete(id);
    if ($.ended) $.ended(id);
    if (running[property] === a) delete running[property];
    el.dispatchEvent(new CustomEvent("viso-animationend", { detail: { target, finished } }));
  };
  a.onfinish = () => a.$end(true);
  a.oncancel = () => a.$end(false);
};
// Applies the requests a transaction made of named nodes, each
// `[action, key, args]`: focus, blur, scroll into view, scroll to, scroll by,
// a list's scroll to a row by index or key, and an animation's start or a
// track's play; and those of animations, by id: cancel and finish (a track
// that repeats forever has no end to finish at, and ends where it is).
const reach = (requests, from) => {
  if (!requests) return;
  for (const [action, key, args] of requests) {
    if (action === 8 || action === 9) {
      const a = animations.get(key);
      if (!a) continue;
      if (action === 8) a.cancel();
      else if (a.effect.getComputedTiming().iterations === Infinity) a.$end(true);
      else a.finish();
      continue;
    }
    const el = named(key, from);
    if (!el) continue;
    const at = (v) => [fold(el, v.f[0]), fold(el, v.f[1])];
    // A scroll's last `smooth` glides, unless motion is reduced.
    const glide = (v) => (v === 1n && !reduced.matches ? "smooth" : "instant");
    const to = (v, smooth) => { const [left, top] = at(v); return { left, top, behavior: glide(smooth) }; };
    const lead = (smooth) => ({ block: "start", inline: "start", behavior: glide(smooth) });
    switch (action) {
      case 0: el.focus({ focusVisible: false }); break;
      case 1: if (el.contains(document.activeElement)) document.activeElement.blur(); break;
      case 2: el.scrollIntoView({ block: "nearest", inline: "nearest", behavior: glide(args[0]) }); break;
      case 3: el.scrollTo(to(args[0], args[1])); break;
      case 4: el.scrollBy(to(args[0], args[1])); break;
      case 5: {
        const all = rows(el);
        const r = all[Math.max(0, Math.min(Number(args[0]), all.length - 1))];
        if (r) r.scrollIntoView(lead(args[1]));
        break;
      }
      case 6: { const r = row(el, $.key(args[0])); if (r) r.scrollIntoView(lead(args[1])); break; }
      case 7: animate(el, args[0], args[1], args[2]); break;
      case 11: play(el, args[0], args[1]); break;
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
// A `Dp` in px; a `box-shadow` of none that a list of them takes.
const dp = (v) => (v !== null && typeof v === "object" ? Number(v.f[0]) : Number(v ?? 0));
const noShadow = "0 0 #0000";
// An `Option<Border>` as an inset `box-shadow`.
const stroke = (v) => {
  if (v === null) return noShadow;
  return "inset 0 0 0 " + Math.max(dp(v.f[0]), 0) + "px " + color(v.f[1]);
};
// A `List<Shadow>` (or one) as a `box-shadow` list.
const shadow = (v) => {
  const all = (Array.isArray(v) ? v : v === null ? [] : [v]).map((s) => {
    const [x, y, blur, spread, paint, inset] = s.f;
    return (Number(inset ?? 0n) === 1 ? "inset " : "") + dp(x) + "px " + dp(y) + "px " +
      Math.max(dp(blur), 0) + "px " + dp(spread) + "px " + color(paint);
  });
  return all.length ? all.join(", ") : noShadow;
};
// A `List<TextShadow>` (or one) as a `text-shadow`; CSS has no inset one.
const textShadow = (v) => {
  const all = (Array.isArray(v) ? v : v === null ? [] : [v])
    .filter((s) => Number(s.f[4] ?? 0n) !== 1)
    .map((s) => {
      const [x, y, blur, paint] = s.f;
      return dp(x) + "px " + dp(y) + "px " + Math.max(dp(blur), 0) + "px " + color(paint);
    });
  return all.length ? all.join(", ") : "none";
};
// A gradient layer covering its box less `inset`, placed and sized within it.
const placed = (layer, inset) => {
  if (!inset || !inset.f) return layer;
  const [t, e, b, s] = inset.f.map(dp);
  if (!t && !e && !b && !s) return layer;
  return layer + " " + s + "px " + t + "px / calc(100% - " + (s + e) + "px) calc(100% - " +
    (t + b) + "px) no-repeat";
};
// A `List<Fill>` (or one `Fill` or colour) as a `background`: its layers top
// first, its first colour the bottom layer's colour.
const fill = (v) => {
  const all = Array.isArray(v) ? v : v === null ? [] : [v];
  const stops = (colors, at) => {
    const placed = at.length === colors.length;
    const all = colors.map((c, i) =>
      color(c) + (placed ? " " + Math.min(Math.max(Number(at[i]), 0), 1) * 100 + "%" : ""));
    return all.length === 1 ? all[0] + ", " + all[0] : all.join(", ");
  };
  let base = null;
  const layers = [];
  all.forEach((f, i) => {
    if (typeof f === "bigint") {
      if (i === 0) base = color(f);
      else layers.push("linear-gradient(" + color(f) + ", " + color(f) + ")");
      return;
    }
    const k = Number(f.t ?? 0), g = f.f;
    if (k === 0) {
      if (i === 0) base = color(g[0]);
      else layers.push("linear-gradient(" + color(g[0]) + ", " + color(g[0]) + ")");
    } else if (k === 1 && g[1].length) {
      layers.push(placed("linear-gradient(in srgb-linear " + Number(g[0] ?? 180) + "deg, " +
        stops(g[1], g[2]) + ")", g[3]));
    } else if (k === 2 && g[2].length) {
      layers.push(placed("radial-gradient(in srgb-linear circle farthest-corner at " +
        Number(g[0]) * 100 + "% " + Number(g[1]) * 100 + "%, " + stops(g[2], g[3]) + ")", g[4]));
    } else if (k === 3) {
      const href = url(g[0]);
      const fit = fits[Number(g[1] ?? 1n)];
      const place = fit === "contain" ? "center / contain no-repeat"
        : fit === "fill" ? "center / 100% 100% no-repeat"
        : fit === "tile" ? "0 0 repeat" : "center / cover no-repeat";
      if (href) layers.push('url("' + href + '") ' + place);
    }
  });
  layers.reverse();
  if (base !== null) layers.push(base);
  return layers.length ? layers.join(", ") : "transparent";
};
// An `Option<Backdrop>` as a `backdrop-filter`, and its tint as a colour.
const backdrop = (v) =>
  v === null ? "none" : "blur(" + Math.max(dp(v.f[0]), 0) + "px) saturate(" + Number(v.f[1] ?? 1) + ")";
const frost = (v) => (v === null ? "transparent" : color(v.f[2]));
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
const fits = ["contain", "cover", "fill", "tile"];
// A fit as a mask's or background's size.
const fitSize = (fit) => (fit === "fill" ? "100% 100%" : fit === "tile" ? "auto" : fit);
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
  const [t, c, bg, w, it, fam, size, raise, under, through, link] = p.f;
  const s = document.createElement(link === null ? "span" : "a");
  if (link !== null) s.href = text(link);
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
const reduced = matchMedia("(prefers-reduced-motion: reduce)");
const items = (el) => Array.from(el.children).filter((c) => c.tagName !== "TEMPLATE");
const select = (el, v, radio) => {
  const k = radio ? "aria-checked" : "aria-selected";
  items(el).forEach((c, i) => {
    const on = BigInt(i) === v;
    c.setAttribute(k, on ? "true" : "false");
    c.tabIndex = on ? 0 : -1;
  });
  if (el.classList.contains("vx-segmented")) {
    thumb(el);
    if (!el.$thumb) {
      el.$thumb = new ResizeObserver(() => thumb(el));
      el.$thumb.observe(el);
    }
  }
};
// A colour picker's colour as its colour input and opacity range show it:
// rgba, packed, its alpha whole while the range is hidden.
const picked = (el) => {
  const [c, a] = el.querySelectorAll("input");
  const rgb = parseInt((c?.value || "#000000").slice(1), 16) || 0;
  const alpha = !a || a.hidden ? 255 : Math.round(Math.min(Math.max(Number(a.value), 0), 1) * 255);
  return BigInt(((rgb << 8) | alpha) >>> 0);
};
// A combo box's or a search field's input and list, the list given an id of
// its own the first time, so copies of one template each keep theirs.
let lists = 0;
const listed = (el) => {
  const input = el.querySelector("input"), list = el.querySelector("datalist");
  if (list && !el.$listed) {
    el.$listed = true;
    list.id = "vx-listed-" + ++lists;
    input.setAttribute("list", list.id);
  }
  return [input, list];
};
// Whether a combo box offers `text`.
const offered = (el, text) => [...(listed(el)[1]?.options ?? [])].some((o) => o.value === text);
// A search field's typing: each query once the typing rests for its
// debounce, at once when emptied or on Enter, as a `viso-query`; Enter and
// a suggestion taken submit it, as a `viso-submit`.
const searching = (el) => {
  if (el.$searching) return;
  el.$searching = true;
  const [input] = listed(el);
  let wait;
  const report = () => {
    clearTimeout(wait);
    wait = undefined;
    el.dispatchEvent(new CustomEvent("viso-query", { detail: input.value }));
  };
  const submit = () => el.dispatchEvent(new CustomEvent("viso-submit"));
  input.addEventListener("input", (e) => {
    clearTimeout(wait);
    const ms = Number(el.dataset.debounce || 0);
    // A suggestion taken replaces the text whole.
    const taken = !(e instanceof InputEvent) || e.inputType === "insertReplacementText";
    if (taken && input.value) { report(); submit(); }
    else if (!ms || !input.value) report();
    else wait = setTimeout(report, ms);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key !== "Enter" || e.repeat || e.isComposing) return;
    if (wait !== undefined) report();
    submit();
  });
};
// A knob's fill and pointer at its value's fraction of its range.
const knob = (el) => {
  if (!el.classList.contains("vx-knob")) return;
  const [lo, hi, v] = [Number(el.min || 0), Number(el.max || 1), Number(el.value)];
  el.style.setProperty("--vx-at", hi > lo ? String(Math.min(Math.max((v - lo) / (hi - lo), 0), 1)) : "0");
};
// A segmented control's thumb under its selected segment, sliding there.
const thumb = (el) => {
  const c = items(el).find((c) => c.getAttribute("aria-checked") === "true");
  el.style.setProperty("--vx-thumb-x", (c ? c.offsetLeft : 0) + "px");
  el.style.setProperty("--vx-thumb-w", (c ? c.offsetWidth : 0) + "px");
};
// The transition a stack's pages trade places by where it names this
// platform's own: a slide on Apple's, a shared axis on Android, else a fade.
const nativeTurn = () => {
  const ua = navigator.userAgent;
  return /Mac|iPhone|iPad/.test(ua) ? 1 : /Android/.test(ua) ? 2 : 3;
};
// Where a page stands and how opaque it is now, mid-move included.
const standing = (c) => {
  const s = getComputedStyle(c);
  return { transform: s.transform === "none" ? "translateX(0px)" : s.transform, opacity: s.opacity };
};
const across = (dx, opacity) => ({ transform: "translateX(" + dx + "px)", opacity: String(opacity) });
// The heroes showing within `page`, by tag.
const heroesIn = (page) => {
  const out = new Map();
  for (const h of [page, ...page.querySelectorAll("[data-hero]")]) {
    if (!h.dataset.hero) continue;
    if (h.getClientRects().length && !out.has(h.dataset.hero)) out.set(h.dataset.hero, h);
  }
  return out;
};
// A navigation stack shows the page at `v`, folding the others away. Turned
// from another page, the two move as its transition says: the arriving one
// in from where it enters, the leaving one out to where it leaves, folding
// once there and taking no pointer meanwhile. Pages stack in their order. A
// turn while one is under way takes each page on from where it stands. Each
// hero of the arriving page flies from where the leaving page's hero of its
// tag stands, scaled to its width, straight across the screen as its page
// moves, while the one it left hides.
const page = (el, v) => {
  const to = v === null ? -1 : Number(v);
  const first = el.$page === undefined;
  if (!first && to === el.$page) return;
  const from = first ? -1 : el.$page;
  el.$page = to;
  const pages = items(el);
  const flies = !first && !reduced.matches && pages[from] && pages[to];
  const left = flies ? heroesIn(pages[from]) : new Map();
  const sources = new Map([...left].map(([tag, h]) => [tag, h.getBoundingClientRect()]));
  const moving = el.$moving || new Map();
  el.$moving = moving;
  const was = new Map();
  for (const [c, a] of moving) {
    was.set(c, standing(c));
    a.cancel();
  }
  moving.clear();
  pages.forEach((c, i) => {
    c.toggleAttribute("data-vx-away", i !== to);
    c.inert = false;
  });
  if (first) return;
  const a = pages[from], b = pages[to];
  if (!a || !b) return;
  let kind = el.$turn ?? Number(el.dataset.turn || 0);
  if (kind === 0) kind = nativeTurn();
  if ((kind === 1 || kind === 2) && reduced.matches) kind = 3;
  if (kind === 4) return;
  const back = to < from;
  const on = getComputedStyle(el).direction === "rtl" ? -1 : 1;
  const w = el.clientWidth;
  let timing, into, out;
  if (kind === 1) {
    const [css, seconds] = spring({ f: [16 * Math.PI * Math.PI, 8 * Math.PI, 1] });
    timing = { duration: seconds * 1000, easing: css };
    const full = w * on, drift = -w * 0.3 * on;
    [into, out] = back ? [across(drift, 1), across(full, 1)] : [across(full, 1), across(drift, 1)];
  } else if (kind === 2) {
    timing = { duration: 300, easing: "cubic-bezier(0.2, 0, 0, 1)" };
    const step = (back ? -30 : 30) * on;
    [into, out] = [across(step, 0), across(-step, 0)];
  } else {
    timing = { duration: 200, easing: "cubic-bezier(0.65, 0, 0.35, 1)" };
    [into, out] = back ? [across(0, 1), across(0, 0)] : [across(0, 0), across(0, 1)];
  }
  const rest = across(0, 1);
  a.removeAttribute("data-vx-away");
  a.inert = true;
  // Each hero's box at rest, measured before its page moves.
  const landing = [];
  if (sources.size) {
    for (const [tag, h] of heroesIn(b)) {
      if (sources.has(tag)) landing.push([tag, h, h.getBoundingClientRect()]);
    }
  }
  const arrive = b.animate([was.get(b) || into, rest], timing);
  const leave = a.animate([was.get(a) || rest, out], timing);
  moving.set(b, arrive).set(a, leave);
  const shift = new DOMMatrixReadOnly((was.get(b) || into).transform);
  for (const [tag, h, box] of landing) {
    const src = sources.get(tag);
    if (!box.width || !src.width) continue;
    const [ox, oy] = getComputedStyle(h).transformOrigin.split(" ").map(parseFloat);
    const k = src.width / box.width;
    const tx = src.x + src.width / 2 - box.x - ox - k * (box.width / 2 - ox) - shift.m41;
    const ty = src.y + src.height / 2 - box.y - oy - k * (box.height / 2 - oy) - shift.m42;
    const fly = h.animate([{ transform: "translate(" + tx + "px, " + ty + "px) scale(" + k + ")" }, { transform: "none" }], timing);
    const gone = left.get(tag);
    moving.set(h, fly).set(gone, gone.animate([{ opacity: 0 }, { opacity: 0 }], timing));
  }
  arrive.onfinish = () => { if (moving.get(b) === arrive) moving.delete(b); };
  leave.onfinish = () => {
    if (moving.get(a) !== leave) return;
    moving.delete(a);
    a.inert = false;
    if (items(el).indexOf(a) !== el.$page) a.setAttribute("data-vx-away", "");
  };
};
// A `data-` attribute `name` the script reads for value `v`: a length in
// px, a viewer's double tap empty for none.
const datum = (v, name) => {
  if (name === "data-pan-margin") return String(dp(v));
  // No text to carry makes no drag source.
  if (v === null) return name === "data-drag" ? null : "";
  return Array.isArray(v) ? v.map(String).join("\n") : String(v);
};

const url = (v) => {
  if (v === null || typeof v !== "object") return "";
  return A.get(String(BigInt.asUintN(64, v.f[0]))) || "";
};
// An avatar's initials and fill, as the runtime makes them: the first
// letters of the first and last words, one letter without case alone; the
// FNV-1a hash of the trimmed name picking the fill.
const initials = (name) => {
  const lead = (w) => { for (const c of w) if (/[\p{L}\p{N}]/u.test(c)) return c; return null; };
  const words = name.split(/\s+/).filter(lead);
  if (!words.length) return "";
  const first = lead(words[0]);
  let out = first.toUpperCase();
  if (first.toLowerCase() !== first.toUpperCase() && words.length > 1) {
    out += lead(words[words.length - 1]).toUpperCase();
  }
  return out;
};
const FILLS = ["#c62828", "#ad1457", "#6a1b9a", "#283593", "#1565c0", "#00695c", "#2e7d32",
  "#d84315", "#4e342e", "#37474f"];
const avatarFill = (name) => {
  let h = 0x811c9dc5;
  for (const b of new TextEncoder().encode(name.trim())) h = Math.imul(h ^ b, 0x01000193) >>> 0;
  return FILLS[h % FILLS.length];
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
  sticky: (el, v) => {
    const on = truth(v);
    el.style.position = on ? "sticky" : "";
    el.style.insetBlockStart = el.style.insetInlineStart = on ? "0" : "";
    el.style.zIndex = on ? "1" : "";
  },
  cursor: (el, v) => { el.style.cursor = cursor(v); },
  weight: (el, v) => { el.style.fontWeight = weight(v); },
  italic: (el, v) => { el.style.fontStyle = Number(v ?? 0n) === 1 ? "italic" : ""; },
  spans: (el, v) => { el.replaceChildren(...pieces(v)); },
  selectable: (el, v) => { el.classList.toggle("vx-selectable", truth(v)); },
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
    if (el.$sheet) el.$sheet.present(on);
    else if (el.tagName !== "DIALOG") el.hidden = !on;
    else if (on && !el.open) el.showModal();
    else if (!on && el.open) el.close();
  },
  range: (el, v, name) => { el[name] = v === null ? "" : String(v); knob(el); },
  href: (el, v) => attr(el, "href", v),
  pressed: (el, v) => attr(el, "aria-pressed", truth(v) ? "true" : "false"),
  // A colour picker's colour, shown on its colour input and its opacity
  // range, or whether it takes an opacity.
  picker(el, v, name) {
    const [c, a] = el.querySelectorAll("input");
    if (name === "alpha") { if (a) a.hidden = !truth(v); return; }
    if (v === null) return;
    const n = Number(BigInt.asUintN(32, v));
    if (c) c.value = "#" + (n >>> 8).toString(16).padStart(6, "0");
    if (a) a.value = String((n & 255) / 255);
  },
  // A combo box's or a search field's text, placeholder, name, the labels
  // its list offers, whether a combo box takes any text, or how long a
  // search field waits.
  listed(el, v, name) {
    const [input, list] = listed(el);
    const text = v === null ? null : String(v);
    if (name === "options") {
      list?.replaceChildren(...(Array.isArray(v) ? v : []).map((label) => {
        const o = document.createElement("option");
        o.value = String(label);
        return o;
      }));
    } else if (name === "free") attr(el, "data-free", truth(v) ? "true" : null);
    else if (name === "debounce") attr(el, "data-debounce", v === null ? null : String(Number(v) * 1000));
    else if (name === "placeholder") attr(input, "placeholder", text);
    else if (name === "label") attr(input, "aria-label", text);
    else {
      el.$held = text ?? "";
      if (input.value !== el.$held) input.value = el.$held;
    }
  },
  // A stepper's number, bounds, step, wrap or tint: the halves that can
  // step no further are spent.
  stepper(el, v, name) {
    if (name === "tint") { el.style.color = v === null ? "" : color(v); return; }
    const a = { value: "aria-valuenow", min: "aria-valuemin", max: "aria-valuemax" }[name];
    if (a) attr(el, a, v === null ? null : String(v));
    else if (name === "wrap") attr(el, "data-wrap", truth(v) ? "true" : null);
    else attr(el, "data-step", v === null ? null : String(v));
    const [lo, hi, now, , wraps] = stepping(el);
    const [down, up] = el.querySelectorAll(".vx-step");
    down?.classList.toggle("vx-spent", !wraps && now <= lo);
    up?.classList.toggle("vx-spent", !wraps && now >= hi);
  },
  badge(el, v, name) {
    el.dataset[name] = v === null ? "" : String(v);
    const c = el.dataset.count, m = Math.max(Number(el.dataset.max || 99), 1);
    const n = c ? Math.max(Number(c), 0) : null;
    el.textContent = n === null ? "" : n > m ? m + "+" : String(n);
    el.classList.toggle("vx-dot", n === null);
  },
  avatar(el, v, name) {
    if (name === "image") el.dataset.image = url(v);
    else {
      el.dataset.name = v ?? "";
      attr(el, "aria-label", el.dataset.name || null);
      attr(el, "role", el.dataset.name ? "img" : null);
    }
    const u = el.dataset.image, who = el.dataset.name || "";
    el.style.backgroundImage = u ? 'url("' + u + '")' : "";
    el.textContent = u ? "" : initials(who);
    if (!("tinted" in el.dataset)) el.style.backgroundColor = avatarFill(who);
  },
  // A progress indicator's value, bounds or tint: its fill shows the
  // value's fraction of its range, and with no value it runs.
  progress(el, v, name) {
    if (name === "tint") { el.style.color = v === null ? "" : color(v); return; }
    const attr = name === "value" ? "aria-valuenow" : "aria-value" + name;
    if (v === null) el.removeAttribute(attr); else el.setAttribute(attr, String(v));
    const n = (a, d) => { const s = el.getAttribute(a); return s === null ? d : Number(s); };
    const min = n("aria-valuemin", 0), max = n("aria-valuemax", 1), now = n("aria-valuenow", null);
    if (now === null) el.style.removeProperty("--vx-at");
    else el.style.setProperty("--vx-at", String(max > min ? Math.min(Math.max((now - min) / (max - min), 0), 1) : 0));
  },
  input: (el, v) => { const s = text(v); if (el.value !== s) el.value = s; },
  placeholder: (el, v) => attr(el, "placeholder", v),
  secure: (el, v) => { el.type = truth(v) ? "password" : "text"; },
  kind(el, v) {
    const [mode, pattern] = holding(v);
    attr(el, "inputmode", mode);
    attr(el, "pattern", pattern);
  },
  maxlength: (el, v) => attr(el, "maxlength", v),
  invalid: (el, v) => attr(el, "aria-invalid", truth(v) ? "true" : null),
  // A number field keeps its text while it reads as the number.
  number(el, v) {
    if (v === null) return;
    const n = Number(v);
    if (reading(el.value) !== n) el.value = settled(el, n)[1];
    el.setAttribute("aria-valuenow", el.value);
  },
  selected: (el, v, radio) => select(el, v === null ? 0n : v, radio),
  page,
  turn: (el, v) => { el.$turn = Number(v ?? 0n); },
  hero: (el, v) => attr(el, "data-hero", v),
  ring: (el, v) => { el.style.outline = truth(v) ? "" : "none"; },
  key: (el, v) => attr(el, "accesskey", v),
  data(el, v, name) {
    attr(el, name, datum(v, name));
    if (name === "data-drag") el.draggable = v !== null;
  },
  axis: (el, v) => {
    if (v === null) delete el.dataset.dragAxis;
    else el.dataset.dragAxis = v === 0n ? "row" : "column";
    if (el.$drag) el.style.touchAction = dragTouch(el);
  },
  // As its `src` (0), the mask its tint shows through (1) or a background
  // pattern (2).
  source(el, v, shown) {
    const u = url(v);
    const image = u ? 'url("' + u + '")' : "none";
    if (shown === 0) el.src = u;
    else if (shown === 1) el.style.maskImage = el.style.webkitMaskImage = image;
    else el.style.backgroundImage = image;
  },
  // A video's `url`, which plays in place of its source when set, and how
  // it plays.
  media(el, v, name) {
    if (name === "facing" || name === "device") {
      const key = name === "facing" ? "vxFacing" : "vxDevice";
      const set = name === "facing" ? ["user", "environment"][Number(v ?? -1)] : v;
      if (set) el.dataset[key] = set; else delete el.dataset[key];
      camera(el);
    } else if (name === "capture") {
      if (truth(v)) delete el.dataset.vxPaused; else el.dataset.vxPaused = "";
      camera(el);
    } else if (name === "mirror") {
      if (v === null || v === undefined) delete el.dataset.vxMirror;
      else el.dataset.vxMirror = truth(v) ? "1" : "0";
      mirrored(el);
    } else if (name === "url") {
      if (v) el.src = v;
    } else if (name === "autoplay") {
      el.autoplay = truth(v);
      if (truth(v)) el.play().catch(() => {}); else el.pause();
    } else if (name === "loop") el.loop = truth(v);
    else if (name === "muted") el.muted = truth(v);
    else if (name === "controls") el.controls = truth(v);
    else if (name === "fullscreen") {
      // Asked from a gesture's handler, it goes; the browser keeps the say.
      if (truth(v) && document.fullscreenElement !== el) el.requestFullscreen?.().catch(() => {});
      else if (!truth(v) && document.fullscreenElement === el) document.exitFullscreen().catch(() => {});
    }
    else if (name === "volume") el.volume = Math.min(Math.max(Number(v ?? 1), 0), 1);
    else el.playbackRate = Number(v ?? 1);
  },
  fit(el, v, shown) {
    const fit = fits[Number(v ?? 0n)] || "contain";
    const s = el.style;
    if (shown === 0) s.objectFit = fit === "tile" ? "none" : fit;
    else if (shown === 1) {
      s.maskSize = s.webkitMaskSize = fitSize(fit);
      s.maskRepeat = s.webkitMaskRepeat = fit === "tile" ? "repeat" : "no-repeat";
      s.maskPosition = s.webkitMaskPosition = fit === "tile" ? "0 0" : "center";
    } else {
      s.backgroundSize = fitSize(fit);
      s.backgroundRepeat = fit === "tile" ? "repeat" : "no-repeat";
      s.backgroundPosition = fit === "tile" ? "0 0" : "center";
    }
  },
  vcolor: (el, v, name) => el.style.setProperty(name, color(v)),
  vopacity: (el, v, name) => el.style.setProperty(name, String(Math.min(Math.max(v ?? 1, 0), 1))),
  vradius: (el, v, name) => el.style.setProperty(name, radius(v)),
  vborder: (el, v, name) => el.style.setProperty(name, stroke(v)),
  vshadow: (el, v, name) => el.style.setProperty(name, shadow(v)),
  vfill: (el, v, name) => el.style.setProperty(name, fill(v)),
  vtext_shadow: (el, v, name) => el.style.setProperty(name, textShadow(v)),
  vbackdrop(el, v, name) {
    el.style.setProperty(name, backdrop(v));
    el.style.setProperty(name + "-tint", frost(v));
  },
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
    el.style.setProperty(name, property + " " + (instant ? "0s" : s(easeSeconds(f))) + " " +
      easing(f[2]) + " " + (instant ? "0s" : s(f[1])));
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
// event, the travel since the last sample and since the press, and the
// velocity (px/s) over the last 40ms of samples, none once cancelled.
// What a touch on `el` the browser may still pan: the axis across its drag's.
const dragTouch = (el) => ({ row: "pan-y", column: "pan-x" })[el.dataset.dragAxis] ?? "none";

const draggable = (el) => {
  if (el.$drag) return;
  el.$drag = true;
  el.style.touchAction = dragTouch(el);
  let press = null;
  const velocity = (e) => {
    const t = e.timeStamp, x = e.clientX, y = e.clientY;
    const seen = press.seen;
    seen.push([t, x, y]);
    while (seen.length > 2 && t - seen[0][0] > 100) seen.shift();
    const from = seen.find((s) => t - s[0] <= 40 && t - s[0] > 10) ?? seen[0];
    const dt = (t - from[0]) / 1000;
    return dt > 0.01 ? [(x - from[1]) / dt, (y - from[2]) / dt] : [0, 0];
  };
  const send = (phase, e) => {
    const x = e.clientX, y = e.clientY;
    const tx = x - press.x, ty = y - press.y;
    const [vx, vy] = e.type === "pointercancel" ? [0, 0] : velocity(e);
    const detail = phase === 0
      ? { phase, e, dx: tx, dy: ty, tx, ty, vx, vy }
      : { phase, e, dx: x - press.lx, dy: y - press.ly, tx, ty, vx, vy };
    press.lx = x;
    press.ly = y;
    el.dispatchEvent(new CustomEvent("viso-drag", { bubbles: true, detail }));
  };
  el.addEventListener("pointerdown", (e) => {
    if (e.button !== 0 || e.$drag || off(e.target)) return;
    e.$drag = el;
    press = { id: e.pointerId, x: e.clientX, y: e.clientY, lx: e.clientX, ly: e.clientY, on: false, seen: [[e.timeStamp, e.clientX, e.clientY]] };
  });
  el.addEventListener("pointermove", (e) => {
    if (!press || e.pointerId !== press.id) return;
    if (press.on) return send(1, e);
    velocity(e);
    const dx = e.clientX - press.x, dy = e.clientY - press.y;
    if (Math.hypot(dx, dy) < slop(e)) return;
    // A drag along one axis lets go of a press that travels across it.
    const axis = el.dataset.dragAxis;
    if (axis && (axis === "row") !== (Math.abs(dx) >= Math.abs(dy))) { press = null; return; }
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

// Makes `el` take pinches: two fingers at or under it that no nearer element
// took, travelling past the slop together, Safari's trackpad gesture, and a
// wheel turned with Control held (a browser's trackpad pinch), which ends
// once it rests 150ms. Each sample bubbles from `el` as a `viso-pinch`
// event: its phase (0 start, 1 move, 2 end), the focal point, the scale and
// turn (degrees, clockwise) since the fingers landed, those since the last
// sample, and the focal point's travel since it.
const pinchable = (el) => {
  if (el.$pinch) return;
  el.$pinch = true;
  el.style.touchAction = "none";
  let fingers = [], on = false, from = null, angle = 0, turned = 0, last = null, rest = 0;
  const send = (phase, x, y, scale, turn) => {
    const was = last ?? { x, y, scale: 1, turn: 0 };
    last = { x, y, scale, turn };
    el.dispatchEvent(new CustomEvent("viso-pinch", { bubbles: true, detail: {
      phase, x, y, scale, turn, ds: scale / was.scale, dt: turn - was.turn, px: x - was.x, py: y - was.y,
    } }));
    if (phase === 2) last = null;
  };
  const span = () => {
    const [a, b] = fingers;
    const dx = b.x - a.x, dy = b.y - a.y;
    return { d: Math.max(Math.hypot(dx, dy), 1), a: Math.atan2(dy, dx) * 180 / Math.PI, x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
  };
  el.addEventListener("pointerdown", (e) => {
    if (e.pointerType !== "touch" || e.$pinch || off(e.target) || fingers.length === 2) return;
    e.$pinch = el;
    fingers.push({ id: e.pointerId, x: e.clientX, y: e.clientY });
    if (fingers.length === 2) { from = span(); angle = from.a; turned = 0; on = false; }
  });
  el.addEventListener("pointermove", (e) => {
    const f = fingers.find((f) => f.id === e.pointerId);
    if (!f) return;
    f.x = e.clientX; f.y = e.clientY;
    if (fingers.length < 2) return;
    const now = span();
    turned += ((now.a - angle + 540) % 360) - 180;
    angle = now.a;
    const turn = turned;
    if (!on) {
      const moved = Math.hypot(now.x - from.x, now.y - from.y);
      if (Math.max(moved, Math.abs(now.d - from.d), Math.abs(turn) * Math.PI / 180 * from.d / 2) < slop(e)) return;
      on = true;
      claimed = true;
      for (const g of fingers) try { el.setPointerCapture(g.id); } catch (_) {}
      return send(0, now.x, now.y, now.d / from.d, turn);
    }
    send(1, now.x, now.y, now.d / from.d, turn);
  });
  const lift = (e) => {
    const at = fingers.findIndex((f) => f.id === e.pointerId);
    if (at < 0) return;
    if (on) { const now = span(); send(2, now.x, now.y, now.d / from.d, last.turn); on = false; }
    fingers.splice(at, 1);
  };
  el.addEventListener("pointerup", lift);
  el.addEventListener("pointercancel", lift);
  let gesture = null;
  const pad = (e) => {
    e.preventDefault();
    if (e.$pinch || fingers.length) return;
    e.$pinch = el;
    if (e.type === "gesturestart") gesture = true;
    if (!gesture) return;
    send(e.type === "gesturestart" ? 0 : e.type === "gestureend" ? 2 : 1, e.clientX, e.clientY, e.scale || 1, e.rotation || 0);
    if (e.type === "gestureend") gesture = null;
  };
  el.addEventListener("gesturestart", pad);
  el.addEventListener("gesturechange", pad);
  el.addEventListener("gestureend", pad);
  el.addEventListener("wheel", (e) => {
    if (!e.ctrlKey || e.$pinch || off(e.target)) return;
    e.preventDefault();
    e.$pinch = el;
    const going = !!last && !!rest;
    const scale = (going ? last.scale : 1) * Math.exp(-e.deltaY / 100);
    send(going ? 1 : 0, e.clientX, e.clientY, scale, going ? last.turn : 0);
    clearTimeout(rest);
    rest = setTimeout(() => { rest = 0; send(2, last.x, last.y, last.scale, last.turn); }, 150);
  }, { passive: false });
};

// Makes `el` a camera view: it shows the stream of the camera its
// `data-vx-device` or `data-vx-facing` names (the browser's default without
// either), asked of the browser, which asks its reader once. Paused
// (`data-vx-paused`) it lets the camera go; a camera it cannot have fires
// `error`. Called again, it asks for another camera only when they changed.
const camera = (el) => {
  const stop = () => {
    for (const t of el.srcObject?.getTracks?.() ?? []) t.stop();
    el.srcObject = null;
  };
  if (el.dataset.vxPaused !== undefined || !navigator.mediaDevices) {
    el.$camera = null;
    el.pause();
    stop();
    if (!navigator.mediaDevices) el.dispatchEvent(new Event("error"));
    return;
  }
  const device = el.dataset.vxDevice, facing = el.dataset.vxFacing;
  const key = (device ?? "") + "|" + (facing ?? "");
  if (el.$camera === key) return;
  el.$camera = key;
  stop();
  const video = device ? { deviceId: { exact: device } } : facing ? { facingMode: facing } : true;
  navigator.mediaDevices.getUserMedia({ video, audio: false }).then((stream) => {
    if (el.$camera !== key) {
      for (const t of stream.getTracks()) t.stop();
      return;
    }
    el.srcObject = stream;
    mirrored(el);
    el.play().catch(() => {});
  }, () => {
    if (el.$camera === key) el.dispatchEvent(new Event("error"));
  });
};
// Shows camera view `el` flipped left to right as its `data-vx-mirror`
// says, else when its camera faces the user, as a laptop's does.
const mirrored = (el) => {
  const asked = el.dataset.vxMirror;
  const track = el.srcObject?.getVideoTracks?.()[0];
  const facing = track?.getSettings?.().facingMode ?? el.dataset.vxFacing;
  const on = asked === undefined ? facing !== "environment" : asked === "1";
  el.style.scale = on ? "-1 1" : "";
};

// Makes `el` a viewer of its first element child: a drag pans the child, a
// pinch zooms it about the pinch's middle as the middle's travel pans it, a
// wheel pans it, arrows and page keys pan it, `+` and `-` zoom it and `0` or
// Home put it back, and a double click zooms to `data-double-tap` (2.5 when
// absent, none when empty) and back. The child keeps its edges at or past
// `el`'s while larger and its middle on `el`'s while smaller,
// `data-pan-margin` further either way, and its scale within
// `data-min-scale`..`data-max-scale` (1 and 4 when absent); a drag that
// lifts moving coasts on, and a zoom asked for glides.
const viewer = (el) => {
  if (el.$viewer) return;
  draggable(el);
  pinchable(el);
  if (getComputedStyle(el).position === "static") el.style.position = "relative";
  const v = { s: 1, x: 0, y: 0, run: null, pinched: false };
  el.$viewer = v;
  const num = (name, d) => { const n = parseFloat(el.dataset[name]); return Number.isFinite(n) ? n : d; };
  const least = () => num("minScale", 1);
  const most = () => Math.max(num("maxScale", 4), least());
  const rest = () => Math.min(Math.max(1, least()), most());
  const kid = () => el.firstElementChild;
  const within = (n, [a, b]) => Math.min(Math.max(n, a), b);
  // The least and greatest pan across (axis 0) or down at scale `s`: the
  // shift of the child's corner from where it was laid out.
  const span = (s, axis) => {
    const c = kid();
    if (!c) return [0, 0];
    const [ve, c0, ce] = axis
      ? [el.clientHeight, c.offsetTop, c.offsetHeight]
      : [el.clientWidth, c.offsetLeft, c.offsetWidth];
    const shown = ce * s, m = num("panMargin", 0);
    const [a, b] = shown >= ve ? [ve - shown, 0] : [(ve - shown) / 2, (ve - shown) / 2];
    return [a - m - c0, b + m - c0];
  };
  const placed = (p) => "translate(" + p.x + "px, " + p.y + "px) scale(" + p.s + ")";
  const show = () => {
    const c = kid();
    if (!c) return;
    c.style.transformOrigin = "0 0";
    c.style.transform = placed(v);
  };
  // Ends the motion under way where it has come to.
  const stop = () => {
    if (!v.run) return;
    const { a, at, from, seconds } = v.run;
    v.run = null;
    Object.assign(v, at(Math.min((performance.now() - from) / 1000, seconds)));
    a.cancel();
    show();
  };
  // Moves the child along the path `at(t)` traces over `seconds`, played
  // by the browser from samples a sixtieth of a second apart; it rests
  // where the path ends.
  const travel = (at, seconds) => {
    stop();
    const c = kid();
    if (!c) return;
    const n = Math.max(Math.ceil(seconds * 60), 1);
    const frames = [];
    for (let i = 0; i <= n; i++) frames.push({ transform: placed(at(seconds * i / n)) });
    Object.assign(v, at(seconds));
    show();
    if (seconds > 0) v.run = { a: c.animate(frames, { duration: seconds * 1000, easing: "linear" }), at, from: performance.now(), seconds };
  };
  const fit = () => {
    stop();
    v.s = within(v.s, [least(), most()]);
    v.x = within(v.x, span(v.s, 0));
    v.y = within(v.y, span(v.s, 1));
    show();
  };
  // A point of the page, and el's middle, from where the child was laid out.
  const local = (px, py) => {
    const r = el.getBoundingClientRect(), c = kid();
    return [px - r.left - el.clientLeft - c.offsetLeft, py - r.top - el.clientTop - c.offsetTop];
  };
  const middle = () => { const c = kid(); return [el.clientWidth / 2 - c.offsetLeft, el.clientHeight / 2 - c.offsetTop]; };
  // Scales by `k` about (fx, fy), within the limits.
  const zoom = (k, fx, fy) => {
    const s = within(v.s * k, [least(), most()]);
    const r = s / v.s;
    v.x = fx - (fx - v.x) * r;
    v.y = fy - (fy - v.y) * r;
    v.s = s;
  };
  // Glides to scale `s` keeping what shows at (fx, fy) there, or with it
  // heading where pan `to` puts it: the scale evenly in proportion, the
  // point straight, along a critically damped spring.
  const glide = (s, fx, fy, to) => {
    stop();
    if (!kid()) return;
    s = within(s, [least(), most()]);
    const q = [(fx - v.x) / v.s, (fy - v.y) / v.s];
    const [tx, ty] = to ?? [fx - q[0] * s, fy - q[1] * s];
    const x1 = within(tx, span(s, 0)), y1 = within(ty, span(s, 1));
    const s0 = v.s, end = [x1 + q[0] * s, y1 + q[1] * s];
    const at = (t) => {
      const left = (1 + 14 * t) * Math.exp(-14 * t);
      if (left < 1e-3) return { s, x: x1, y: y1 };
      const p = 1 - left, k = Math.exp(Math.log(s0) + (Math.log(s) - Math.log(s0)) * p);
      return { s: k, x: fx + (end[0] - fx) * p - q[0] * k, y: fy + (end[1] - fy) * p - q[1] * k };
    };
    travel(at, reduced.matches ? 0 : 0.66);
  };
  // Coasts on at (vx, vy) px/s, slowing as a native scroller does, each
  // axis until it rests or meets the bounds.
  const coast = (vx, vy) => {
    stop();
    const speed = Math.hypot(vx, vy);
    if (speed < 50) return;
    const lambda = -Math.log(0.998) * 1000;
    const from = { s: v.s, x: v.x, y: v.y }, ax = span(v.s, 0), ay = span(v.s, 1);
    const at = (t) => {
      const gone = (1 - Math.exp(-lambda * t)) / lambda;
      return { s: from.s, x: within(from.x + vx * gone, ax), y: within(from.y + vy * gone, ay) };
    };
    travel(at, Math.log(speed / 5) / lambda);
  };
  el.addEventListener("viso-drag", (e) => {
    if (e.target !== el) return;
    const d = e.detail;
    if (d.phase === 0) v.pinched = false;
    if (v.pinched) return;
    stop();
    v.x += d.dx;
    v.y += d.dy;
    fit();
    if (d.phase === 2) coast(d.vx, d.vy);
  });
  el.addEventListener("viso-pinch", (e) => {
    if (e.target !== el) return;
    const d = e.detail;
    v.pinched = true;
    stop();
    if (d.phase === 2) return;
    v.x += d.px;
    v.y += d.py;
    const [fx, fy] = local(d.x, d.y);
    zoom(d.ds, fx, fy);
    fit();
  });
  el.addEventListener("wheel", (e) => {
    if (e.ctrlKey || e.$viewer || off(e.target)) return;
    stop();
    const k = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? el.clientHeight : 1;
    const x = within(v.x - e.deltaX * k, span(v.s, 0)), y = within(v.y - e.deltaY * k, span(v.s, 1));
    if (x === v.x && y === v.y) return;
    e.preventDefault();
    e.$viewer = el;
    v.x = x;
    v.y = y;
    show();
  }, { passive: false });
  el.addEventListener("dblclick", (e) => {
    const to = el.dataset.doubleTap;
    if (to === "" || off(e.target) || !kid()) return;
    if (v.s > rest() * 1.01) return glide(rest(), ...middle(), [0, 0]);
    glide(to === undefined ? 2.5 : parseFloat(to), ...local(e.clientX, e.clientY));
  });
  el.addEventListener("keydown", (e) => {
    if (e.ctrlKey || e.metaKey || e.altKey || off(e.target) || !kid()) return;
    const line = 40, page = el.clientHeight * 0.875;
    if (e.key === "+" || e.key === "=") { e.preventDefault(); return glide(v.s * 1.25, ...middle()); }
    if (e.shiftKey) return;
    if (e.key === "-") { e.preventDefault(); return glide(v.s / 1.25, ...middle()); }
    if (e.key === "0" || e.key === "Home") { e.preventDefault(); return glide(rest(), ...middle(), [0, 0]); }
    const by = { ArrowLeft: [line, 0], ArrowRight: [-line, 0], ArrowUp: [0, line], ArrowDown: [0, -line], PageUp: [0, page], PageDown: [0, -page] }[e.key];
    if (!by) return;
    const to = [within(v.x + by[0], span(v.s, 0)), within(v.y + by[1], span(v.s, 1))];
    if (to[0] === v.x && to[1] === v.y) return;
    e.preventDefault();
    glide(v.s, ...middle(), to);
  });
  new ResizeObserver(fit).observe(el);
  new MutationObserver(fit).observe(el, { attributes: true, attributeFilter: ["data-min-scale", "data-max-scale", "data-pan-margin"] });
  fit();
};

// The swipe row standing open, if one does: one at a time.
let swiped = null;

// Makes `el` a swipe row: a drag across slides its first child not marked
// `data-swipe-edge` aside, and the panel marked for the edge it uncovers
// (0 leading, 1 trailing) slides in after it, its edge held to the
// content's. Let go past half the panel's actions or flicked open, it
// springs open on them, else shut; an edge `data-full-swipe` names (bit 0
// leading, bit 1 trailing; trailing unless set) lets a swipe carry the
// content off, which tells the row by a `viso-fullswipe` event before it
// springs back. A trackpad's swipe across slides it too, a press elsewhere
// in its list or a scroll shuts it, the press clicking nothing, and a press
// on an action shuts it once it clicks. The focus coming to an action opens
// the row on it; Escape shuts it.
// A sheet: a dialog whose first child, its panel, lies against the edge
// its `data-sheet-edge` names (0 the bottom, 1 the top, 2 the leading, 3
// the trailing) and slides in, while open, to the detent its `data-detent`
// picks among its `data-detents` — each `N%` of the window along its axis,
// `Npx` or `full`, none for all of the panel — and out before it closes, its
// own box, the scrim, darkening as it comes. A drag on the panel along its
// axis carries it; let go, it springs to the detent nearest where its speed
// would carry it, telling which by a `viso-detent` event `{ detent }`, or,
// past its least detent unless `data-dismissible` is "false", off, telling
// a `viso-dismiss` event; one the view keeps open springs back.
const sheet = (el) => {
  if (el.$sheet) return;
  const panel = () => el.firstElementChild;
  const edge = () => Number(el.dataset.sheetEdge || 0);
  const down = () => edge() < 2;
  const rtl = () => getComputedStyle(el).direction === "rtl";
  // The way the panel slides in along its axis: 1 down or right.
  const inward = () => { const e = edge(); return e === 0 ? -1 : e === 1 ? 1 : (e === 2) !== rtl() ? 1 : -1; };
  const full = () => { const p = panel(); return p ? (down() ? p.offsetHeight : p.offsetWidth) : 0; };
  const surface = () => (down() ? window.innerHeight : window.innerWidth);
  const extents = () => {
    const f = full(), all = (el.dataset.detents || "full").split(" ").filter(Boolean);
    return all.map((d) => Math.min(Math.max(d === "full" ? f : d.endsWith("%") ? (parseFloat(d) / 100) * surface() : parseFloat(d), 0), f));
  };
  const dismissible = () => el.dataset.dismissible !== "false";
  const v = { shown: 0, raw: 0, run: null, open: false, held: false, detent: Number(el.dataset.detent || 0), scrim: getComputedStyle(el).backgroundColor };
  el.$sheet = v;
  const aim = () => { const e = extents(); return e[Math.min(v.detent, e.length - 1)]; };
  // The panel's slide and the scrim's dim with `s` of it showing.
  const slide = (s) => (down() ? "translateY(" : "translateX(") + -inward() * Math.max(full() - s, 0) + "px)";
  const dim = (s) => {
    const least = extents()[0];
    const d = least > 0 ? Math.min(Math.max(s / least, 0), 1) : v.open ? 1 : 0;
    return "color-mix(in srgb, " + v.scrim + " " + d * 100 + "%, transparent)";
  };
  const show = () => {
    const p = panel();
    if (p) p.style.transform = slide(v.shown);
    el.style.backgroundColor = dim(v.shown);
  };
  // Ends the spring under way where it has come to.
  const stop = () => {
    if (!v.run) return;
    const { anims, at, from, seconds, timer } = v.run;
    v.run = null;
    clearTimeout(timer);
    v.shown = at(Math.min((performance.now() - from) / 1000, seconds));
    for (const a of anims) a.cancel();
    show();
  };
  // Springs to show `to` from moving at `speed` px/s of it, critically
  // damped; then `then` runs.
  const spring = (to, speed, then) => {
    stop();
    const e0 = v.shown - to;
    const at = (t) => to + (e0 + (speed + 20 * e0) * t) * Math.exp(-20 * t);
    let seconds = 0;
    if (!reduced.matches) while (seconds < 1.5 && (Math.abs(at(seconds) - to) > 0.5 || seconds < 0.05)) seconds += 1 / 60;
    v.shown = to;
    show();
    if (seconds === 0) return then && then();
    const n = Math.ceil(seconds * 60), slides = [], dims = [];
    for (let i = 0; i <= n; i++) {
      const s = at((seconds * i) / n);
      slides.push({ transform: slide(s) });
      dims.push({ backgroundColor: dim(s) });
    }
    const timing = { duration: seconds * 1000, easing: "linear" };
    const anims = [el.animate(dims, timing)];
    if (panel()) anims.push(panel().animate(slides, timing));
    const timer = setTimeout(() => { if (v.run && v.run.timer === timer) { v.run = null; if (then) then(); } }, seconds * 1000);
    v.run = { anims, at, from: performance.now(), seconds, timer };
  };
  v.present = (on) => {
    if (on === v.open) return;
    v.open = on;
    if (on) {
      if (!el.open) el.showModal();
      show();
      if (!v.held) spring(aim(), 0);
    } else if (!v.held) spring(0, 0, () => { if (!v.open && el.open) el.close(); });
  };
  const rest = () => { if (v.open && !v.held) spring(aim(), 0); };
  // How much shows for a drag that travelled `raw`: all of it at most, and
  // below its least detent eased unless a drag dismisses it.
  const shape = (raw) => {
    const least = extents()[0];
    raw = Math.min(raw, full());
    if (raw >= least || dismissible()) return Math.max(raw, 0);
    const d = least - raw, m = least * 0.15;
    return least - (m * 0.55 * d) / (m * 0.55 + d);
  };
  const release = (speed) => {
    const e = extents(), projected = v.shown + speed * 0.2;
    const near = e.map((x, i) => [x, i]);
    if (dismissible()) near.push([0, -1]);
    let best = near[0];
    for (const c of near) if (Math.abs(c[0] - projected) < Math.abs(best[0] - projected)) best = c;
    if (best[1] < 0) {
      el.dispatchEvent(new CustomEvent("viso-dismiss"));
      if (v.open) spring(aim(), speed);
      else spring(0, speed, () => { if (!v.open && el.open) el.close(); });
      return;
    }
    const moved = best[1] !== v.detent;
    v.detent = best[1];
    spring(best[0], speed);
    if (moved) el.dispatchEvent(new CustomEvent("viso-detent", { detail: { detent: best[1] } }));
  };
  const p = panel();
  if (p) {
    p.dataset.dragAxis = down() ? "column" : "row";
    draggable(p);
    p.addEventListener("viso-drag", (e) => {
      if (e.target !== p) return;
      const d = e.detail;
      if (d.phase === 0) {
        stop();
        v.held = true;
        v.raw = v.shown;
      }
      v.raw += (down() ? d.dy : d.dx) * inward();
      v.shown = shape(v.raw);
      show();
      if (d.phase === 2) {
        v.held = false;
        release((down() ? d.vy : d.vx) * inward());
      }
    });
  }
  new MutationObserver(() => {
    const d = Number(el.dataset.detent || 0);
    if (d !== v.detent) { v.detent = d; rest(); }
  }).observe(el, { attributes: true, attributeFilter: ["data-detent"] });
  if (el.hasAttribute("open")) {
    el.removeAttribute("open");
    v.present(true);
  } else show();
};

const swipeRow = (el) => {
  if (el.$swipe) return;
  el.dataset.dragAxis = "row";
  draggable(el);
  if (getComputedStyle(el).position === "static") el.style.position = "relative";
  el.style.isolation = "isolate";
  const v = { x: 0, raw: 0, run: null, full: null, focus: false, pad: null };
  el.$swipe = v;
  const panel = (edge) => [...el.children].find((c) => c.dataset.swipeEdge === String(edge));
  const content = () => [...el.children].find((c) => c.dataset.swipeEdge === undefined);
  const rtl = () => getComputedStyle(el).direction === "rtl";
  // The way the content slides to show `edge`'s actions: 1 to the right.
  const sign = (edge) => ((edge === 0) !== rtl() ? 1 : -1);
  const edgeOf = (x) => (x * sign(0) > 0 ? 0 : 1);
  const width = () => el.clientWidth;
  const reach = (edge) => {
    const p = panel(edge);
    if (!p || !p.children.length) return 0;
    let lo = Infinity, hi = -Infinity;
    for (const c of p.children) {
      lo = Math.min(lo, c.offsetLeft);
      hi = Math.max(hi, c.offsetLeft + c.offsetWidth);
    }
    return Math.min(Math.max(hi - lo, 0), width());
  };
  const full = (edge) => (Number(el.dataset.fullSwipe || 2) & (edge === 0 ? 1 : 2)) !== 0 && reach(edge) > 0;
  const mark = (edge) => Math.min(Math.max(width() * 0.55, reach(edge) + 48), width() * 0.95);
  const stretch = () => width() * 0.15;
  const eased = (m, d) => (d <= 0 ? 0 : (m * 0.55 * d) / (m * 0.55 + d));
  const uneased = (m, d) => { if (d <= 0) return 0; const s = Math.min(m, d * 0.999); return (s * d) / (0.55 * (d - s)); };
  // Where the content shows for a travel of `raw`, and back.
  const shape = (raw) => {
    if (raw === 0) return 0;
    const edge = edgeOf(raw), r = reach(edge), m = Math.abs(raw);
    if (r === 0) return 0;
    const shown = full(edge) ? Math.min(m, width()) : m <= r ? m : r + eased(m - r, stretch());
    return Math.sign(raw) * shown;
  };
  const unshape = (x) => {
    if (x === 0) return 0;
    const edge = edgeOf(x), r = reach(edge), m = Math.abs(x);
    return Math.sign(x) * (full(edge) || m <= r ? m : r + uneased(m - r, stretch()));
  };
  const parts = () => [[content(), 0], [panel(0), -sign(0)], [panel(1), -sign(1)]].filter(([n]) => n);
  const put = (x) => {
    for (const [n, away] of parts()) n.style.transform = "translateX(" + (x + away * width()) + "px)";
  };
  const show = () => put(v.x);
  // Ends the spring under way where it has come to.
  const stop = () => {
    if (!v.run) return;
    const { anims, at, from, seconds, timer } = v.run;
    v.run = null;
    clearTimeout(timer);
    v.x = at(Math.min((performance.now() - from) / 1000, seconds));
    for (const a of anims) a.cancel();
    show();
  };
  // Springs to `to` from moving at `speed` px/s, critically damped; then
  // `then` runs.
  const spring = (to, speed, then) => {
    stop();
    const e0 = v.x - to;
    const at = (t) => to + (e0 + (speed + 20 * e0) * t) * Math.exp(-20 * t);
    let seconds = 0;
    if (!reduced.matches) while (seconds < 1.5 && (Math.abs(at(seconds) - to) > 0.5 || seconds < 0.05)) seconds += 1 / 60;
    v.x = to;
    show();
    if (seconds === 0) return then && then();
    const n = Math.ceil(seconds * 60);
    const anims = parts().map(([node, away]) => {
      const frames = [];
      for (let i = 0; i <= n; i++) frames.push({ transform: "translateX(" + (at((seconds * i) / n) + away * width()) + "px)" });
      return node.animate(frames, { duration: seconds * 1000, easing: "linear" });
    });
    const timer = setTimeout(() => { if (v.run && v.run.timer === timer) { v.run = null; if (then) then(); } }, seconds * 1000);
    v.run = { anims, at, from: performance.now(), seconds, timer };
  };
  const shut = () => {
    v.full = null;
    v.focus = false;
    if (swiped === el) swiped = null;
    spring(0, 0);
  };
  const open = (edge, speed = 0) => {
    if (swiped && swiped !== el) swiped.$swipe.shut();
    swiped = el;
    spring(sign(edge) * reach(edge), speed);
  };
  v.shut = shut;
  // Lets the row go moving at `speed` px/s across.
  const release = (speed) => {
    const x = v.x, way = x !== 0 ? x : speed;
    if (way === 0) return shut();
    const edge = edgeOf(way), r = reach(edge);
    if (x !== 0 && full(edge) && Math.abs(x) >= mark(edge)) {
      if (swiped && swiped !== el) swiped.$swipe.shut();
      swiped = el;
      return spring(sign(edge) * width(), speed, () => {
        el.dispatchEvent(new CustomEvent("viso-fullswipe", { detail: { edge } }));
        setTimeout(shut, reduced.matches ? 0 : 150);
      });
    }
    const opens = Math.abs(speed) >= 400 ? x === 0 || speed * x > 0 : r > 0 && Math.abs(x) >= r / 2;
    if (opens && r > 0) open(edge, speed);
    else shut();
  };
  el.addEventListener("viso-drag", (e) => {
    if (e.target !== el) return;
    const d = e.detail;
    if (d.phase === 0) {
      stop();
      if (swiped && swiped !== el) swiped.$swipe.shut();
      v.raw = unshape(v.x);
    }
    v.raw += d.dx;
    v.x = shape(v.raw);
    show();
    if (d.phase === 2) release(d.vx);
  });
  // A trackpad's swipe across, until it rests a moment.
  el.addEventListener("wheel", (e) => {
    if (e.ctrlKey || e.deltaMode !== 0 || e.$swipe) return;
    if (!v.pad && Math.abs(e.deltaX) <= Math.abs(e.deltaY) * 1.5) return;
    e.$swipe = el;
    e.preventDefault();
    if (!v.pad) {
      stop();
      if (swiped && swiped !== el) swiped.$swipe.shut();
      v.raw = unshape(v.x);
      v.pad = { timer: 0, seen: [] };
    }
    clearTimeout(v.pad.timer);
    v.raw -= e.deltaX;
    v.x = shape(v.raw);
    v.pad.seen.push([e.timeStamp, v.raw]);
    show();
    v.pad.timer = setTimeout(() => {
      const seen = v.pad.seen;
      v.pad = null;
      const a = seen[Math.max(seen.length - 4, 0)], b = seen[seen.length - 1];
      const dt = (b[0] - a[0]) / 1000;
      release(dt > 0.01 ? (b[1] - a[1]) / dt : 0);
    }, 120);
  }, { passive: false });
  // A press on an action shuts the row once the action clicked.
  el.addEventListener("click", (e) => {
    if (swiped === el && [panel(0), panel(1)].some((p) => p && p.contains(e.target))) shut();
  });
  el.addEventListener("focusin", (e) => {
    const edge = [0, 1].find((k) => panel(k) && panel(k).contains(e.target));
    if (edge === undefined) return;
    if (swiped !== el || edgeOf(v.x) !== edge) {
      open(edge);
      v.focus = true;
    }
  });
  el.addEventListener("focusout", (e) => {
    if (!v.focus) return;
    if ([panel(0), panel(1)].some((p) => p && p.contains(e.relatedTarget))) return;
    shut();
  });
  el.addEventListener("keydown", (e) => {
    if (e.key !== "Escape" || swiped !== el) return;
    e.preventDefault();
    e.stopPropagation();
    if ([panel(0), panel(1)].some((p) => p && p.contains(document.activeElement)) && el.tabIndex >= 0) el.focus();
    shut();
  });
  for (const [edge, p] of [[0, panel(0)], [1, panel(1)]]) {
    if (p) p.style.justifyContent = edge === 0 ? "flex-end" : "flex-start";
  }
  new ResizeObserver(() => { if (!v.run && v.x !== 0) v.x = sign(edgeOf(v.x)) * reach(edgeOf(v.x)); show(); }).observe(el);
  show();
};

// A press anywhere but on the open swipe row's actions shuts it, and one
// within its list clicks nothing; a scroll elsewhere shuts it too.
addEventListener("pointerdown", (e) => {
  const row = swiped;
  if (!row) return;
  const on = [...row.children].some((c) => c.dataset.swipeEdge !== undefined && c.contains(e.target));
  if (on) return;
  let list = row.parentElement;
  while (list && list !== document.body && !/(auto|scroll)/.test(getComputedStyle(list).overflowY + getComputedStyle(list).overflowX)) list = list.parentElement;
  if ((list && list !== document.body ? list : row).contains(e.target)) claimed = true;
  row.$swipe.shut();
}, true);
addEventListener("wheel", (e) => {
  if (swiped && !e.$swipe && !swiped.contains(e.target)) swiped.$swipe.shut();
}, { passive: true });

// Makes `el` a reorderable list: its rows are its children, or its
// content's if it scrolls one. A drag on a `data-reorder-handle` within a
// row lifts the row above the others and carries it along the list, the
// rows between sliding aside by their midpoints; held near either end of
// the viewport it scrolls, and a wheel scrolls it, the row kept under the
// pointer. Let go, it slides into its gap and tells the list where it went
// by a `viso-reorder` event `{ from, to }`, `to` among the rows without it;
// back in its place it tells nothing. Space or Enter on a handle lifts its
// row, the arrows along the list move it, Space or Enter drops it, and
// Escape, a press or the focus leaving puts it back.
const reorderList = (el) => {
  if (el.$reorder) return;
  el.$reorder = true;
  const scrolls = (n) => /(auto|scroll)/.test(getComputedStyle(n).overflowY + getComputedStyle(n).overflowX);
  const rowsOf = () => (scrolls(el) && el.children.length === 1 ? el.firstElementChild : el);
  const slide = 200;
  let lift = null;
  const at = (e) => (lift.y ? e.clientY : e.clientX);
  const origin = () => { const r = lift.rows.getBoundingClientRect(); return lift.y ? r.top : r.left; };
  const put = (n, by, moving) => {
    n.style.transition = moving && !reduced.matches ? "transform " + slide + "ms cubic-bezier(0.2, 0, 0, 1)" : "";
    n.style.transform = by ? (lift.y ? "translateY(" : "translateX(") + by + "px)" : "";
  };
  const begin = (handle, pointer, grip) => {
    const rows = rowsOf();
    const row = [...rows.children].find((c) => c.contains(handle));
    if (!row) return;
    const s = getComputedStyle(rows);
    const y = !(s.display.includes("flex") && s.flexDirection.startsWith("row"));
    const all = [...rows.children];
    const o = y ? rows.getBoundingClientRect().top : rows.getBoundingClientRect().left;
    const spans = all.map((c) => { const r = c.getBoundingClientRect(); return y ? [r.top - o, r.bottom - o] : [r.left - o, r.right - o]; });
    const from = all.indexOf(row);
    // Which way the places grow, and the gap to a neighbour.
    const flow = all.length > 1 && spans[Math.min(from + 1, all.length - 1)][0] < spans[Math.max(from - 1, 0)][0] ? -1 : 1;
    const next = spans[from + 1], prev = spans[from - 1], [lo, hi] = spans[from];
    const gap = flow > 0 ? (next ? next[0] - hi : prev ? lo - prev[1] : 0) : (next ? lo - next[1] : prev ? prev[0] - hi : 0);
    let scroller = rows;
    while (scroller && scroller !== document.body && !scrolls(scroller)) scroller = scroller.parentElement;
    lift = { rows, row, all, spans, from, to: from, y, flow, pitch: hi - lo + Math.max(gap, 0), pointer, grip: 0, at: 0, timer: 0, handle, scroller: scroller !== document.body ? scroller : null, offset: 0 };
    lift.grip = grip - o;
    if (getComputedStyle(row).position === "static") row.style.position = "relative";
    row.style.zIndex = "1";
    lift.timer = setInterval(edge, 16);
    if (lift.scroller) lift.scroller.addEventListener("scroll", follow, { passive: true });
  };
  // Where the rows slide: the lifted one by `offset`, the others aside of
  // the gap it would land in.
  const lay = () => {
    const { all, spans, from, flow, pitch, row } = lift;
    for (let j = 0; j < all.length; j++) {
      if (all[j] === row) continue;
      const by = from < j && j <= lift.to ? -flow * pitch : lift.to <= j && j < from ? flow * pitch : 0;
      put(all[j], by, true);
    }
  };
  const landing = () => {
    const { spans, from, to, flow } = lift;
    if (to === from) return 0;
    return to > from ? (flow > 0 ? spans[to][1] - spans[from][1] : spans[to][0] - spans[from][0]) : (flow > 0 ? spans[to][0] - spans[from][0] : spans[to][1] - spans[from][1]);
  };
  const follow = () => {
    if (!lift || lift.pointer === null) return;
    const { spans, from } = lift;
    const last = spans.length - 1;
    const pos = lift.at - origin();
    const offset = Math.min(Math.max(pos - lift.grip, spans[0][0] - spans[from][0]), Math.max(spans[last][1] - spans[from][1], spans[0][0] - spans[from][0]));
    lift.offset = offset;
    const mid = (spans[from][0] + spans[from][1]) / 2 + offset;
    let to = 0;
    for (let j = 0; j < spans.length; j++) if (j !== from && ((spans[j][0] + spans[j][1]) / 2 - mid) * lift.flow < 0) to++;
    lift.to = to;
    put(lift.row, offset, false);
    lay();
  };
  // Held near an end of the viewport, the pointer scrolls it.
  const edge = () => {
    if (!lift || lift.pointer === null || !lift.scroller) return;
    const r = lift.scroller.getBoundingClientRect();
    const [v0, v1] = lift.y ? [r.top, r.bottom] : [r.left, r.right];
    const band = Math.min(40, (v1 - v0) / 3);
    const near = Math.min((v0 + band - lift.at) / band, 1), far = Math.min((lift.at - (v1 - band)) / band, 1);
    const by = near > 0 ? -800 * near / 60 : far > 0 ? 800 * far / 60 : 0;
    if (!by) return;
    if (lift.y) lift.scroller.scrollTop += by;
    else lift.scroller.scrollLeft += by;
    follow();
  };
  const end = (tell) => {
    const l = lift;
    lift = null;
    clearInterval(l.timer);
    if (l.scroller) l.scroller.removeEventListener("scroll", follow);
    if (tell && l.to !== l.from) el.dispatchEvent(new CustomEvent("viso-reorder", { detail: { from: l.from, to: l.to } }));
    for (const n of l.all) { n.style.transition = ""; n.style.transform = ""; }
    l.row.style.zIndex = "";
  };
  const settle = (tell) => {
    const l = lift;
    l.pointer = null;
    l.settling = true;
    if (!tell) l.to = l.from;
    lay();
    put(l.row, tell ? landing() : 0, true);
    setTimeout(() => { if (lift === l) end(tell); }, reduced.matches ? 0 : slide);
  };
  el.addEventListener("pointerdown", (e) => {
    const handle = e.target.closest && e.target.closest("[data-reorder-handle]");
    if (e.button !== 0 || !handle || !el.contains(handle) || lift) return;
    e.preventDefault();
    e.stopPropagation();
    e.$drag = handle;
    const press = { id: e.pointerId, x: e.clientX, y: e.clientY, handle };
    const move = (m) => {
      if (m.pointerId !== press.id) return;
      if (!lift) {
        if (Math.hypot(m.clientX - press.x, m.clientY - press.y) < 4) return;
        begin(handle, press.id, 0);
        if (!lift) return;
        lift.grip = (lift.y ? press.y : press.x) - origin();
        try { handle.setPointerCapture(press.id); } catch {}
      }
      lift.at = at(m);
      follow();
    };
    const up = (u) => {
      if (u.pointerId !== press.id) return;
      removeEventListener("pointermove", move, true);
      removeEventListener("pointerup", up, true);
      removeEventListener("pointercancel", up, true);
      if (lift && lift.pointer === press.id) settle(u.type === "pointerup");
    };
    addEventListener("pointermove", move, true);
    addEventListener("pointerup", up, true);
    addEventListener("pointercancel", up, true);
  });
  el.addEventListener("keydown", (e) => {
    const handle = e.target.closest && e.target.closest("[data-reorder-handle]");
    if (!handle || !el.contains(handle)) return;
    const bare = !e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey;
    const toggle = bare && !e.repeat && (e.key === " " || e.key === "Enter");
    if (lift && lift.settling) return;
    if (lift && lift.pointer !== null) {
      if (e.key === "Escape") { e.preventDefault(); settle(false); }
      return;
    }
    if (!lift) {
      if (!toggle) return;
      e.preventDefault();
      begin(handle, null, 0);
      if (lift) lift.pointer = null;
      return;
    }
    if (e.key === "Escape") { e.preventDefault(); settle(false); return; }
    if (toggle) { e.preventDefault(); settle(true); return; }
    if (!bare) return;
    const dir = lift.y ? { ArrowUp: -1, ArrowDown: 1 }[e.key] : { ArrowLeft: -1, ArrowRight: 1 }[e.key];
    if (dir === undefined) return;
    e.preventDefault();
    lift.to = Math.min(Math.max(lift.to + dir * lift.flow, 0), lift.all.length - 1);
    lay();
    put(lift.row, landing(), true);
    lift.row.scrollIntoView && lift.all[lift.to].scrollIntoView({ block: "nearest", inline: "nearest" });
  });
  el.addEventListener("focusout", (e) => {
    if (lift && lift.pointer === null && !lift.settling && e.target === lift.handle) settle(false);
  });
  addEventListener("pointerdown", (e) => {
    if (lift && lift.pointer === null && !lift.settling) settle(false);
  }, true);
};

// Drag and drop, by the browser's own: a `data-drag` element is a source
// carrying its text as its `data-drag-kind` (`text` without one), its
// `data-drag-preview` child the image the drag shows; a `data-drop-accepts`
// element is a target taking the kinds it lists, one a line, the innermost
// under the pointer that takes one of the drag's being the one it is over,
// `data-viso-drop-over` meanwhile. A target hears a `viso-drop` event
// `{ phase, e, taken }` (`phase` 0 as the drag comes over it, 1 as it leaves,
// 2 as it is let go over it; `taken` the index among its kinds of the one it
// takes it as) and the source a `viso-dragend` `{ operation }`, the index of
// what the drop did, `null` for one put back. Files from outside the page
// come as kind `files`.
const DROPS = ["move", "copy", "link"];
const kindType = (kind) => ({ text: "text/plain", uri: "text/uri-list", files: "Files" })[kind] ?? "application/x-viso." + kind.toLowerCase();
const dropKinds = (el) => (el.dataset.dropAccepts ?? "").split("\n").filter(Boolean);
const takenBy = (el, dt) => {
  const types = [...dt.types];
  return dropKinds(el).findIndex((k) => types.includes(kindType(k)));
};
let dragDropping = false;
const dragDrop = () => {
  if (dragDropping) return;
  dragDropping = true;
  // The operation of a drop on this page; one elsewhere is the browser's.
  let carried = null, over = null, dropped = null;
  const tell = (el, phase, e) => el.dispatchEvent(new CustomEvent("viso-drop", { detail: { phase, e, taken: Math.max(takenBy(el, e.dataTransfer), 0) } }));
  const targetOf = (e) => {
    let el = e.target instanceof Element ? e.target : null;
    while (el && (el = el.closest("[data-drop-accepts]"))) {
      if (el !== carried && !off(el) && takenBy(el, e.dataTransfer) >= 0) return el;
      el = el.parentElement;
    }
    return null;
  };
  const come = (to, e) => {
    if (to === over) return;
    const was = over;
    over = to;
    if (was) { was.removeAttribute("data-viso-drop-over"); tell(was, 1, e); }
    if (to) { to.setAttribute("data-viso-drop-over", ""); tell(to, 0, e); }
  };
  document.addEventListener("dragstart", (e) => {
    const el = e.target instanceof Element ? e.target.closest("[data-drag]") : null;
    if (!el || off(el)) return;
    const kind = el.dataset.dragKind || "text", t = el.dataset.drag, dt = e.dataTransfer;
    if (kind === "uri") dt.setData("text/uri-list", t);
    dt.setData(kind === "uri" || kind === "files" ? "text/plain" : kindType(kind), t);
    dt.effectAllowed = "all";
    const preview = [...el.children].find((c) => c.hasAttribute("data-drag-preview"));
    if (preview) {
      const r = el.getBoundingClientRect();
      dt.setDragImage(preview, e.clientX - r.left, e.clientY - r.top);
    }
    el.setAttribute("data-viso-dragging", "");
    carried = el;
    dropped = null;
  });
  document.addEventListener("dragenter", (e) => { if (targetOf(e)) e.preventDefault(); });
  document.addEventListener("dragover", (e) => {
    const to = targetOf(e);
    come(to, e);
    if (!to) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = DROPS[Number(to.dataset.dropOperation || 0)] ?? "move";
  });
  // Leaving the page leaves the target.
  document.addEventListener("dragleave", (e) => { if (!e.relatedTarget) come(null, e); });
  document.addEventListener("drop", (e) => {
    const to = targetOf(e);
    if (!to) return;
    e.preventDefault();
    over = null;
    to.removeAttribute("data-viso-drop-over");
    dropped = Number(to.dataset.dropOperation || 0);
    tell(to, 2, e);
  });
  document.addEventListener("dragend", (e) => {
    come(null, e);
    const el = carried;
    carried = null;
    if (!el) return;
    el.removeAttribute("data-viso-dragging");
    const op = dropped ?? DROPS.indexOf(e.dataTransfer.dropEffect);
    el.dispatchEvent(new CustomEvent("viso-dragend", { detail: { operation: op < 0 ? null : op } }));
  });
};

// The `DropEvent` of a `viso-drop` on `el`: the text of the kind it takes
// the data as, readable only once let go, and the files of `files`.
const dropEvent = (el, d) => {
  const dt = d.e.dataTransfer, kind = dropKinds(el)[d.taken] ?? "text";
  const dropped = d.phase === 2;
  let t = dropped ? dt.getData(kind === "files" ? "text/plain" : kindType(kind)) : "";
  if (kind === "uri" && dropped) t = t.split("\n").find((l) => l && !l.startsWith("#")) ?? "";
  const files = kind !== "files" || !dropped ? [] : dt.files.length ? [...dt.files].map((f) => f.name) : t.split("\n").filter(Boolean);
  return rec([point(d.e, el), BigInt(d.taken), kind === "files" ? "" : t, files, BigInt(Number(el.dataset.dropOperation || 0))]);
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

// A stepper's bounds, number, step and whether it comes round past a bound.
const stepping = (el) => {
  const n = (a, d) => { const s = el.getAttribute(a); return s === null ? d : Number(s); };
  const lo = n("aria-valuemin", -Infinity), hi = n("aria-valuemax", Infinity);
  const step = Number(el.dataset.step || 1) || 1;
  return [lo, hi, n("aria-valuenow", 0), step, el.dataset.wrap === "true" && isFinite(lo) && isFinite(hi)];
};

// Steps stepper `el` by its halves and its keys, calling `report` with each
// number it steps to: a held half steps on after a pause, faster the longer
// it is held; the arrows step one, Page Up and Page Down ten, Home and End go
// to the bounds.
const stepper = (el, report, capture) => {
  const go = (input) => {
    const [lo, hi, now, step, wraps] = stepping(el);
    let next;
    if (input === "min") next = lo;
    else if (input === "max") next = hi;
    else {
      const [sign, times] = input;
      if (wraps && sign > 0 && now >= hi) next = lo;
      else if (wraps && sign < 0 && now <= lo) next = hi;
      else {
        const base = isFinite(lo) ? lo : 0;
        next = base + Math.round((now + sign * step * times - base) / step) * step;
      }
    }
    next = Math.min(Math.max(next, Math.min(lo, hi)), Math.max(lo, hi));
    if (!Number.isFinite(next) || next === now) return false;
    report(next);
    return true;
  };
  let hold = null;
  const stop = () => { clearTimeout(hold); hold = null; };
  const on = (sign, delay, steps) => {
    hold = setTimeout(() => { if (go([sign, 1])) on(sign, steps < 8 ? 120 : 50, steps + 1); else stop(); }, delay);
  };
  el.addEventListener("pointerdown", (e) => {
    const half = e.target.closest && e.target.closest(".vx-step");
    if (!half || e.button !== 0 || off(el)) return;
    e.preventDefault();
    el.focus();
    stop();
    const sign = Number(half.dataset.by);
    if (go([sign, 1])) on(sign, 500, 0);
  }, capture);
  for (const t of ["pointerup", "pointerleave", "pointercancel"]) el.addEventListener(t, stop, capture);
  el.addEventListener("keydown", (e) => {
    const back = getComputedStyle(el).direction === "rtl" ? 1 : -1;
    const keys = { ArrowUp: [1, 1], ArrowDown: [-1, 1], ArrowRight: [-back, 1], ArrowLeft: [back, 1], PageUp: [1, 10], PageDown: [-1, 10], Home: "min", End: "max" };
    const input = keys[e.key];
    if (!input || off(el)) return;
    e.preventDefault();
    go(input);
  }, capture);
};

// Whether a click on a removable chip `el` lands on its cross, which takes
// its end edge; a click a key made lands on none.
const crossed = (el, e) => {
  if (el.dataset.removable !== "true" || e.detail === 0) return false;
  const b = el.getBoundingClientRect(), trail = b.height * 1.0625;
  return getComputedStyle(el).direction === "rtl" ? e.clientX <= b.left + trail : e.clientX >= b.right - trail;
};

// The key shortcuts on the page. A key press nothing focused took is
// offered to those whose parent holds the focus, nearest first, then to the
// window's; the first whose chord it is takes it. While the focus takes
// text, a key held with no Control, Alt or Command is the text's.
const shortcuts = [];
const apple = /Mac|iPhone|iPad/.test(navigator.platform);
const chorded = (el, e) => {
  const chord = el.dataset.chord;
  if (!chord) return false;
  const parts = chord.split("+");
  const name = parts.pop() || "+";
  const held = new Set(parts);
  const primary = held.has("primary");
  if (e.shiftKey !== held.has("shift") || e.altKey !== held.has("alt")) return false;
  if (e.ctrlKey !== (held.has("control") || (primary && !apple)) || e.metaKey !== (primary && apple)) return false;
  if (/^(Key[A-Z]|Digit[0-9])$/.test(name)) return e.code === name;
  return e.key === name;
};
const shortcut = (el, fire) => {
  el.$shortcut = fire;
  shortcuts.push(el);
  if (shortcuts.length > 1) return;
  addEventListener("keydown", (e) => {
    if (e.defaultPrevented || e.isComposing) return;
    const focus = document.activeElement === document.body ? null : document.activeElement;
    const texty = focus && (focus.isContentEditable || focus.matches("input, textarea, select"));
    if (texty && !e.ctrlKey && !e.altKey && !e.metaKey) return;
    const live = shortcuts.filter((s) => s.isConnected && s.parentElement.checkVisibility?.() !== false);
    const order = [];
    for (let n = focus ?? document.body.firstElementChild; n; n = n.parentElement) {
      for (const s of live) if (s.dataset.scope !== "window" && s.parentElement === n) order.push(s);
    }
    for (const s of live) if (s.dataset.scope === "window") order.push(s);
    const hit = order.find((s) => chorded(s, e));
    if (!hit) return;
    e.preventDefault();
    if (!e.repeat) hit.$shortcut(null);
  });
};

const DOM = ["click", "click", "pointerdown", "pointermove", "pointerup", "pointerenter", "pointerleave", "keydown", "keyup"];

// Calls `fire` with the payload of each `route` event of `el`, a control
// of kind `control`. An event aimed into a disabled subtree reaches no
// listener, but a pointer still leaves what it left.
const listen = (el, route, capture, control, fire) => {
  const live = (f) => (e) => { if (route === 6 || !off(e.target)) f(e); };
  if (route === 1) {
    // Where a double tap is heard too, a tap waits out the double-click time
    // and runs only when no second press came.
    el.addEventListener("click", live((e) => {
      if (!el.$double) return fire(pointer(1, e, el));
      clearTimeout(el.$tap);
      if (e.detail > 1) return;
      const tap = pointer(1, e, el);
      el.$tap = setTimeout(() => fire(tap), 500);
    }), capture);
  } else if (route <= 6) {
    el.addEventListener(DOM[route], live((e) => {
      if (route === 0 && crossed(el, e)) return;
      fire(pointer(route, e, el));
    }), capture);
  } else if (route <= 8) {
    el.addEventListener(DOM[route], live((e) => fire(rec([key(e), bool(e.repeat), mods(e)]))), capture);
  } else if (route === 9) {
    if (control === 0 || control === 8) el.addEventListener("change", () => fire(rec([bool(el.checked)])), capture);
    else if (control === 28) stepper(el, (n) => fire(rec([n])), capture);
    else if (control === 27) el.addEventListener("click", live((e) => {
      if (!crossed(el, e)) fire(rec([bool(el.getAttribute("aria-pressed") !== "true")]));
    }), capture);
    else if (control === 9) el.addEventListener("toggle", () => fire(rec([bool(el.open)])), capture);
    else if (control === 10 || control === 20) {
      // Escape and a press on the scrim, the dialog's own box, and a
      // sheet's drag off ask it to close; the view decides.
      el.addEventListener("cancel", (e) => { e.preventDefault(); fire(rec([bool(false)])); }, capture);
      el.addEventListener("pointerdown", (e) => { if (e.target === el && el.open) fire(rec([bool(false)])); }, capture);
      if (control === 20) el.addEventListener("viso-dismiss", () => fire(rec([bool(false)])), capture);
    }
    else if (control === 1 || control === 30) el.addEventListener("input", () => { knob(el); fire(rec([Number(el.value)])); }, capture);
    else if (control === 31) el.addEventListener("input", (e) => { if (e.target !== el) fire(rec([picked(el)])); }, capture);
    else if (control === 32) {
      // Free text is taken on Enter or as the focus leaves; otherwise only
      // an option is, and any other text goes back to the one held.
      const [input] = listed(el);
      const take = () => { el.$held = input.value; fire(rec([input.value])); };
      input.addEventListener("input", () => {
        if (el.dataset.free !== "true" && offered(el, input.value)) take();
      }, capture);
      input.addEventListener("change", () => {
        if (el.dataset.free === "true") { if (input.value !== (el.$held ?? input.defaultValue)) take(); }
        else if (!offered(el, input.value)) input.value = el.$held ?? input.defaultValue;
      }, capture);
    }
    else if (control === 33) {
      searching(el);
      el.addEventListener("viso-query", (e) => fire(rec([e.detail])), capture);
    }
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
  } else if (route === 10 && control === 33) {
    searching(el);
    el.addEventListener("viso-submit", () => fire(null), capture);
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
  } else if (route === 19) {
    el.addEventListener("click", live((e) => {
      const a = e.target.closest && e.target.closest("a[href]");
      if (!a || !el.contains(a)) return;
      e.preventDefault();
      fire(rec([a.getAttribute("href")]));
    }), capture);
  } else if (route === 22) {
    // A back gesture past the first page asks for the one before: Escape or
    // Backspace on the stack itself, the browser's back key, or the mouse's
    // back button anywhere in it, which the deepest stack takes.
    const back = (e) => {
      const at = el.$page ?? 0;
      if (at <= 0) return;
      e.preventDefault();
      e.stopPropagation();
      fire(rec([BigInt(at - 1)]));
    };
    el.addEventListener("keydown", live((e) => {
      if (e.repeat || e.isComposing) return;
      if (e.key === "BrowserBack" || ((e.key === "Escape" || e.key === "Backspace") && e.target === el)) back(e);
    }), capture);
    el.addEventListener("mouseup", live((e) => { if (e.button === 3) back(e); }), capture);
  } else if (route === 23) {
    el.$double = true;
    el.addEventListener("dblclick", live((e) => fire(rec([point(e, el), kind(e)]))), capture);
  } else if (route === 28) {
    el.addEventListener("viso-fullswipe", (e) => fire(rec([BigInt(e.detail.edge)])), capture);
  } else if (route === 30) {
    el.addEventListener("viso-detent", (e) => fire(rec([BigInt(e.detail.detent)])), capture);
  } else if (route === 29) {
    reorderList(el);
    el.addEventListener("viso-reorder", (e) => fire(rec([BigInt(e.detail.from), BigInt(e.detail.to)])), capture);
  } else if (route >= 31 && route <= 33) {
    dragDrop();
    const phase = [2, 0, 1][route - 31];
    el.addEventListener("viso-drop", (e) => {
      if (e.target === el && e.detail.phase === phase) fire(dropEvent(el, e.detail));
    }, capture);
  } else if (route === 43) {
    shortcut(el, fire);
  } else if (route === 35) {
    // A click on a chip's cross, or Delete or Backspace on it, asks it to go.
    el.addEventListener("click", live((e) => { if (crossed(el, e)) fire(null); }), capture);
    el.addEventListener("keydown", live((e) => {
      if (e.repeat || e.isComposing || el.dataset.removable !== "true") return;
      if (e.key === "Delete" || e.key === "Backspace") { e.preventDefault(); fire(null); }
    }), capture);
  } else if (route === 34) {
    dragDrop();
    el.addEventListener("viso-dragend", (e) => {
      const op = e.detail.operation;
      if (e.target === el) fire(rec([op === null ? null : BigInt(op)]));
    }, capture);
  } else if (route === 36) {
    // An image's or a video's first picture, at its natural size.
    const loaded = () => fire(rec([el.naturalWidth ?? el.videoWidth, el.naturalHeight ?? el.videoHeight]));
    el.addEventListener(el.tagName === "VIDEO" ? "loadeddata" : "load", loaded, capture);
    if (el.tagName === "IMG" && el.complete && el.naturalWidth) queueMicrotask(loaded);
  } else if (route === 37) {
    el.addEventListener("error", () => fire(null), capture);
  } else if (route === 38) {
    // Where a video is, how long it plays, whether it plays and whether it
    // waits for data, each time one changes; its time a few times a second.
    let waiting = false;
    const report = () => fire(rec([
      el.currentTime,
      Number.isFinite(el.duration) ? el.duration : null,
      bool(!el.paused && !el.ended),
      bool(waiting),
    ]));
    for (const name of ["timeupdate", "durationchange", "play", "pause", "seeked", "ended"]) {
      el.addEventListener(name, report, capture);
    }
    el.addEventListener("waiting", () => { waiting = true; report(); }, capture);
    el.addEventListener("playing", () => { waiting = false; report(); }, capture);
  } else if (route === 39) {
    el.addEventListener("ended", () => fire(null), capture);
  } else if (route === 40) {
    el.addEventListener("play", () => fire(bool(true)), capture);
    el.addEventListener("pause", () => fire(bool(false)), capture);
  } else if (route === 41) {
    el.addEventListener("fullscreenchange", () => fire(bool(document.fullscreenElement === el)));
  } else if (route >= 25 && route <= 27) {
    pinchable(el);
    el.addEventListener("viso-pinch", live((e) => {
      const d = e.detail;
      if (d.phase !== route - 25) return;
      const r = el.getBoundingClientRect();
      fire(rec([rec([d.x - r.left, d.y - r.top]), d.scale, d.turn, d.ds, d.dt, rec([mixed(d.px), mixed(d.py)])]));
    }), capture);
  } else if (route === 24) {
    // A drag that lifts moving at 400px/s or more along the axis it moves
    // fastest along, the way it travelled, is a swipe that way.
    draggable(el);
    el.addEventListener("viso-drag", live((e) => {
      const d = e.detail;
      if (d.phase !== 2) return;
      const across = Math.abs(d.vx) >= Math.abs(d.vy);
      const v = across ? d.vx : d.vy;
      if (Math.abs(v) < 400 || v * (across ? d.tx : d.ty) <= 0) return;
      const way = BigInt((across ? 0 : 2) + (v > 0 ? 1 : 0));
      fire(rec([way, point(d.e, el), rec([mixed(d.vx), mixed(d.vy)])]));
    }), capture);
  } else if (route >= 15) {
    draggable(el);
    el.addEventListener("viso-drag", live((e) => {
      const d = e.detail;
      if (d.phase === route - 15) fire(rec([point(d.e, el), rec([mixed(d.dx), mixed(d.dy)]), rec([mixed(d.tx), mixed(d.ty)]), rec([mixed(d.vx), mixed(d.vy)])]));
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
  else if (control === 10 || control === 11 || control === 20) P.layer(el, v);
  else if (control === 1) el.value = v === null ? el.min : String(v);
  else if (control === 13) P.number(el, v);
  else if (control === 2 || control === 29) select(el, v === null ? 0n : v, el.$radio);
  else if (control === 14) page(el, v);
  else if (v !== null) P.input(el, v);
};

