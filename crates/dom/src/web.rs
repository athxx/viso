//! The page script's side of the runtime: `visoDom` opens the page, and the
//! page it returns is the script's `V`.
//!
//! Values cross in the representation the page script computes with:
//! integers as `BigInt`, floats as numbers, strings as strings, lists as
//! arrays, aggregates as `{t, f}`, `None` as `null`. A closure or a native
//! object stays in the runtime: the script holds `{$o}`, a token the
//! runtime looks it up by and forgets once the script lets go of it.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use js_sys::{Array, BigInt, Function, Object, Promise, Reflect};
use viso_behavior::native::Timers;
use viso_behavior::{Aggregate, Fault, NodeAction, TaskPolicy, TaskStep, Value};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::Page;

#[wasm_bindgen]
extern "C" {
    type FinalizationRegistry;

    #[wasm_bindgen(constructor)]
    fn new(cleanup: &Function) -> FinalizationRegistry;

    #[wasm_bindgen(method)]
    fn register(this: &FinalizationRegistry, target: &JsValue, held: &JsValue);
}

/// Opens the page of component `component` on `module`, the page's own
/// build of it, or on the app's, which must be build `build`: the page the
/// app shares with the script, open already where the app mounted its
/// island.
///
/// # Errors
///
/// What keeps the view from running.
#[wasm_bindgen(js_name = visoDom)]
pub fn viso_dom(component: &str, module: Option<Vec<u8>>, build: u64) -> Result<DomPage, JsError> {
    let page = match module {
        Some(module) => Page::open(component, Some(&module), build).map(|p| Rc::new(RefCell::new(p))),
        None => crate::shared(component).and_then(|page| {
            if page.borrow().build() == build {
                Ok(page)
            } else {
                Err(format!(
                    "the page and the app were built from different sources of `{component}`: build them again together"
                ))
            }
        }),
    }
    .map_err(|e| JsError::new(&e))?;
    page.borrow_mut()
        .services_mut()
        .insert::<Box<dyn Timers>>(Box::new(PageTimers));
    let inner = Rc::new(RefCell::new(Inner {
        page,
        opaques: Vec::new(),
        free: Vec::new(),
    }));
    let weak = Rc::downgrade(&inner);
    let cleanup = Closure::<dyn FnMut(u32)>::new(move |id: u32| {
        if let Some(inner) = weak.upgrade()
            && let Ok(mut inner) = inner.try_borrow_mut()
            && let Some(slot) = inner.opaques.get_mut(id as usize)
        {
            *slot = None;
            inner.free.push(id);
        }
    });
    let registry = FinalizationRegistry::new(cleanup.as_ref().unchecked_ref());
    Ok(DomPage {
        inner,
        registry,
        _cleanup: cleanup,
    })
}

/// The page's VM, as the page script calls it.
#[wasm_bindgen]
pub struct DomPage {
    inner: Rc<RefCell<Inner>>,
    registry: FinalizationRegistry,
    _cleanup: Closure<dyn FnMut(u32)>,
}

struct Inner {
    page: Rc<RefCell<Page>>,
    /// The closures and native objects the script holds tokens of.
    opaques: Vec<Option<Value>>,
    free: Vec<u32>,
}

#[wasm_bindgen]
impl DomPage {
    /// The value of entry `entry` for `args`; nothing it does is kept.
    pub fn e(&self, entry: u32, args: &Array) -> Result<JsValue, JsError> {
        let args = self.values(args);
        let value = self.inner.borrow().page.borrow_mut().evaluate(entry, &args);
        self.answer(value)
    }

    /// Runs entry `entry` with `args` as a transaction.
    pub fn t(&self, entry: u32, args: &Array) -> Result<JsValue, JsError> {
        let args = self.values(args);
        let value = self.inner.borrow().page.borrow_mut().transact(entry, &args);
        self.answer(value)
    }

    /// Calls the closure `callee` with `args` as a transaction.
    pub fn c(&self, callee: &JsValue, args: &Array) -> Result<JsValue, JsError> {
        let callee = self.value(callee);
        let args = self.values(args);
        let value = self.inner.borrow().page.borrow_mut().call(&callee, &args);
        self.answer(value)
    }

    /// Puts `value` in state slot `slot`.
    pub fn put(&self, slot: u32, value: &JsValue) {
        let value = self.value(value);
        self.inner.borrow().page.borrow_mut().put(slot, value);
    }

    /// State slot `slot`.
    pub fn get(&self, slot: u32) -> JsValue {
        let value = self.inner.borrow().page.borrow().state(slot);
        self.js(&value)
    }

    /// Writes each `[slot, value]` of `changes`.
    pub fn w(&self, changes: &Array) {
        let changes: Vec<(u32, Value)> = changes
            .iter()
            .filter_map(|change| {
                let change: Array = change.dyn_into().ok()?;
                let slot = change.get(0).as_f64()? as u32;
                Some((slot, self.value(&change.get(1))))
            })
            .collect();
        self.inner.borrow().page.borrow_mut().write(changes);
    }

    /// Puts the default theme in each of `slots`.
    pub fn theme(&self, slots: Vec<u32>) {
        self.inner.borrow().page.borrow_mut().theme(&slots);
    }

    /// What the calls since the last take did: `{c, e, s, n}`, the slots
    /// they changed, their events as `[event, args]`, their starts and
    /// their requests of named nodes as `[action, key, args]`.
    pub fn take(&self) -> JsValue {
        let batch = self.inner.borrow().page.borrow_mut().take();
        let out = Object::new();
        let changed: Array = batch.changed.iter().map(|&s| JsValue::from(s)).collect();
        let events: Array = batch
            .events
            .iter()
            .map(|event| {
                let pair = Array::new();
                pair.push(&JsValue::from(event.index));
                pair.push(&self.list(&event.args));
                JsValue::from(pair)
            })
            .collect();
        let starts: Array = batch
            .starts
            .iter()
            .map(|start| {
                let s = Object::new();
                set(&s, "k", &JsValue::from(start.task));
                set(&s, "a", &self.list(&start.args));
                set(
                    &s,
                    "d",
                    &start.done.as_ref().map_or(JsValue::NULL, |d| self.js(d)),
                );
                set(
                    &s,
                    "x",
                    &start
                        .cancelled
                        .as_ref()
                        .map_or(JsValue::NULL, |c| self.js(c)),
                );
                set(&s, "i", &JsValue::from(start.instance));
                set(&s, "n", &start.slot.map_or(JsValue::NULL, JsValue::from));
                let policy = match start.policy {
                    TaskPolicy::KeepLatest => 0,
                    TaskPolicy::DropNew => 1,
                    TaskPolicy::Queue => 2,
                    TaskPolicy::Parallel(n) => 2 + n,
                };
                set(&s, "p", &JsValue::from(policy));
                JsValue::from(s)
            })
            .collect();
        let nodes: Array = batch
            .nodes
            .iter()
            .map(|request| {
                let triple = Array::new();
                triple.push(&JsValue::from(request.action as u32));
                // An animation's own request names it by its id, its only
                // argument.
                if matches!(request.action, NodeAction::Cancel | NodeAction::Finish) {
                    triple.push(&request.args.first().map_or(JsValue::NULL, |id| self.js(id)));
                    triple.push(&Array::new());
                } else {
                    triple.push(&BigInt::from(request.node).into());
                    triple.push(&self.list(&request.args));
                }
                JsValue::from(triple)
            })
            .collect();
        set(&out, "c", &changed);
        set(&out, "e", &events);
        set(&out, "s", &starts);
        set(&out, "n", &nodes);
        out.into()
    }

    /// Runs the task of chunk `task` with `args`: a promise of its value,
    /// which settles with `undefined` once the task token `tk` is off.
    pub fn run(&self, task: u32, args: &Array, tk: JsValue) -> Promise {
        let inner = Rc::clone(&self.inner);
        let args = self.values(args);
        let this = self.share();
        future_to_promise(async move {
            let mut step = inner
                .borrow()
                .page
                .borrow_mut()
                .start_task(task, &args)
                .map_err(fault)?;
            loop {
                match step {
                    TaskStep::Done(value) => return Ok(this.js(&value)),
                    TaskStep::Awaiting(fiber, work) => {
                        let result = work.await;
                        if Reflect::get(&tk, &"off".into()).is_ok_and(|off| off.is_truthy()) {
                            return Ok(JsValue::UNDEFINED);
                        }
                        step = inner
                            .borrow()
                            .page
                            .borrow_mut()
                            .resume_task(fiber, result)
                            .map_err(fault)?;
                    }
                }
            }
        })
    }

    /// Pushes `value` to input slot `slot` of island `island`.
    pub fn i(&self, island: u32, slot: u32, value: &JsValue) {
        crate::island::push(island, slot, self.value(value));
    }

    /// Has `listener` run each event an island emits, called with its
    /// handler entry and its record once the frame that emitted it is over.
    pub fn listen(&self, listener: Function) {
        let shared = self.share();
        let deliver = Closure::<dyn FnMut()>::new(crate::island::deliver).into_js_value();
        let queue = Reflect::get(&js_sys::global(), &"queueMicrotask".into())
            .ok()
            .and_then(|f| f.dyn_into::<Function>().ok());
        crate::island::listen(
            move |entry, payload| {
                let payload = shared.js(&payload);
                if let Err(e) = listener.call2(&JsValue::NULL, &JsValue::from(entry), &payload) {
                    report(&e);
                }
            },
            move || {
                if let Some(queue) = &queue {
                    let _ = queue.call1(&JsValue::NULL, &deliver);
                }
            },
        );
    }

    /// Forgets the animation `id`, which the page ended.
    pub fn ended(&self, id: &JsValue) {
        if let Ok(id) = i64::try_from(id.clone()) {
            self.inner
                .borrow()
                .page
                .borrow_mut()
                .animation_ended(id as u64);
        }
    }
}

/// What a task holds of the page to hand its value back in the script's
/// representation.
struct Shared {
    inner: Rc<RefCell<Inner>>,
    registry: JsValue,
}

impl DomPage {
    fn share(&self) -> Shared {
        Shared {
            inner: Rc::clone(&self.inner),
            registry: self.registry.clone(),
        }
    }

    fn js(&self, value: &Value) -> JsValue {
        to_js(value, &mut self.inner.borrow_mut(), &self.registry)
    }

    fn list(&self, values: &[Value]) -> JsValue {
        let mut inner = self.inner.borrow_mut();
        values
            .iter()
            .map(|v| to_js(v, &mut inner, &self.registry))
            .collect::<Array>()
            .into()
    }

    fn value(&self, js: &JsValue) -> Value {
        from_js(js, &self.inner.borrow())
    }

    fn values(&self, args: &Array) -> Vec<Value> {
        let inner = self.inner.borrow();
        args.iter().map(|a| from_js(&a, &inner)).collect()
    }

    fn answer(&self, value: Result<Value, Fault>) -> Result<JsValue, JsError> {
        value
            .map(|v| self.js(&v))
            .map_err(|f| JsError::new(&format!("viso: {}", f.message)))
    }
}

impl Shared {
    fn js(&self, value: &Value) -> JsValue {
        to_js(
            value,
            &mut self.inner.borrow_mut(),
            self.registry.unchecked_ref(),
        )
    }
}

fn fault(fault: Fault) -> JsValue {
    JsError::new(&format!("viso: {}", fault.message)).into()
}

/// Logs `error`, which a handler the script runs threw, to the console.
fn report(error: &JsValue) {
    let error_fn = Reflect::get(&js_sys::global(), &"console".into())
        .and_then(|console| Reflect::get(&console, &"error".into()))
        .ok()
        .and_then(|f| f.dyn_into::<Function>().ok());
    if let Some(error_fn) = error_fn {
        let _ = error_fn.call1(&JsValue::NULL, error);
    }
}

fn set(target: &Object, key: &str, value: &JsValue) {
    let _ = Reflect::set(target, &key.into(), value);
}

fn to_js(value: &Value, inner: &mut Inner, registry: &FinalizationRegistry) -> JsValue {
    match value {
        Value::Nil => JsValue::NULL,
        Value::Int(i) => BigInt::from(*i).into(),
        Value::Float(f) => JsValue::from_f64(*f),
        Value::Str(s) => JsValue::from_str(s),
        Value::List(items) => items
            .iter()
            .map(|v| to_js(v, inner, registry))
            .collect::<Array>()
            .into(),
        Value::Agg(agg) => {
            let out = Object::new();
            set(&out, "t", &JsValue::from(agg.tag));
            let fields: Array = agg
                .fields
                .iter()
                .map(|v| to_js(v, inner, registry))
                .collect();
            set(&out, "f", &fields);
            out.into()
        }
        Value::Closure(_) | Value::Handle(_) => {
            let id = match inner.free.pop() {
                Some(id) => {
                    inner.opaques[id as usize] = Some(value.clone());
                    id
                }
                None => {
                    inner.opaques.push(Some(value.clone()));
                    (inner.opaques.len() - 1) as u32
                }
            };
            let token = Object::new();
            set(&token, "$o", &JsValue::from(id));
            registry.register(&token, &JsValue::from(id));
            token.into()
        }
    }
}

fn from_js(js: &JsValue, inner: &Inner) -> Value {
    if js.is_null() || js.is_undefined() {
        return Value::Nil;
    }
    if js.is_bigint() {
        return i64::try_from(js.clone()).map_or(Value::Nil, Value::Int);
    }
    if let Some(f) = js.as_f64() {
        return Value::Float(f);
    }
    if let Some(s) = js.as_string() {
        return Value::str(s);
    }
    if let Some(b) = js.as_bool() {
        return Value::Int(i64::from(b));
    }
    if Array::is_array(js) {
        let items: Vec<Value> = js
            .unchecked_ref::<Array>()
            .iter()
            .map(|v| from_js(&v, inner))
            .collect();
        return Value::List(Rc::new(items));
    }
    if let Ok(id) = Reflect::get(js, &"$o".into())
        && let Some(id) = id.as_f64()
    {
        return inner
            .opaques
            .get(id as usize)
            .cloned()
            .flatten()
            .unwrap_or_default();
    }
    let tag = Reflect::get(js, &"t".into())
        .ok()
        .and_then(|t| t.as_f64())
        .unwrap_or(0.0) as u32;
    let fields: Box<[Value]> = Reflect::get(js, &"f".into())
        .ok()
        .filter(Array::is_array)
        .map(|f| {
            f.unchecked_ref::<Array>()
                .iter()
                .map(|v| from_js(&v, inner))
                .collect()
        })
        .unwrap_or_default();
    Value::Agg(Rc::new(Aggregate { tag, fields }))
}

/// The page's timers: the browser's.
struct PageTimers;

impl Timers for PageTimers {
    fn sleep(&mut self, duration: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
        let ms = duration.as_secs_f64() * 1000.0;
        let promise = Promise::new(&mut |resolve, _| {
            let set_timeout = Reflect::get(&js_sys::global(), &"setTimeout".into())
                .ok()
                .and_then(|f| f.dyn_into::<Function>().ok());
            match set_timeout {
                Some(set_timeout) => {
                    let _ = set_timeout.call2(&JsValue::NULL, &resolve, &JsValue::from_f64(ms));
                }
                None => {
                    let _ = resolve.call0(&JsValue::NULL);
                }
            }
        });
        Box::pin(async move {
            let _ = JsFuture::from(promise).await;
        })
    }
}
