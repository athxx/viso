let root = null;
let top = null;
let pending = [];
let starts = [];
$.emit = (event, args) => pending.push([event, args]);
$.start = (task, args, done, cancelled, instance, slot, policy) =>
  starts.push({ task, args, done, cancelled, instance, slot, policy });
$.tr = (m, a) => translate(T, m, a);

// A mount of content: its wired values `u` and regions `r`, the region
// bindings `sc` its functions take, the mount `p` around it, the instances
// `o` it mounts, the values `l` it keeps of their states, and its effects.
// `f` says whether it or a mount around it keeps states.
const mount = (sc, p, o = null, l = null) => {
  const m = { u: [], r: [], sc, p, o, l: null, f: !!(p && p.f), fx: [], live: true };
  if (l) {
    if (m.f) focus(p);
    const s = [], v = [];
    for (const [slot, init] of l) {
      let x = null;
      if (init) {
        try { x = init(); } catch (e) { console.error(e); }
      }
      S[slot] = x;
      s.push(slot);
      v.push(x);
    }
    m.l = { s, v };
    m.f = true;
  }
  return m;
};
// Puts the states mount `m` and the mounts around it keep where its
// functions read them.
const focus = (m) => {
  if (m.p) focus(m.p);
  const l = m.l;
  if (l) for (let i = 0; i < l.s.length; i++) S[l.s[i]] = l.v[i];
};
// Keeps what a transaction left in the states of `m` and the mounts around it.
const keep = (m) => {
  for (let x = m; x; x = x.p) {
    const l = x.l;
    if (l) for (let i = 0; i < l.s.length; i++) l.v[i] = S[l.s[i]];
  }
};

const refresh = (m, changed) => {
  if (m.f) focus(m);
  for (const [deps, f] of m.u) {
    if (hit(deps, changed)) {
      try { f(); } catch (e) { console.error(e); }
    }
  }
  for (const r of m.r) {
    try { r.update(changed); } catch (e) { console.error(e); }
  }
};

const deliver = (events) => {
  for (const [event, args] of events) {
    root.dispatchEvent(new CustomEvent(E[event], { detail: args, bubbles: true }));
  }
};
// The page's transactions, over the states `S`: a handler writes a working
// copy `W`, which only success commits.
const work = new Work({
  transact(f, m) {
    if (m.f) focus(m);
    const before = S;
    W = S.slice();
    pending = [];
    starts = [];
    let value;
    try {
      value = f();
    } catch (e) {
      W = S;
      pending = [];
      starts = [];
      console.error(e);
      return null;
    }
    S = W;
    if (m.f) keep(m);
    const changed = new Set();
    for (let i = 0; i < S.length; i++) if (!Object.is(S[i], before[i])) changed.add(i);
    const t = { value, changed, events: pending, starts };
    pending = [];
    starts = [];
    return t;
  },
  land(t) {
    if (t.changed.size) refresh(top, t.changed);
    deliver(t.events);
  },
  read(f, m) {
    if (m.f) focus(m);
    return f();
  },
  drop(f, m) {
    if (m.f) focus(m);
    W = S.slice();
    pending = [];
    try { f(); } catch (e) { console.error(e); }
    W = S;
    const events = pending;
    pending = [];
    starts = [];
    deliver(events);
  },
}, null);
const fx = (m, list) => work.effects(m, list);

// Ends mount `m` and the content it mounts.
const dispose = (m) => {
  for (const r of m.r) {
    if (r.c) dispose(r.c.m);
    for (const c of r.kept.values()) dispose(c.m);
    if (r.items) for (const it of r.items) dispose(it.m);
  }
  unanchor(m);
  work.end(m);
};

const on = (el, route, capture, control, handler, m) =>
  listen(el, route, capture, control, (payload) => {
    work.run(() => handler(payload), m);
    if (el.$sync) {
      if (m.f) focus(m);
      el.$sync();
    }
  });

const clone = (r, arm, scope) => {
  const frag = r.anchor.content.children[arm].content.cloneNode(true);
  const nodes = Array.from(frag.childNodes);
  const m = r.g.arms[arm](Array.from(frag.querySelectorAll("[data-v]")), scope, r.m);
  refresh(m, null);
  return { m, nodes, scope, key: null };
};
// The nodes content `c` shows, in order: its own, and before each region
// anchor among them what that region mounts.
const nodesOf = (c, out = []) => {
  for (const n of c.nodes) {
    const r = c.m.r.find((r) => r.anchor === n);
    if (r) {
      if (r.items) for (const it of r.items) nodesOf(it, out);
      else if (r.c) nodesOf(r.c, out);
    }
    out.push(n);
  }
  return out;
};
const place = (r, c) => {
  for (const n of nodesOf(c)) r.anchor.parentNode.insertBefore(n, r.anchor);
};
const drop = (c) => {
  for (const n of nodesOf(c)) n.remove();
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
        else dispose(r.c.m);
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
  for (const it of old.values()) {
    drop(it);
    dispose(it.m);
  }
  for (const it of next) place(r, it);
  r.items = next;
};
const region = (anchor, g, sc, m) => {
  const r = { anchor, g, sc, m, arm: -1, c: null, kept: new Map(), items: null };
  r.update = (changed) => {
    if (m.f) focus(m);
    if (g.k === 2) repeat(r, changed);
    else choice(r, changed);
  };
  return r;
};

const start = (view) => {
  root = document.getElementById("viso-root");
  top = view(Array.from(document.querySelectorAll("[data-v]")), [], null);
  work.top = top;
  refresh(top, null);
  work.flush(new Set());
};
