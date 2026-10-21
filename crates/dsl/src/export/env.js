// The adaptive environment as the browser reports it: each `env` field by
// its tag, typed as the prelude's `Environment` record types it, and the
// anchors whose fields depend on where their reader sits.

const SAFE = "position:fixed;visibility:hidden;pointer-events:none;" +
  "padding:env(safe-area-inset-top) env(safe-area-inset-right) env(safe-area-inset-bottom) env(safe-area-inset-left)";
// Where the size classes start, in dp.
const MEDIUM = 600, EXPANDED = 840;

// Lengths and scales are `f32`s the runtime widens.
const f = Math.fround;
const px = (s) => parseFloat(s) || 0;
const probe = (style, read) => {
  const el = document.createElement("span");
  el.setAttribute("style", style);
  document.documentElement.appendChild(el);
  const v = read(getComputedStyle(el));
  el.remove();
  return v;
};
const insets = () => probe(SAFE, (s) => [s.paddingTop, s.paddingRight, s.paddingBottom, s.paddingLeft].map((v) => f(px(v))));
const classify = (width) => (width >= EXPANDED ? 2n : width >= MEDIUM ? 1n : 0n);
const matches = (q) => matchMedia(q).matches;

// The window-wide field `tag` now.
const envField = (tag) => {
  switch (tag) {
    case 0: return rec([rec([f(innerWidth), f(innerHeight)]), f(devicePixelRatio)]);
    case 1: {
      const [t, r, b, l] = insets();
      return rec([0, f(Math.max(innerWidth - l - r, 0)), 0, f(Math.max(innerHeight - t - b, 0))]);
    }
    case 2: return classify(innerWidth);
    case 3: return rec(insets());
    case 4: {
      const v = window.visualViewport;
      const covered = v && Math.abs(v.scale - 1) <= 0.01 ? innerHeight - (v.height + v.offsetTop) : 0;
      return rec([covered >= 1 ? f(covered) : 0]);
    }
    case 5: {
      const segments = (window.viewport && window.viewport.segments) || [];
      const out = [];
      for (let i = 1; i < segments.length; i++) {
        const a = segments[i - 1], b = segments[i];
        const side = b.left >= a.right;
        const x = side ? a.right : a.left, y = side ? a.top : a.bottom;
        const bounds = [x, y, side ? b.left - a.right : a.width, side ? a.height : b.top - a.bottom];
        out.push({ t: 0, f: [rec(bounds.map(f))] });
      }
      return out;
    }
    case 6: {
      const pads = navigator.getGamepads ? Array.from(navigator.getGamepads()).some(Boolean) : false;
      return rec([
        matches("(pointer: fine)") ? 0n : matches("(pointer: coarse)") ? 1n : 2n,
        bool(matches("(hover: hover)")),
        bool(!matches("(pointer: coarse)") || matches("(any-pointer: fine)")),
        bool(navigator.maxTouchPoints > 0),
        0n,
        bool(pads),
      ]);
    }
    case 7: return f(probe("position:absolute;visibility:hidden;font-size:medium", (s) => px(s.fontSize) / 16 || 1));
    case 8: return bool(matches("(prefers-reduced-motion: reduce)"));
    case 9: return innerWidth > innerHeight ? 1n : 0n;
    case 10: return getComputedStyle(document.documentElement).direction === "rtl" ? 1n : 0n;
    default: return null;
  }
};

// The constraints `el`'s parent gives it along an axis (0 the width, 1 the
// height): the parent's content box, found through every parent whose
// extent its content decides, less their padding; null under a viewport
// scrolling along the axis. The view's root gets its own box.
const incoming = (el, axis) => {
  const fit = axis === 0 ? "w" : "h";
  let inset = 0;
  for (let at = el; ;) {
    const p = at.parentElement;
    if (at.hasAttribute("data-root") || !p) {
      const r = at.getBoundingClientRect();
      return f(Math.max((axis === 0 ? r.width : r.height) - inset, 0));
    }
    const s = getComputedStyle(p);
    const overflow = axis === 0 ? s.overflowX : s.overflowY;
    if (overflow === "auto" || overflow === "scroll") return null;
    inset += axis === 0 ? px(s.paddingLeft) + px(s.paddingRight) : px(s.paddingTop) + px(s.paddingBottom);
    if (!(p.getAttribute("data-fit") || "").includes(fit)) {
      const r = p.getBoundingClientRect();
      return f(Math.max((axis === 0 ? r.width : r.height) - inset, 0));
    }
    at = p;
  }
};
// The class of the nearest adaptive scope at or above `el`: a scope with a
// basis classifies it, one without the width its parent gives it, and one
// its parent does not bound defers to the scope above; under none, the
// window's.
const sizeClass = (el) => {
  for (let at = el; at; at = at.hasAttribute("data-root") ? null : at.parentElement) {
    const basis = at.getAttribute("data-scope");
    if (basis !== null) {
      const width = basis === "" ? incoming(at, 0) : Number(basis);
      if (width !== null) return classify(width);
    }
  }
  return classify(innerWidth);
};
// Field `tag` for the reader anchored at `el`.
const anchored = (tag, el) => {
  if (!el || !el.isConnected) return envField(tag);
  if (tag === 2) return sizeClass(el);
  if (tag === 10) return getComputedStyle(el).direction === "rtl" ? 1n : 0n;
  const w = incoming(el, 0), h = incoming(el, 1);
  return rec([0, w, 0, h]);
};

// Keeps the `env` slots current: `reads` the window-wide ones, `[slot, tag]`
// each; `write(changes, m)` commits `[slot, value]` pairs in the scope of
// mount `m` (the view's when null), and `get(slot, m)` reads a slot there.
class Env {
  constructor(reads, write, get) {
    this.reads = reads;
    this.write = write;
    this.get = get;
    if (!reads.length) return;
    const update = () => this.update();
    addEventListener("resize", update);
    addEventListener("orientationchange", update);
    addEventListener("gamepadconnected", update);
    addEventListener("gamepaddisconnected", update);
    if (window.visualViewport) visualViewport.addEventListener("resize", update);
    for (const q of ["(pointer: fine)", "(pointer: coarse)", "(hover: hover)", "(prefers-reduced-motion: reduce)"]) {
      matchMedia(q).addEventListener("change", update);
    }
  }

  // The window-wide values, as the view starts.
  initial() {
    return this.reads.map(([slot, tag]) => [slot, envField(tag)]);
  }

  update() {
    const changes = [];
    for (const [slot, tag] of this.reads) {
      const v = envField(tag);
      if (!$.eq(v, this.get(slot, null))) changes.push([slot, v]);
    }
    if (changes.length) this.write(changes, null);
  }

  // Follows the anchored `reads` of the reader whose root is `el`, in mount
  // `m`, as the boxes around it change, until `m` ends.
  anchor(el, reads, m) {
    const a = { seen: new Set(), ro: null };
    const settle = () => {
      for (let at = el; at && !a.seen.has(at); at = at.hasAttribute("data-root") ? null : at.parentElement) {
        a.seen.add(at);
        a.ro.observe(at);
      }
      const changes = [];
      for (const [slot, tag] of reads) {
        const v = anchored(tag, el);
        if (!$.eq(v, this.get(slot, m))) changes.push([slot, v]);
      }
      if (changes.length) this.write(changes, m);
    };
    a.ro = new ResizeObserver(settle);
    a.ro.observe(el);
    if (m) (m.env || (m.env = [])).push(a.ro);
  }
}

// Stops following the anchors of mount `m`.
const unanchor = (m) => {
  if (m.env) for (const ro of m.env) ro.disconnect();
};
