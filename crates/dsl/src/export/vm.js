// The page's behavior on the Viso VM `V`, the app's own module running in
// the page: each call commits as one transaction or rolls back, and
// `V.take()` hands over what the calls since the last take changed,
// emitted, started and asked of named nodes. A closure the VM hands back is
// a function that calls it as a transaction.
const put = (slot, x) => V.put(slot, x);
const held = (slot) => V.get(slot);
const fn = (o) => (o === null || o === undefined ? null : (...a) => V.c(o, a));
const taken = () => {
  const o = V.take();
  return {
    changed: new Set(o.c),
    events: o.e,
    starts: o.s.map((s) => ({
      task: (tk, ...a) => V.run(s.k, a, tk),
      args: s.a,
      done: fn(s.d),
      cancelled: fn(s.x),
      instance: s.i,
      slot: s.n,
      policy: s.p,
    })),
    nodes: o.n,
  };
};
$.ended = (id) => V.ended(id);
// An event an island's component emits runs the handler of entry `n` with
// its record `p`, as a transaction of the page.
V.listen((n, p) => work.run(() => V.t(n, [p])));

const host = {
  transact(f, m) {
    if (m.f) focus(m);
    let value;
    try {
      value = f();
    } catch (e) {
      V.take();
      console.error(e);
      return null;
    }
    if (m.f) keep(m);
    const t = taken();
    t.value = value;
    t.from = origin;
    return t;
  },
  land(t) {
    if (t.changed.size) refresh(top, t.changed);
    deliver(t.events);
    reach(t.nodes, t.from);
  },
  read(f, m) {
    if (m.f) focus(m);
    return f();
  },
  // A cleanup's writes stay unseen; its events are delivered.
  drop(f, m) {
    if (m.f) focus(m);
    try { f(); } catch (e) { console.error(e); }
    deliver(taken().events);
  },
};
