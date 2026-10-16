
// A component's store: its states as signals, its inputs read from its
// props, its events called back through them. A handler runs as one
// transaction over a working copy of the states; only on success does it
// set the signals whose state it changed, in one batch, and call back what
// it emitted.
class Store {
  constructor(o) {
    this.states = o.states;
    this.inputs = o.inputs;
    this.events = o.events;
    this.messages = o.messages;
    this.work = null;
    this.pending = [];
    for (const [id, href] of o.assets) A.set(id, href);
  }
  get(i) {
    return this.work ? this.work[i] : this.states[i][0]();
  }
  set(i, v) {
    if (!this.work) throw fault("a state is written outside a handler");
    this.work[i] = v;
  }
  input(i) {
    const [read, kind, fallback] = this.inputs[i];
    const v = read();
    if (v === undefined) return fallback;
    switch (kind) {
      case "bool": return v ? 1n : 0n;
      case "int": return typeof v === "bigint" ? v : BigInt(v);
      case "float": return Number(v);
      default: return v;
    }
  }
  emit(event, args) {
    this.pending.push([event, args]);
  }
  tr(m, a) {
    return translate(this.messages, m, a);
  }
  memo(f) {
    return createMemo(f, undefined, { equals: $.eq });
  }
  effect(f) {
    createRenderEffect(f);
  }
  run(handler) {
    if (this.work) {
      handler();
      return;
    }
    const before = this.states.map(([read]) => untrack(read));
    this.work = before.slice();
    this.pending = [];
    try {
      untrack(handler);
    } catch (e) {
      this.work = null;
      this.pending = [];
      console.error(e);
      return;
    }
    const after = this.work;
    const events = this.pending;
    this.work = null;
    this.pending = [];
    batch(() => {
      for (let i = 0; i < after.length; i++) {
        if (!Object.is(after[i], before[i])) this.states[i][1](() => after[i]);
      }
    });
    for (const [event, args] of events) {
      const call = this.events[event]();
      if (call) call(...args);
    }
  }
  on(el, route, capture, control, handler) {
    listen(el, route, capture, control, (payload) => {
      this.run(() => handler(payload));
      if (el.$sync) el.$sync();
    });
  }
  click(e) {
    return pointer(0, e, e.currentTarget);
  }
  sync(el, control, value) {
    if (control === 2) el.$radio = el.getAttribute("role") === "radiogroup";
    el.$sync = () => sync(el, control, value);
  }
}

export const store = (o) => new Store(o);

// The items of a `for` region, each mounted once per key: an item whose key
// stays keeps its elements and sees its new value.
export function Each(props) {
  const rows = createMemo(() => {
    let list = props.each();
    if (!Array.isArray(list)) {
      const lo = list.f[0];
      const hi = list.t === 1 ? list.f[1] + 1n : list.f[1];
      if (hi - lo > MAX_ITEMS) throw fault("a `for` region mounts at most " + MAX_ITEMS + " items");
      list = [];
      for (let i = lo; i < hi; i++) list.push(i);
    }
    const by = props.by;
    const keys = list.map((v, i) => (by ? $.key(by(v)) : "#" + i));
    if (new Set(keys).size !== keys.length) throw fault("two items of a `for` region share a key");
    return { list, keys };
  });
  const keys = createMemo(() => rows().keys);
  const render = props.children;
  return createMemo(
    mapArray(keys, (_, i) => render(createMemo(() => rows().list[i()], undefined, { equals: $.eq }))),
  );
}

// A `match` region's value, read once for its arms.
export function With(props) {
  const value = createMemo(() => props.value(), undefined, { equals: $.eq });
  const render = props.children;
  return untrack(() => render(value));
}

// The arm `arm()` of an `if` or `match` region whose arms `keep` marks are
// preserved: hidden, not dropped, while another shows.
export function Choose(props) {
  const owner = getOwner();
  const kept = new Map();
  let shown = null;
  onCleanup(() => {
    for (const c of kept.values()) c.dispose();
    if (shown && !kept.has(shown.arm)) shown.dispose();
  });
  const arm = createMemo(() => Number(props.arm()));
  return createMemo(() => {
    const at = arm();
    return untrack(() => {
      if (shown && !props.keep[shown.arm]) shown.dispose();
      shown = null;
      if (at < 0) return null;
      let c = kept.get(at);
      if (!c) {
        c = createRoot((dispose) => ({ arm: at, dispose, nodes: props.arms[at]() }), owner);
        if (props.keep[at]) kept.set(at, c);
      }
      shown = c;
      return c.nodes;
    });
  });
}

// What an element shows of a value.
export const show = {
  text,
  truth,
  color,
  opt: (v) => (v === null ? undefined : text(v)),
  num: (v) => (v === null ? undefined : String(v)),
  url,
  fit: (v) => fits[Number(v ?? 0n)] || "contain",
  maskSize: (v) => {
    const fit = fits[Number(v ?? 0n)] || "contain";
    return fit === "fill" ? "100% 100%" : fit;
  },
  mask: (href) => (href ? 'url("' + href + '")' : "none"),
  opacity: (v) => String(Math.min(Math.max(v ?? 1, 0), 1)),
  move(v, property) {
    const f = v === null ? [] : v.f;
    const instant = reduced.matches && f[3] === 0n;
    const s = (x) => Math.max(Number(x ?? 0), 0) + "s";
    return property + " " + (instant ? "0s" : s(f[0])) + " " +
      (easings[Number(f[2] ?? 2n)] || "ease-out") + " " + (instant ? "0s" : s(f[1]));
  },
  select,
};

export { $, fault };
