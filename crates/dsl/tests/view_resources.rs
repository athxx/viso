//! A view's resources: a state only its loader writes, loading on mount and
//! on each key change through the task machinery, its result settled as
//! `ready` or `error` while the key is still the one it loaded; `keep_latest`,
//! `debounce`, `cache_for` and `cache_errors`; unmount, hot reload and the
//! release package. And the checks: `load` and `key` once (`E4301`), the
//! policies and the scope (`E4302`), a `StableKey` key (`E2701`), a task-call
//! loader (`E4401`) of the declared result (`E2103`), no writes (`E2110`),
//! exhaustive `ResourceState` matches (`E2301`).

mod support;

use std::time::Duration;

use support::{Rt, origin};
use viso_dsl::frontend::compile_file;
use viso_view::Value;

/// No diagnostic.
const CLEAN: [&str; 0] = [];

/// The diagnostic codes compiling `members` into a component reports.
fn codes(members: &str) -> Vec<&'static str> {
    let source = format!(
        "import viso::time;\nexport component C {{\n    state q = 1;\n    state f = 1.5;\n    \
         task fetch(x: I64) -> Result<I64, String> {{ await time::sleep(1s); Ok(x) }}\n    \
         task plain() -> I64 {{ 3 }}\n    action poke() {{ q += 1; }}\n    {members}\n    \
         view {{ Text {{}} }}\n}}\n"
    );
    let compiled = compile_file(&source, &origin());
    let diagnostics: Vec<_> = compiled.errors().collect();
    eprintln!("{diagnostics:#?}");
    diagnostics.iter().map(|d| d.code).collect()
}

/// A resource `r` of `Resource<I64, String>` with `items`.
fn resource(items: &str) -> String {
    format!("resource r: Resource<I64, String> {{ {items} }}")
}

#[test]
fn a_resource_takes_load_and_key_once() {
    assert_eq!(codes(&resource("load = fetch(q); key = q;")), CLEAN);
    assert_eq!(
        codes(&resource(
            "key = q; scope = ResourceScope::component; load = fetch(q); policy = [];"
        )),
        CLEAN,
        "the items in any order"
    );
    assert_eq!(codes(&resource("key = q;")), ["E4301"]);
    assert_eq!(codes(&resource("load = fetch(q);")), ["E4301"]);
    assert_eq!(
        codes(&resource("load = fetch(q); key = q; key = q;")),
        ["E4301"]
    );
    assert_eq!(
        codes(&resource(
            "load = fetch(q); key = q; policy = []; policy = [];"
        )),
        ["E4301"]
    );
    assert!(
        !codes(&resource("load = fetch(q); key = q; retry = 3;")).is_empty(),
        "an unknown item is rejected"
    );
}

#[test]
fn the_policies_and_scope_are_checked() {
    for policy in [
        "[ResourcePolicy::keep_latest]",
        "[ResourcePolicy::debounce(250ms), ResourcePolicy::keep_latest]",
        "[ResourcePolicy::cache_for(5min), ResourcePolicy::cache_errors]",
    ] {
        assert_eq!(
            codes(&resource(&format!(
                "load = fetch(q); key = q; policy = {policy};"
            ))),
            CLEAN,
            "{policy}"
        );
    }
    for bad in [
        "ResourcePolicy::keep_latest",
        "[ResourcePolicy::sometimes]",
        "[ResourcePolicy::keep_latest, ResourcePolicy::keep_latest]",
        "[ResourcePolicy::debounce(0ms)]",
        "[ResourcePolicy::debounce(q)]",
        "[ResourcePolicy::cache_errors]",
    ] {
        assert_eq!(
            codes(&resource(&format!(
                "load = fetch(q); key = q; policy = {bad};"
            ))),
            ["E4302"],
            "{bad}"
        );
    }
    assert_eq!(
        codes(&resource(
            "load = fetch(q); key = q; scope = ResourceScope::app;"
        )),
        ["E4302"]
    );
}

#[test]
fn the_key_is_a_pure_stable_key_and_the_load_a_task_call_of_the_result() {
    assert_eq!(
        codes(&resource("load = fetch(q); key = (q, \"a\");")),
        CLEAN
    );
    assert_eq!(codes(&resource("load = fetch(q); key = f;")), ["E2701"]);
    assert_eq!(codes(&resource("load = fetch(q); key = [q];")), ["E2701"]);
    assert_eq!(
        codes(&resource("load = fetch(q); key = { poke(); q };")),
        ["E2502"]
    );
    assert_eq!(codes(&resource("load = plain(); key = q;")), ["E2103"]);
    assert_eq!(codes(&resource("load = q; key = q;")), ["E4401"]);
    assert_eq!(
        codes("resource r: Resource<String, String> { load = fetch(q); key = q; }"),
        ["E2103"]
    );
    assert_eq!(
        codes("resource r: List<I64> { load = fetch(q); key = q; }"),
        ["E2103"]
    );
}

#[test]
fn a_resource_is_read_through_its_state_and_never_written() {
    let r = resource("load = fetch(q); key = q;");
    let all = "match r.state { ResourceState::idle => 0, ResourceState::loading => 1, \
               ResourceState::ready(v) => v, ResourceState::error(e) => 3, \
               ResourceState::reloading(v) => v }";
    assert_eq!(codes(&format!("{r} computed c: I64 = {all};")), CLEAN);
    assert_eq!(
        codes(&format!(
            "{r} computed c: I64 = match r.state {{ ResourceState::ready(v) => v, _ => 0 }};"
        )),
        CLEAN
    );
    assert_eq!(
        codes(&format!(
            "{r} computed c: I64 = match r.state {{ ResourceState::ready(v) => v, \
             ResourceState::idle => 0 }};"
        )),
        ["E2301"]
    );
    assert_eq!(
        codes(&format!(
            "{r} computed c: String = match r.state {{ ResourceState::error(e) => e, _ => \"\" }};"
        )),
        CLEAN,
        "`error` carries the error"
    );
    assert_eq!(codes(&format!("{r} computed c: I64 = r.value;")), ["E2001"]);
    assert_eq!(codes(&format!("{r} action go() {{ r = r; }}")), ["E2110"]);
}

#[test]
fn a_system_declares_no_resource() {
    let source = "import viso::time;\ntask get() -> Result<I64, String> { Ok(1) }\n\
                  system S {\n    resource r: Resource<I64, String> { load = get(); key = 1; }\n}\n";
    let compiled = compile_file(source, &origin());
    let codes: Vec<_> = compiled.errors().map(|d| d.code).collect();
    assert!(codes.contains(&"E9109"), "{codes:?}");
}

/// A view loading `r` by key `q` under `policy`, showing its state in a view
/// `match`; its buttons: `q += 1`, `q = 1`, `q = -q`, and toggling the
/// instance that holds its own resource.
fn search(policy: &str) -> String {
    format!(
        r#"
import viso::time;

component Item {{
    resource own: Resource<I64, String> {{ load = fetch(7); key = 0; }}
    task fetch(n: I64) -> Result<I64, String> {{ await time::sleep(1s); Ok(n) }}
    view {{ Text {{ width: 20dp; height: 20dp; }} }}
}}

export component Search {{
    state q = 1;
    state show = false;
    resource r: Resource<I64, String> {{
        load = fetch(q);
        key = q;
        policy = [{policy}];
    }}
    task fetch(n: I64) -> Result<I64, String> {{
        await time::sleep(1s);
        if n < 0 {{ return Err("negative"); }}
        Ok(n * 10)
    }}
    view {{
        Column {{
            width: 400dp;
            height: 100dp;
            Text {{ width: 20dp; height: 20dp; on click {{ q += 1; }} }}
            Text {{ width: 20dp; height: 20dp; on click {{ q = 1; }} }}
            Text {{ width: 20dp; height: 20dp; on click {{ q = -q; }} }}
            Text {{ width: 20dp; height: 20dp; on click {{ show = !show; }} }}
            match r.state {{
                ResourceState::ready(v) => {{ Text {{ text: format("{{}}", v); }} }},
                _ => {{ }},
            }}
            if show {{ Item {{}} }}
        }}
    }}
}}
"#
    )
}

/// The resource's state, spelled.
fn state(rt: &Rt, name: &str) -> String {
    let host = rt.view.as_ref().expect("a view").borrow();
    let slot = host.state_slot(name).expect("the resource's slot");
    match host.state(slot) {
        Some(Value::Int(0)) => "idle".into(),
        Some(Value::Int(1)) => "loading".into(),
        Some(Value::Agg(agg)) => {
            let name = ["", "", "ready", "error", "reloading"][agg.tag as usize];
            match &agg.fields[0] {
                Value::Int(n) => format!("{name}({n})"),
                Value::Str(s) => format!("{name}({s})"),
                other => format!("{name}({other:?})"),
            }
        }
        other => format!("{other:?}"),
    }
}

#[test]
fn a_resource_loads_on_mount_and_settles_ready_or_error() {
    let mut rt = Rt::mount(&search("ResourcePolicy::keep_latest"));
    assert_eq!(state(&rt, "r"), "loading", "the mount started the loader");
    assert_eq!(rt.clock.pending(), 1);
    rt.ring();
    assert_eq!(state(&rt, "r"), "ready(10)");
    assert_eq!(rt.fault(), None);

    rt.click(2);
    assert_eq!(
        state(&rt, "r"),
        "reloading(10)",
        "a key change keeps the value while it reloads"
    );
    rt.ring();
    assert_eq!(state(&rt, "r"), "error(negative)");
    rt.click(0);
    assert_eq!(state(&rt, "r"), "loading", "an error is no value to keep");
    rt.ring();
    assert_eq!(state(&rt, "r"), "ready(0)", "q went from -1 to 0");
    assert_eq!(rt.store.task_count(), 0);
}

#[test]
fn a_stale_load_never_overwrites_a_newer_key() {
    // keep_latest cancels the load in flight.
    let mut rt = Rt::mount(&search("ResourcePolicy::keep_latest"));
    rt.click(0);
    assert_eq!(rt.store.task_count(), 1, "the first load was cancelled");
    rt.ring();
    assert_eq!(state(&rt, "r"), "ready(20)");

    // Without it, both finish; only the current key's settles the state.
    let mut rt = Rt::mount(&search(""));
    rt.click(0);
    assert_eq!(rt.store.task_count(), 2);
    assert_eq!(rt.ring(), 2);
    assert_eq!(state(&rt, "r"), "ready(20)");
    assert_eq!(rt.fault(), None);
}

#[test]
fn debounce_delays_the_loader_not_the_state() {
    let mut rt = Rt::mount(&search("ResourcePolicy::debounce(250ms)"));
    assert_eq!(state(&rt, "r"), "loading");
    assert_eq!(rt.store.task_count(), 1, "the debounce, not yet a load");
    rt.click(0);
    rt.click(0);
    assert_eq!(state(&rt, "r"), "loading");
    assert_eq!(
        rt.store.task_count(),
        1,
        "each change restarted the debounce"
    );
    rt.ring();
    assert_eq!(
        rt.store.task_count(),
        1,
        "the debounce ran out and started one load"
    );
    assert_eq!(state(&rt, "r"), "loading");
    rt.ring();
    assert_eq!(state(&rt, "r"), "ready(30)");
}

#[test]
fn cache_for_settles_a_returning_key_without_loading() {
    let mut rt = Rt::mount(&search(
        "ResourcePolicy::cache_for(1min), ResourcePolicy::cache_errors",
    ));
    rt.ring();
    assert_eq!(state(&rt, "r"), "ready(10)");
    // The expiry of key 1 sleeps now.
    assert_eq!(rt.clock.pending(), 1);
    rt.click(2);
    rt.ring_upto(Duration::from_secs(1));
    assert_eq!(state(&rt, "r"), "error(negative)");
    rt.click(1);
    assert_eq!(state(&rt, "r"), "ready(10)", "key 1 came from the cache");
    rt.click(2);
    assert_eq!(state(&rt, "r"), "error(negative)", "errors are cached too");
    assert_eq!(rt.fault(), None);
}

#[test]
fn an_expired_entry_loads_again() {
    let mut rt = Rt::mount(&search("ResourcePolicy::cache_for(1s)"));
    rt.ring();
    assert_eq!(state(&rt, "r"), "ready(10)");
    rt.ring();
    rt.click(0);
    rt.ring();
    rt.click(1);
    assert_eq!(
        state(&rt, "r"),
        "reloading(20)",
        "the entry of key 1 expired"
    );
    rt.ring();
    assert_eq!(state(&rt, "r"), "ready(10)");
}

#[test]
fn an_instance_in_region_content_loads_its_own_and_unmounting_drops_it() {
    let mut rt = Rt::mount(&search("ResourcePolicy::keep_latest"));
    rt.ring();
    rt.click(3);
    assert_eq!(rt.store.task_count(), 1, "the instance's own load");
    rt.click(3);
    assert_eq!(rt.store.task_count(), 0, "freeing its root dropped it");
    rt.click(3);
    rt.ring();
    assert_eq!(rt.fault(), None);
    assert_eq!(rt.view.as_ref().unwrap().borrow().task_count(), 0);
}

#[test]
fn a_hot_reload_keeps_the_state_and_loads_again() {
    let source = search("ResourcePolicy::keep_latest");
    let mut rt = Rt::mount(&source);
    rt.ring();
    assert_eq!(state(&rt, "r"), "ready(10)");
    rt.reload(&source.replace("Ok(n * 10)", "Ok(n * 100)"));
    assert_eq!(state(&rt, "r"), "reloading(10)");
    rt.ring();
    assert_eq!(state(&rt, "r"), "ready(100)", "the new loader ran");
    rt.click(0);
    rt.reload(&source);
    assert_eq!(rt.store.task_count(), 1, "the load in flight was cancelled");
    rt.ring();
    assert_eq!(state(&rt, "r"), "ready(20)");
}

#[test]
fn the_release_package_runs_the_same() {
    let mut rt = Rt::packaged(&search("ResourcePolicy::keep_latest"));
    assert_eq!(state(&rt, "r"), "loading");
    rt.ring();
    assert_eq!(state(&rt, "r"), "ready(10)");
    rt.click(0);
    rt.ring();
    assert_eq!(state(&rt, "r"), "ready(20)");
    assert_eq!(rt.fault(), None);
}
