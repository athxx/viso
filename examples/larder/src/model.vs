// What the kitchen knows: recipes and what goes into them, the shopping
// list, the week's plan and the timers, and the pure functions over them.

import viso::i18n::MessageKey

@derive(Eq, StableKey)
export enum Course {
    breakfast
    lunch
    dinner
    dessert
    snack
}

@derive(Eq)
export enum Measure {
    gram
    millilitre
    piece
    spoon
    pinch
}

@derive(Eq, StableKey)
export enum Aisle {
    produce
    dairy
    bakery
    pantry
    spices
    frozen
}

export record Ingredient {
    name: String
    amount: F64
    unit: Measure
    aisle: Aisle
}

export record Step {
    // Its place in the method, from zero.
    index: I64 = 0
    text: String
    // Minutes a timer for this step runs; zero when it needs none.
    minutes: I64 = 0
}

export record Recipe {
    id: I64
    title: String
    summary: String
    course: Course
    minutes: I64
    serves: I64
    picture: SvgAsset
    tags: List<String> = []
    ingredients: List<Ingredient> = []
    steps: List<Step> = []
}

export record ShoppingItem {
    id: I64
    name: String
    aisle: Aisle
    note: String = ""
    bought: Bool = false
}

@derive(Eq, StableKey)
export enum Weekday {
    monday
    tuesday
    wednesday
    thursday
    friday
    saturday
    sunday
}

export record PlannedMeal {
    day: Weekday
    course: Course
    recipe: I64
}

export record KitchenTimer {
    id: I64
    label: String
    total: I64
    left: I64
    running: Bool = true
}

@derive(Eq, StableKey)
export enum Section {
    recipes
    shopping
    planner
    timers
    settings
}

@derive(Eq)
export enum UnitSystem {
    metric
    imperial
}

export record Preferences {
    measures: UnitSystem = UnitSystem::metric
    servings: I64 = 2
    compact_lists: Bool = false
    sounds: Bool = true
}

export fn course_key(course: Course) -> MessageKey {
    match course {
        Course::breakfast => "course.breakfast"
        Course::lunch => "course.lunch"
        Course::dinner => "course.dinner"
        Course::dessert => "course.dessert"
        Course::snack => "course.snack"
    }
}

export fn aisle_key(aisle: Aisle) -> MessageKey {
    match aisle {
        Aisle::produce => "aisle.produce"
        Aisle::dairy => "aisle.dairy"
        Aisle::bakery => "aisle.bakery"
        Aisle::pantry => "aisle.pantry"
        Aisle::spices => "aisle.spices"
        Aisle::frozen => "aisle.frozen"
    }
}

export fn day_key(day: Weekday) -> MessageKey {
    match day {
        Weekday::monday => "day.monday"
        Weekday::tuesday => "day.tuesday"
        Weekday::wednesday => "day.wednesday"
        Weekday::thursday => "day.thursday"
        Weekday::friday => "day.friday"
        Weekday::saturday => "day.saturday"
        Weekday::sunday => "day.sunday"
    }
}

export fn section_key(section: Section) -> MessageKey {
    match section {
        Section::recipes => "section.recipes"
        Section::shopping => "section.shopping"
        Section::planner => "section.planner"
        Section::timers => "section.timers"
        Section::settings => "section.settings"
    }
}

export fn section_icon(section: Section) -> String {
    match section {
        Section::recipes => "icons/book.svg"
        Section::shopping => "icons/basket.svg"
        Section::planner => "icons/calendar.svg"
        Section::timers => "icons/timer.svg"
        Section::settings => "icons/gear.svg"
    }
}

export fn course_color(course: Course) -> Color {
    match course {
        Course::breakfast => #f2b544
        Course::lunch => #5fb36b
        Course::dinner => #d0603f
        Course::dessert => #c46aa8
        Course::snack => #4f8fd6
    }
}

// An amount for `serves` people of a recipe written for `base`.
export fn scaled(amount: F64, base: I64, serves: I64) -> F64 {
    if base <= 0 { return amount }
    amount * (serves as F64) / (base as F64)
}

// An amount as a cook reads it: whole numbers bare, the rest to a quarter.
export fn amount_text(amount: F64) -> String {
    let quarters = (amount * 4.0 + 0.5) as I64
    let whole = quarters / 4
    let rest = quarters % 4
    let fraction = match rest {
        1 => "¼"
        2 => "½"
        3 => "¾"
        _ => ""
    }
    if whole == 0 && fraction != "" { return fraction }
    "{whole}{fraction}"
}

export fn unit_text(unit: Measure, amount: F64, measures: UnitSystem) -> String {
    match unit {
        Measure::gram => if measures == UnitSystem::imperial {
            "{amount_text(amount / 28.35)} oz"
        } else {
            "{amount_text(amount)} g"
        }
        Measure::millilitre => if measures == UnitSystem::imperial {
            "{amount_text(amount / 29.57)} fl oz"
        } else {
            "{amount_text(amount)} ml"
        }
        Measure::piece => amount_text(amount)
        Measure::spoon => "{amount_text(amount)} tbsp"
        Measure::pinch => "{amount_text(amount)} pinch"
    }
}

// Minutes as a clock face: `1:05` for sixty-five seconds.
export fn clock_text(seconds: I64) -> String {
    let m = seconds / 60
    let s = seconds % 60
    if s < 10 { "{m}:0{s}" } else { "{m}:{s}" }
}

export fn duration_text(minutes: I64) -> String {
    if minutes < 60 { return "{minutes} min" }
    let h = minutes / 60
    let m = minutes % 60
    if m == 0 { "{h} h" } else { "{h} h {m} min" }
}

// Whether `recipe` matches what the search box holds: its title or a tag
// containing the query, and the course when one is picked.
export fn matches(recipe: Recipe, query: String, course: Option<Course>) -> Bool {
    let in_course = match course {
        Option::Some(c) => recipe.course == c
        Option::None => true
    }
    if !in_course { return false }
    if query == "" { return true }
    if recipe.title.contains(query) { return true }
    for tag in recipe.tags {
        if tag.contains(query) { return true }
    }
    false
}

export fn find_recipe(recipes: List<Recipe>, id: I64) -> Option<Recipe> {
    for recipe in recipes {
        if recipe.id == id { return Option::Some(recipe) }
    }
    Option::None
}

export fn left_to_buy(items: List<ShoppingItem>) -> I64 {
    let mut n = 0
    for item in items {
        if !item.bought { n += 1 }
    }
    n
}

export fn in_aisle(items: List<ShoppingItem>, aisle: Aisle) -> List<ShoppingItem> {
    let mut out: List<ShoppingItem> = []
    for item in items {
        if item.aisle == aisle { out.push(item) }
    }
    out
}

export fn planned(plan: List<PlannedMeal>, day: Weekday, course: Course) -> Option<I64> {
    for meal in plan {
        if meal.day == day && meal.course == course { return Option::Some(meal.recipe) }
    }
    Option::None
}

export fn all_aisles() -> List<Aisle> {
    [Aisle::produce, Aisle::dairy, Aisle::bakery, Aisle::pantry, Aisle::spices, Aisle::frozen]
}

export fn all_days() -> List<Weekday> {
    [
        Weekday::monday,
        Weekday::tuesday,
        Weekday::wednesday,
        Weekday::thursday,
        Weekday::friday,
        Weekday::saturday,
        Weekday::sunday,
    ]
}

export fn all_courses() -> List<Course> {
    [Course::breakfast, Course::lunch, Course::dinner, Course::dessert, Course::snack]
}
