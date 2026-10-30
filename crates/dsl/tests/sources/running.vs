import viso::time

component Counter {
    input label: String
    state n = 1
    task later() -> I64 {
        await time::sleep(20ms)
        10
    }
    action add(by: I64) {
        n += by
    }
    effect boot {
        transaction { add(1) }
        start later() { success (v) { add(v) } }
    }
    view {
        Text { text: "{label}={n}" }
    }
}

component Tally {
    input label: String
    task echo(s: String) -> Result<String, String> {
        await time::sleep(10ms)
        Ok(s)
    }
    resource seen: Resource<String, String> {
        load = echo(label)
        key = 0
        scope = ResourceScope::shared
    }
    view {
        Text {
            text: match seen.state {
                ResourceState::ready(s) => "tally=" + label + s
                _ => "tally"
            }
        }
    }
}

export component Demo {
    state log = ""
    state runs = 0
    state count = 0
    state names = ["x", "y"]
    state query = "a"

    task wait(v: I64) -> I64 {
        await time::sleep(30ms)
        v
    }

    task find(q: String) -> Result<List<String>, String> {
        await time::sleep(10ms)
        if q == "" {
            return Err("empty")
        }
        Ok([q, q + q])
    }

    resource results: Resource<List<String>, String> {
        load = find(query)
        key = query
        policy = [ResourcePolicy::keep_latest, ResourcePolicy::debounce(20ms)]
    }

    action note(s: String) {
        log = log + s
    }

    action bump() {
        count += 1
    }

    effect boot {
        start wait(1) as job { success (v) { note("a{v}") } cancelled { note("c") } }
        start wait(2) as job {
            success (v) { note("a{v}"); bump() }
            cancelled { note("c") }
        }
        start wait(3) as queued {
            policy = [TaskPolicy::queue]
            success (v) { note("q{v}") }
        }
        start wait(4) as queued {
            policy = [TaskPolicy::queue]
            success (v) { note("q{v}") }
        }
        start wait(5) as dropped {
            policy = [TaskPolicy::drop_new]
            success (v) { note("d{v}") }
        }
        start wait(6) as dropped {
            policy = [TaskPolicy::drop_new]
            success (v) { note("d{v}") }
        }
    }

    effect follow when (count) {
        transaction {
            runs += 1
            if count == 1 {
                query = "b"
                names = ["y"]
            }
        }
    }

    view {
        Column {
            Text { text: "log=" + log }
            Text { text: "runs={runs}" }
            match results.state {
                ResourceState::ready(items) => {
                    for item in items key item {
                        Text { text: "item=" + item }
                    }
                }
                ResourceState::error(message) => {
                    Text { text: "error=" + message }
                }
                _ => {
                    Text { text: "loading" }
                }
            }
            for name in names key name {
                Counter { label: name }
            }
            Tally { label: "p" }
            Tally { label: "r" }
        }
    }
}
