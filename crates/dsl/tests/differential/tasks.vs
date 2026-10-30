import viso::time

export component Jobs {
    state a = 0
    state b = 0
    state log = ""

    task wait() -> I64 { await time::sleep(1s); 3 }

    task fetch(x: I64) -> Result<I64, String> {
        await time::sleep(1s)
        if x < 0 { Err("negative") } else { Ok(x * 2) }
    }

    task count(n: I64) -> I64 {
        let mut i = 0
        let mut t = 0
        while i < n {
            await time::sleep(1s)
            t += i * i
            i += 1
        }
        t
    }

    task nested(n: I64) -> I64 {
        let w = await wait()
        let c = await count(n)
        let f = |x: I64| x + w
        f(c)
    }

    task strict(n: I64) -> I64 {
        await time::sleep(1s)
        let xs = [1, 2, 3]
        xs[n] * 9223372036854775807
    }

    task immediate(n: I64) -> I64 { n + 1 }

    action go() {
        start fetch(a) as job { success (v) { b = v } error (e) { log = e } }
        a += 1
    }

    action chain(n: I64) {
        start wait() { success (v) { b += v + n } }
        start count(n)
        log = format("{n}", n: n)
    }

    view { Text { text: log } }
}
