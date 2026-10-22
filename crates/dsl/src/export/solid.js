
// A component's store: its states as signals, its inputs read from its
// props, its events called back through them, and the work it runs beside
// its transactions. A handler runs as one transaction over a working copy of
// the states; only on success does it set the signals whose state it
// changed, in one batch, call back what it emitted, and start the tasks it
// started. Region content mounting a component keeps that component's states
// as signals of its own mount.
class Store {
  constructor(o) {
    this.states = o.states;
    this.inputs = o.inputs;
    this.events = o.events;
    this.messages = o.messages;
    this.work = null;
    this.pending = [];
    this.starts = [];
    this.nodes = [];
    // The mount whose states a read sees, beside the component's.
    this.cur = null;
    this.top = { p: null, o: null, l: null, fx: [], live: true };
    this.w = new Work(this, this.top);
    this.env = new Env(
      o.env || [],
      (changes, m) => this.run(() => { for (const [s, v] of changes) this.set(s, v); }, m),
      (s, m) => this.read(() => this.get(s), m || this.top),
    );
    onCleanup(() => {
      unanchor(this.top);
      this.w.end(this.top);
    });
    for (const [id, href] of o.assets) A.set(id, href);
  }
  // The signal of slot `i` as mount `m` sees it: the one the nearest mount
  // keeping it holds, else the component's.
  cell(i, m) {
    for (let x = m; x; x = x.p) {
      const l = x.l;
      if (l) {
        const at = l.s.indexOf(i);
        if (at >= 0) return l.v[at];
      }
    }
    return this.states[i];
  }
  get(i) {
    return this.work ? this.work[i] : this.cell(i, this.cur)[0]();
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
  node(action, key, args) {
    return request(this.nodes, action, key, args);
  }
  start(task, args, done, cancelled, instance, slot, policy) {
    this.starts.push({ task, args, done, cancelled, instance, slot, policy });
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
  // Evaluates `f` with the states mount `m` keeps.
  at(m, f) {
    const prev = this.cur;
    this.cur = m;
    try {
      return f();
    } finally {
      this.cur = prev;
    }
  }
  // A transaction over a working copy of the states mount `m` sees; only
  // success sets the signals whose state it changed.
  transact(f, m) {
    if (this.work) {
      f();
      return { value: undefined, changed: new Set(), events: [], starts: [], cells: [], after: [] };
    }
    const cells = this.states.map((_, i) => this.cell(i, m));
    const before = cells.map(([read]) => untrack(read));
    this.work = before.slice();
    this.pending = [];
    this.starts = [];
    this.nodes = [];
    const prev = this.cur;
    this.cur = m;
    let value;
    try {
      value = untrack(f);
    } catch (e) {
      this.work = null;
      this.pending = [];
      this.starts = [];
      this.nodes = [];
      console.error(e);
      return null;
    } finally {
      this.cur = prev;
    }
    const after = this.work;
    this.work = null;
    const changed = new Set();
    for (let i = 0; i < after.length; i++) if (!Object.is(after[i], before[i])) changed.add(i);
    const t = {
      value,
      changed,
      events: this.pending,
      starts: this.starts,
      nodes: this.nodes,
      from: this.origin,
      cells,
      after,
    };
    this.pending = [];
    this.starts = [];
    this.nodes = [];
    return t;
  }
  land(t) {
    if (t.changed.size) {
      batch(() => {
        for (const i of t.changed) t.cells[i][1](() => t.after[i]);
      });
    }
    this.deliver(t.events);
    reach(t.nodes, t.from);
  }
  deliver(events) {
    for (const [event, args] of events) {
      const call = this.events[event]();
      if (call) call(...args);
    }
  }
  read(f, m) {
    return untrack(() => this.at(m, f));
  }
  drop(f, m) {
    const t = this.transact(f, m);
    if (t) this.deliver(t.events);
  }
  run(handler, m) {
    if (this.work) {
      handler();
      return;
    }
    this.w.run(handler, m || this.top);
  }
  // The effects of the component and of the instances no region mounts.
  fx(list) {
    this.w.effects(this.top, list);
  }
  // A mount of region content, inside mount `p`, mounting the instances
  // `own`: it keeps their states `locals` (`[slot, init]` each), runs their
  // effects `fx(m)` and owns the tasks they start, until the content goes.
  mount(p, own, locals, fx, render) {
    const m = { p: p || this.top, o: own, l: null, fx: [], live: true };
    if (locals.length) {
      const s = [], v = [];
      m.l = { s, v };
      for (const [slot, init] of locals) {
        let x = null;
        if (init) {
          try { x = this.read(init, m); } catch (e) { console.error(e); }
        }
        s.push(slot);
        v.push(createSignal(x, { equals: false }));
      }
    }
    onCleanup(() => {
      unanchor(m);
      this.w.end(m);
    });
    if (fx) this.w.effects(m, fx(m));
    return untrack(() => render(m));
  }
  // Follows the `env` fields the reader whose root is `el` reads that depend
  // on where it sits.
  anchor(el, reads, m) {
    this.env.anchor(el, reads, m || this.top);
  }
  on(el, route, capture, control, handler, m) {
    listen(el, route, capture, control, (payload) => {
      this.origin = el;
      this.run(() => handler(payload), m);
      this.origin = null;
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
    mapArray(keys, (k, i) => mark(render(createMemo(() => rows().list[i()], undefined, { equals: $.eq })), k)),
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
  mode: (v) => holding(v)[0] ?? undefined,
  pattern: (v) => holding(v)[1] ?? undefined,
  number: P.number,
  maskSize: (v) => {
    const fit = fits[Number(v ?? 0n)] || "contain";
    return fit === "fill" ? "100% 100%" : fit;
  },
  mask: (href) => (href ? 'url("' + href + '")' : "none"),
  opacity: (v) => String(Math.min(Math.max(v ?? 1, 0), 1)),
  radius,
  stroke,
  offset,
  scale: scaling,
  rotate: turning,
  origin: pivot,
  focusable: P.focusable,
  layer: P.layer,
  enabled: P.enabled,
  autofocus: P.autofocus,
  reaches: (e) => !off(e.target),
  move(v, property) {
    const f = v === null ? [] : v.f;
    const instant = reduced.matches && f[3] === 0n;
    const s = (x) => Math.max(Number(x ?? 0), 0) + "s";
    return property + " " + (instant ? "0s" : s(f[0])) + " " +
      (easings[Number(f[2] ?? 2n)] || "ease-out") + " " + (instant ? "0s" : s(f[1]));
  },
  select,
};

export { $, envField, fault, settle };
