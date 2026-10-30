record Point {
    x: F64
    y: F64 = 1.0
}

enum Shape {
    Circle(F64)
    Square(F64)
    Empty
}

fn area(s: Shape) -> F64 {
    match s {
        Shape::Circle(r) => 3.0 * r * r
        Shape::Square(w) => w * w
        Shape::Empty => 0.0
    }
}

export component Core {
    state count = 0
    state items: List<I64> = [1, 2, 3]
    state label = ""
    state point = Point { x: 2.0 }
    state picked: Option<I64> = None
    computed doubled: I64 = count * 2
    computed total: I64 = sum(items) + doubled
    event changed(value: I64)
    event named(text: String)

    action add(by: I64) {
        count += by
        emit changed(count)
    }

    action scale(by: I64) {
        count = count * by
        emit changed(count)
    }

    action divide(by: I64) {
        count = count / by
        count = count % (by + 1)
    }

    action push(v: I64) {
        items.push(v)
        items[0] = items[0] + v
        emit changed(items.len())
    }

    action poke(i: I64, v: I64) {
        count += 1
        emit changed(count)
        items[i] = v
    }

    action rename(name: String) {
        label = "{name}:{count}"
        emit named(label)
    }

    action nudge(dx: F64) {
        point.x += dx
        point = Point { x: point.y, ..point }
    }

    action pick(i: I64) {
        picked = if i >= 0 && i < items.len() { Some(items[i]) } else { None }
    }

    fn sum(xs: List<I64>) -> I64 {
        let mut t = 0
        for x in xs {
            if x > 10 && x < 100 {
                continue
            }
            t += x
        }
        t
    }

    fn count_to(n: I64) -> I64 {
        let mut hits = 0
        for i in 0..=n {
            if i % 3 == 0 {
                continue
            }
            hits += 1
        }
        let mut k = 0
        while true {
            k += 1
            if k > n {
                break
            }
        }
        hits + k
    }

    fn classify(n: I64) -> String {
        match n {
            0 => "zero"
            1 | 2 => "few"
            _ => "many {n}"
        }
    }

    fn digit(n: I64) -> I64 {
        match n {
            1 => 10
            2 => 20
            3 => 30
            4 => 40
            _ => 0
        }
    }

    fn shapes(k: F64) -> F64 {
        area(Shape::Circle(k)) + area(Shape::Square(k + 1.0)) + area(Shape::Empty)
    }

    fn apply(k: I64) -> I64 {
        let add = |x: I64| x + k
        let twice = |x: I64| add(add(x))
        twice(3) * k
    }

    fn mul(a: I64, b: I64) -> I64 { a * b }

    fn sub(a: I64, b: I64) -> I64 { a - b }

    fn neg(a: I64) -> I64 { -a }

    fn at(xs: List<I64>, i: I64) -> I64 { xs[i] }

    fn ratio(a: F64, b: F64) -> F64 { a / b + a * b - (a - b) }

    fn widen(a: I64) -> F64 { a as F64 * 0.5 }

    fn narrow(a: F64) -> I64 { a as I64 }

    fn fib(n: I64) -> I64 {
        if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
    }

    fn spin(n: I64) -> I64 {
        let mut i = 0
        while i < n {
            i += 1
        }
        i
    }

    view { Text("{total}") }
}
