// The page's states `S`, its inputs `I` and the functions its script
// carries, which a handler's transaction runs against a working copy `W`.
let pending = [];
let starts = [];
let nodes = [];
$.emit = (event, args) => pending.push([event, args]);
$.node = (action, key, args) => request(nodes, action, key, args);
$.start = (task, args, done, cancelled, instance, slot, policy) =>
  starts.push({ task, args, done, cancelled, instance, slot, policy });
$.tr = (m, a) => translate(T, m, a);
const put = (slot, x) => { S[slot] = x; };
const held = (slot) => S[slot];

// A handler writes a working copy `W`, which only success commits.
const host = {
  transact(f, m) {
    if (m.f) focus(m);
    const before = S;
    W = S.slice();
    pending = [];
    starts = [];
    nodes = [];
    let value;
    try {
      value = f();
    } catch (e) {
      W = S;
      pending = [];
      starts = [];
      nodes = [];
      console.error(e);
      return null;
    }
    S = W;
    if (m.f) keep(m);
    const changed = new Set();
    for (let i = 0; i < S.length; i++) if (!Object.is(S[i], before[i])) changed.add(i);
    const t = { value, changed, events: pending, starts, nodes, from: origin };
    pending = [];
    starts = [];
    nodes = [];
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
  drop(f, m) {
    if (m.f) focus(m);
    W = S.slice();
    pending = [];
    try { f(); } catch (e) { console.error(e); }
    W = S;
    const events = pending;
    pending = [];
    starts = [];
    nodes = [];
    deliver(events);
  },
};
