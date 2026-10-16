let root = null;
let top = null;
let pending = [];
$.emit = (event, args) => pending.push([event, args]);
$.tr = (m, a) => translate(T, m, a);

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

const on = (el, route, capture, control, handler) =>
  listen(el, route, capture, control, (payload) => {
    run(() => handler(payload));
    if (el.$sync) el.$sync();
  });

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
