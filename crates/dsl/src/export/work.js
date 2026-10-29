// The work a view runs beside its transactions: tasks, effects and
// resources, each owned by a mount of content — the view's, or that of
// region content mounting a component. A mount carries the mount around it
// `p`, the instances it mounts `o` and its effects `fx`, and is `live` until
// its content goes. The host `h` runs the transactions:
// `transact(f, m)` runs `f` as one in the scope of mount `m`, returning what
// it returned, the states it changed, the events it emitted and the tasks it
// started, or null when it faulted; `land(t, m)` shows a committed one and
// delivers its events; `read(f, m)` evaluates `f` in the scope of `m`; and
// `drop(f, m)` runs a cleanup, whose writes are dropped.

const CANCEL = { cancelled: true };
const ROUNDS = 16;
const LOADING = 1n, READY = 2, ERROR = 3, RELOADING = 4;

// A task native's work, a promise its task awaits; stopping the task
// rejects it.
$.sleep = (tk, d) => new Promise((ok, no) => {
  const stop = () => { clearTimeout(id); no(CANCEL); };
  const id = setTimeout(() => { tk.w.delete(stop); ok(0n); }, Math.max(d, 0) * 1000);
  tk.w.add(stop);
});
// A closure called in a task, which passes its token to one that suspends.
$.callt = (tk, f, a) => (f.$tk ? f(tk, ...a) : f(...a));
$.bindt = (f, c) => {
  const g = (tk, ...a) => f(tk, ...a, ...c);
  g.$tk = true;
  return g;
};

class Work {
  constructor(h, top) {
    this.h = h;
    this.top = top;
    this.jobs = [];
    this.held = [];
    this.fx = [];
    // What the commits of the effects' current round changed, which wakes
    // the next; null outside one.
    this.woke = null;
    this.queued = false;
    // The loads and cache of each shared resource, by pool.
    this.pools = [];
  }

  // Runs `f` as one transaction in mount `m`, then the tasks it started and
  // the effects it woke.
  run(f, m = this.top) {
    const t = this.h.transact(f, m);
    if (!t) return;
    this.land(t, m);
    if (this.woke) for (const i of t.changed) this.woke.add(i);
    else this.flush(t.changed);
  }

  land(t, m) {
    this.h.land(t, m);
    this.launch(t.starts, m);
  }

  // The effects of mount `m`: each `[dep, deps, body, run, res]`, its
  // dependency list and the slots it reads, its body, its run policy (0
  // mount, 1 change, 2 both) and, for a resource, how it loads.
  effects(m, list) {
    for (const [dep, d, body, run, res] of list) {
      const x = { m, dep, d, body, run, res, on: false, last: undefined, clean: null, live: true };
      if (res) {
        x.q = { key: undefined, gen: 0, wait: null };
        x.s = res.p === null ? { loads: [], cache: [] } : (this.pools[res.p] ??= { loads: [], cache: [] });
      }
      m.fx.push(x);
      this.fx.push(x);
    }
    if (list.length && !this.woke && !this.queued) {
      this.queued = true;
      queueMicrotask(() => this.flush(new Set()));
    }
  }

  // Runs the effects mounted since the last round first, then those whose
  // dependencies the round's commits changed, until a round changes nothing.
  flush(changed) {
    this.queued = false;
    let ch = changed;
    this.woke = new Set();
    for (let round = 0; ; round++) {
      this.fx = this.fx.filter((x) => x.live);
      const due = this.fx
        .filter((x) => !x.on)
        .concat(this.fx.filter((x) => x.on && x.dep && hit(x.d, ch)));
      if (!due.length) break;
      if (round === ROUNDS) {
        console.error(fault("the effects did not settle within " + ROUNDS + " rounds"));
        break;
      }
      for (const x of due) if (x.live && this.gate(x)) this.fire(x);
      ch = this.woke;
      this.woke = new Set();
    }
    this.woke = null;
  }

  gate(x) {
    const first = !x.on;
    x.on = true;
    if (!x.dep) return first;
    let v;
    try {
      v = this.h.read(x.dep, x.m);
    } catch (e) {
      console.error(e);
      return false;
    }
    const changed = first || !$.eq(x.last, v);
    x.last = v;
    if (x.q) x.q.key = v;
    return x.run === 0 ? first : x.run === 1 ? !first && changed : changed;
  }

  fire(x) {
    if (x.res) {
      this.reload(x);
      return;
    }
    if (x.clean) {
      const c = x.clean;
      x.clean = null;
      this.h.drop(c, x.m);
    }
    const t = this.h.transact(x.body, x.m);
    if (!t) return;
    this.land(t, x.m);
    for (const i of t.changed) this.woke.add(i);
    if (typeof t.value === "function") x.clean = t.value;
  }

  // The tasks: each runs from the commit that starts it, owned by the mount
  // of the instance that starts it, in a named slot under its policy (0
  // keep_latest, 1 drop_new, 2 queue, 2 + n parallel(n)); its value runs its
  // `done` handler as a new transaction, its cancellation its `cancelled`.
  owner(m, instance) {
    for (let x = m; x; x = x.p) if (x.o && x.o.includes(instance)) return x;
    return this.top;
  }

  launch(list, m) {
    for (const s of list) {
      const o = this.owner(m, s.instance);
      const slot = s.slot === null ? null : { o, i: s.instance, n: s.slot, policy: s.policy };
      if (slot) {
        const running = this.jobs.filter((j) => j.slot && same(j.slot, slot));
        if (slot.policy === 0) running.forEach((j) => this.cancel(j));
        else if (slot.policy === 1) {
          if (running.length) continue;
        } else if (running.length >= room(slot)) {
          this.held.push({ s, slot, m, o });
          continue;
        }
      }
      this.begin(s, slot, m, o, null);
    }
  }

  begin(s, slot, m, o, load) {
    const tk = { off: false, w: new Set() };
    const j = { tk, s, slot, m, o, load };
    let p;
    try {
      p = this.h.read(() => s.task(tk, ...s.args), m);
    } catch (e) {
      console.error(e);
      this.free(slot);
      return null;
    }
    this.jobs.push(j);
    p.then(
      (v) => { if (!tk.off) this.finish(j, v); },
      (e) => {
        if (tk.off) return;
        console.error(e);
        this.finish(j, undefined);
      },
    );
    return j;
  }

  finish(j, v) {
    const at = this.jobs.indexOf(j);
    if (at < 0) return;
    this.jobs.splice(at, 1);
    if (j.load) this.loaded(j, v);
    else if (v !== undefined && j.s.done) this.run(() => j.s.done(v), j.m);
    this.free(j.slot);
  }

  cancel(j) {
    const at = this.jobs.indexOf(j);
    if (at < 0) return;
    this.jobs.splice(at, 1);
    stop(j);
    if (!j.load && j.s.cancelled) this.run(() => j.s.cancelled(), j.m);
  }

  // Starts the first start `slot` holds, when it has room now.
  free(slot) {
    if (!slot) return;
    if (this.jobs.filter((j) => j.slot && same(j.slot, slot)).length >= room(slot)) return;
    const at = this.held.findIndex((h) => same(h.slot, slot));
    if (at < 0) return;
    const [h] = this.held.splice(at, 1);
    this.begin(h.s, h.slot, h.m, h.o, null);
  }

  // The resources: a key change moves the state to `loading` — `reloading`
  // while it holds a value — and starts the loader, after the debounce; the
  // loader's `Result` settles it while the key is still the one it loaded,
  // and stays cached under its key for `cache_for`. Each keeps its loads and
  // cache in its store `s`, its own or its pool's: a key loading there is
  // joined, its result settling every resource waiting `w` on it, and
  // `keep_latest` cancels a load once none waits on it.
  reload(x) {
    const q = x.q, R = x.res, s = x.s;
    if (q.key === undefined) return;
    q.gen++;
    if (q.wait !== null) {
      clearTimeout(q.wait);
      q.wait = null;
    }
    for (const l of s.loads.slice()) {
      if ($.eq(l.key, q.key)) continue;
      l.w = l.w.filter((y) => y !== x && y.live);
      if (R.k && !l.w.length) {
        s.loads.splice(s.loads.indexOf(l), 1);
        this.cancel(l.j);
      }
    }
    const hit = s.cache.find((c) => $.eq(c.key, q.key));
    if (hit) {
      this.run(() => R.w(hit.state), x.m);
      return;
    }
    let now;
    try {
      now = this.h.read(R.s, x.m);
    } catch (e) {
      console.error(e);
      return;
    }
    const full = now !== null && typeof now === "object" && (now.t === READY || now.t === RELOADING);
    this.run(() => R.w(full ? { t: RELOADING, f: [now.f[0]] } : LOADING), x.m);
    const key = q.key;
    if (R.d === null) this.load(x, key);
    else {
      const gen = q.gen;
      q.wait = setTimeout(() => {
        q.wait = null;
        if (x.live && q.gen === gen) this.load(x, key);
      }, R.d * 1000);
    }
  }

  load(x, key) {
    const s = x.s;
    const flying = s.loads.find((l) => $.eq(l.key, key));
    if (flying) {
      if (!flying.w.includes(x)) flying.w.push(x);
      return;
    }
    const t = this.h.transact(x.body, x.m);
    if (!t || !t.starts.length) return;
    const owner = x.res.p === null ? x.m : this.top;
    const j = this.begin(t.starts[0], null, x.m, owner, { s, R: x.res, key });
    if (j) s.loads.push({ j, key, w: [x] });
  }

  loaded(j, v) {
    const { s, R, key } = j.load;
    const at = s.loads.findIndex((l) => l.j === j);
    const w = at < 0 ? [] : s.loads.splice(at, 1)[0].w;
    if (v === undefined) return;
    if (v === null || typeof v !== "object" || v.t > 1 || v.f.length !== 1) {
      console.error(fault("a resource's loader returned no `Result`"));
      return;
    }
    const state = { t: v.t === 0 ? READY : ERROR, f: [v.f[0]] };
    if (R.c !== null && (state.t === READY || R.e)) {
      const stale = s.cache.findIndex((c) => $.eq(c.key, key));
      if (stale >= 0) clearTimeout(s.cache.splice(stale, 1)[0].x);
      const c = { key, state, x: 0 };
      c.x = setTimeout(() => { s.cache = s.cache.filter((e) => e !== c); }, R.c * 1000);
      s.cache.push(c);
    }
    for (const y of w) if (y.live && $.eq(y.q.key, key)) this.run(() => y.res.w(state), y.m);
  }

  // Ends the work of mount `m`: its effects clean up, the tasks it owns stop
  // without a handler, and what their slots hold is dropped.
  end(m) {
    m.live = false;
    for (const x of m.fx) {
      x.live = false;
      if (x.q) {
        if (x.q.wait !== null) clearTimeout(x.q.wait);
        if (x.res.p === null) for (const c of x.s.cache) clearTimeout(c.x);
      }
      if (x.clean) this.h.drop(x.clean, m);
      x.clean = null;
    }
    const gone = this.jobs.filter((j) => j.o === m);
    if (gone.length) {
      this.jobs = this.jobs.filter((j) => j.o !== m);
      gone.forEach(stop);
    }
    this.held = this.held.filter((h) => h.o !== m);
  }
}

const hit = (deps, changed) => changed === null || deps === null || deps.some((d) => changed.has(d));
const same = (a, b) => a.o === b.o && a.i === b.i && a.n === b.n;
const room = (slot) => (slot.policy > 2 ? slot.policy - 2 : 1);
const stop = (j) => {
  j.tk.off = true;
  for (const w of j.tk.w) w();
  j.tk.w.clear();
};
