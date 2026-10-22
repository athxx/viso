// The easing curves a view's `Easing` names, in the order it declares them:
// `linear`, the cubic `ease_*`, then each family in, out and in-out; and an
// `Easing` as a CSS timing function, a named curve or a spring sampled once
// into a `linear()` that stays within 0.2% of it.
const EASE_C1 = 1.70158, EASE_C2 = EASE_C1 * 1.525, EASE_C3 = EASE_C1 + 1;
const EASE_C4 = (2 * Math.PI) / 3, EASE_C5 = (2 * Math.PI) / 4.5;
const bounceOut = (t) => {
  const n = 7.5625, d = 2.75;
  if (t < 1 / d) return n * t * t;
  if (t < 2 / d) return n * (t -= 1.5 / d) * t + 0.75;
  if (t < 2.5 / d) return n * (t -= 2.25 / d) * t + 0.9375;
  return n * (t -= 2.625 / d) * t + 0.984375;
};
const easeIns = [
  (t) => t * t,
  (t) => t * t * t,
  (t) => t * t * t * t,
  (t) => t * t * t * t * t,
  (t) => 1 - Math.cos((t * Math.PI) / 2),
  (t) => (t <= 0 ? 0 : 2 ** (10 * t - 10)),
  (t) => 1 - Math.sqrt(Math.max(1 - t * t, 0)),
  (t) => EASE_C3 * t * t * t - EASE_C1 * t * t,
  (t) => (t <= 0 || t >= 1 ? Math.min(Math.max(t, 0), 1)
    : -(2 ** (10 * t - 10)) * Math.sin((t * 10 - 10.75) * EASE_C4)),
  (t) => 1 - bounceOut(1 - t),
];
const easeInOut = (family, t) => {
  if (family === 7) {
    const u = 2 * t;
    return t < 0.5 ? (u * u * ((EASE_C2 + 1) * u - EASE_C2)) / 2
      : ((u - 2) * (u - 2) * ((EASE_C2 + 1) * (u - 2) + EASE_C2) + 2) / 2;
  }
  if (family === 8) {
    if (t <= 0 || t >= 1) return Math.min(Math.max(t, 0), 1);
    const ring = Math.sin((20 * t - 11.125) * EASE_C5);
    return t < 0.5 ? -(2 ** (20 * t - 10) * ring) / 2 : (2 ** (-20 * t + 10) * ring) / 2 + 1;
  }
  const f = easeIns[family];
  return t < 0.5 ? f(2 * t) / 2 : 1 - f(2 - 2 * t) / 2;
};
// The progress of named curve `tag` at time `t`.
const curveAt = (tag, t) => {
  t = Math.min(Math.max(t, 0), 1);
  if (tag === 0) return t;
  const [family, shape] = tag < 4 ? [1, tag - 1] : [Math.floor((tag - 4) / 3), (tag - 4) % 3];
  const f = easeIns[family];
  return shape === 0 ? f(t) : shape === 1 ? 1 - f(1 - t) : easeInOut(family, t);
};
const easings = [];
// The stops past `a` up to `b` along which a line keeps to `f`.
const easeStops = (f, a, b, depth, out) => {
  const fa = f(a), fb = f(b);
  const off = (t) => Math.abs(f(t) - (fa + ((fb - fa) * (t - a)) / (b - a)));
  const straight = [0.25, 0.5, 0.75].every((k) => off(a + (b - a) * k) <= 0.002);
  if (depth >= 8 || (depth >= 2 && straight)) return void out.push(b);
  const mid = (a + b) / 2;
  easeStops(f, a, mid, depth + 1, out);
  easeStops(f, mid, b, depth + 1, out);
};
const easeNumber = (v) => String(Math.round(v * 10000) / 10000);
const easeNamed = (tag) => {
  if (tag === 0) return "linear";
  if (easings[tag]) return easings[tag];
  const f = (t) => curveAt(tag, t);
  const stops = [0];
  easeStops(f, 0, 1, 0, stops);
  const parts = stops.map((t, i) =>
    easeNumber(f(t)) + (i > 0 && i < stops.length - 1 ? " " + easeNumber(t * 100) + "%" : ""));
  return (easings[tag] = "linear(" + parts.join(", ") + ")");
};
// A `spring(stiffness, damping, mass)` as the runtime holds it: its undamped
// frequency and damping ratio, a hundredth of critical or more.
const springOf = (e) => {
  const [k0, c0, m0] = e.f.map((v) => Number(v));
  const pos = (v, or) => (Number.isFinite(v) && v > 0 ? v : or);
  const k = pos(k0, (4 * Math.PI) ** 2), m = pos(m0, 1);
  const c = Number.isFinite(c0) ? c0 : 0;
  return [Math.sqrt(k / m), Math.max(c / (2 * Math.sqrt(k * m)), 0.01)];
};
// Where a mass released one from rest stands `t` seconds on, and how fast.
const springAt = (w, z, t) => {
  if (Math.abs(z - 1) < 1e-3) {
    const d = Math.exp(-w * t);
    return [(1 + w * t) * d, -w * w * t * d];
  }
  if (z < 1) {
    const a = z * w, wd = w * Math.sqrt(1 - z * z), b = a / wd, d = Math.exp(-a * t);
    const s = Math.sin(wd * t), co = Math.cos(wd * t);
    return [(co + b * s) * d, -(wd + a * b) * s * d];
  }
  const root = w * Math.sqrt(z * z - 1), r1 = -z * w + root, r2 = -z * w - root;
  const c2 = -r1 / (r2 - r1), c1 = 1 - c2, e1 = Math.exp(r1 * t), e2 = Math.exp(r2 * t);
  return [c1 * e1 + c2 * e2, r1 * c1 * e1 + r2 * c2 * e2];
};
// A spring's move from rest as a `linear()` over the seconds it takes to
// settle within a thousandth of its way, and those seconds.
const springs = new Map();
const spring = (e) => {
  const [w, z] = springOf(e);
  const key = w + " " + z;
  if (springs.has(key)) return springs.get(key);
  const settled = (t) => {
    const [x, v] = springAt(w, z, t);
    return x * x + (v / w) ** 2 <= 1e-6;
  };
  let lo = 0, hi = 0.05;
  while (!settled(hi) && hi < 600) hi *= 2;
  for (let i = 0; i < 32; i++) {
    const mid = (lo + hi) / 2;
    if (settled(mid)) hi = mid; else lo = mid;
  }
  const f = (u) => (u >= 1 ? 1 : 1 - springAt(w, z, u * hi)[0]);
  const stops = [0];
  easeStops(f, 0, 1, 0, stops);
  const parts = stops.map((t, i) =>
    easeNumber(f(t)) + (i > 0 && i < stops.length - 1 ? " " + easeNumber(t * 100) + "%" : ""));
  const out = ["linear(" + parts.join(", ") + ")", hi];
  springs.set(key, out);
  return out;
};
const easing = (e) => {
  if (typeof e === "bigint") return Number(e) < 34 ? easeNamed(Number(e)) : easeNamed(2);
  if (e === null || e === undefined || !e.f) return easeNamed(2);
  if (e.t === 35) return spring(e)[0];
  const [x1, y1, x2, y2] = e.f.map((v) => Number(v) || 0);
  const x = (v) => Math.min(Math.max(v, 0), 1);
  return `cubic-bezier(${x(x1)}, ${y1}, ${x(x2)}, ${y2})`;
};
// The seconds a `Transition`'s fields `f` move for: a spring's settle time
// in place of its duration.
const easeSeconds = (f) => (f[2] && f[2].t === 35 ? spring(f[2])[1] : f[0]);
