// The week's plan: a grid of days by course, each cell a recipe or room
// for one, and a sheet to pick it from.

import viso::i18n::{tr, MessageKey}
import model::{Recipe, PlannedMeal, Weekday, Course, day_key, course_key, course_color, planned, find_recipe, all_days, all_courses}
import kit::{Card, PageHeader, Tag}

// The courses a week plans: breakfast, lunch and dinner.
fn planned_courses() -> List<Course> {
    [Course::breakfast, Course::lunch, Course::dinner]
}

// One cell of the plan: the recipe planned there, or a button to plan one.
component MealCell {
    input recipe: Option<Recipe>
    input course: Course
    event picked()
    event opened(id: I64)
    event cleared()

    view {
        Column {
            width: fill
            min_height: 64dp
            padding: 8dp
            gap: 4dp
            corner_radius: 10dp
            background: match recipe {
                Option::Some(_) => course_color(course)
                Option::None => theme.colors.background
            }
            border: Border { width: 1dp, color: theme.colors.outline }
            match recipe {
                Option::Some(found) => {
                    Text(found.title) {
                        font_size: 13sp
                        font_weight: FontWeight::semibold
                        color: #ffffff
                        max_lines: Option::Some(2)
                        overflow: TextOverflow::ellipsis
                        soft_wrap: true
                    }
                    Row {
                        gap: 4dp
                        Button(tr("planner.open")) { on click { emit opened(found.id) } }
                        Button(tr("planner.clear")) { on click { emit cleared() } }
                    }
                }
                Option::None => {
                    Button(tr("planner.pick")) {
                        width: fill
                        on click { emit picked() }
                    }
                }
            }
        }
    }
}

// The recipes of one course to choose from, for the cell being planned.
component Picker {
    input recipes: List<Recipe>
    input course: Course
    event chosen(id: I64)

    computed choices: List<Recipe> = of_course(recipes, course)

    view {
        Column {
            width: fill
            gap: 8dp
            Text(tr("planner.choose", course: tr(course_key(course)))) {
                font_size: 18sp
                font_weight: FontWeight::semibold
            }
            for recipe in choices key recipe.id {
                Row {
                    width: fill
                    gap: 10dp
                    padding: 8dp
                    align: Align::center
                    corner_radius: 8dp
                    focusable: true
                    cursor: Cursor::pointer
                    semantics.role: Role::button
                    semantics.label: recipe.title
                    on click { emit chosen(recipe.id) }
                    Stack {
                        width: 36dp
                        height: 36dp
                        corner_radius: 8dp
                        background: course_color(recipe.course)
                        Svg(recipe.picture) { width: fill; height: fill }
                    }
                    Text(recipe.title) { width: fill }
                    Tag { label: tr("recipes.serves", count: recipe.serves) }
                }
            }
        }
    }
}

fn of_course(recipes: List<Recipe>, course: Course) -> List<Recipe> {
    let mut out: List<Recipe> = []
    for recipe in recipes {
        if recipe.course == course { out.push(recipe) }
    }
    out
}

// How many cells of the plan hold a recipe.
fn filled(plan: List<PlannedMeal>) -> I64 {
    plan.len()
}

// The week as a grid: a row a day, a column a course; narrow, a day at a
// time.
export component Planner {
    input recipes: List<Recipe>
    input plan: List<PlannedMeal>
    input wide: Bool = true
    event planned(day: Weekday, course: Course, recipe: I64)
    event unplanned(day: Weekday, course: Course)
    event opened(id: I64)
    event shop_week()

    state picking = false
    state day: Weekday = Weekday::monday
    state course: Course = Course::dinner
    state shown_day: U32 = 0

    action pick(d: Weekday, c: Course) {
        day = d
        course = c
        picking = true
    }

    view {
        Stack {
            width: fill
            height: fill
            Scroll {
                width: fill
                height: fill
                Column {
                    width: fill
                    gap: 16dp
                    padding: EdgeInsets { bottom: 32dp }
                    PageHeader {
                        title: tr("planner.title")
                        subtitle: tr("planner.filled", count: filled(plan), total: 21)
                        tools {
                            Button(tr("planner.shop_week")) {
                                enabled: !plan.is_empty()
                                on click { emit shop_week() }
                            }
                        }
                    }
                    if wide {
                        Card {
                            Grid {
                                width: fill
                                columns: [Track::fixed(110dp), Track::fr(1.0), Track::fr(1.0), Track::fr(1.0)]
                                column_gap: 8dp
                                row_gap: 8dp
                                Text("")
                                for c in planned_courses() key c {
                                    Text(tr(course_key(c))) { font_weight: FontWeight::semibold }
                                }
                                for d in all_days() key d {
                                    DayCells {
                                        recipes: recipes
                                        plan: plan
                                        day: d
                                        on picked(e) { pick(e.day, e.course) }
                                        on opened(e) { emit opened(e.id) }
                                        on cleared(e) { emit unplanned(e.day, e.course) }
                                    }
                                }
                            }
                        }
                    } else {
                        SegmentedControl {
                            width: fill
                            semantics.label: tr("planner.day")
                            bind selected <=> shown_day
                            for d in all_days() key d {
                                Text(tr(day_short(d)))
                            }
                        }
                        Card {
                            for c in planned_courses() key c {
                                Column {
                                    width: fill
                                    gap: 6dp
                                    Text(tr(course_key(c))) { font_weight: FontWeight::semibold }
                                    MealCell {
                                        recipe: meal_at(recipes, plan, day_at(shown_day), c)
                                        course: c
                                        on picked { pick(day_at(shown_day), c) }
                                        on opened(e) { emit opened(e.id) }
                                        on cleared { emit unplanned(day_at(shown_day), c) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Sheet {
                bind open <=> picking
                detents: [60%]
                Column {
                    width: fill
                    padding: 20dp
                    gap: 12dp
                    Text(tr(day_key(day))) { color: theme.colors.muted }
                    Picker {
                        recipes: recipes
                        course: course
                        on chosen(e) {
                            emit planned(day, course, e.id)
                            picking = false
                        }
                    }
                    Button(tr("common.cancel")) { on click { picking = false } }
                }
            }
        }
    }
}

// One day's row of the wide grid: its name, then a cell a course.
component DayCells {
    input recipes: List<Recipe>
    input plan: List<PlannedMeal>
    input day: Weekday
    event picked(day: Weekday, course: Course)
    event opened(id: I64)
    event cleared(day: Weekday, course: Course)

    view {
        Fragment {
            Text(tr(day_key(day))) { font_weight: FontWeight::medium }
            for c in planned_courses() key c {
                MealCell {
                    recipe: meal_at(recipes, plan, day, c)
                    course: c
                    on picked { emit picked(day, c) }
                    on opened(e) { emit opened(e.id) }
                    on cleared { emit cleared(day, c) }
                }
            }
        }
    }
}

fn meal_at(recipes: List<Recipe>, plan: List<PlannedMeal>, day: Weekday, course: Course) -> Option<Recipe> {
    match planned(plan, day, course) {
        Option::Some(id) => find_recipe(recipes, id)
        Option::None => Option::None
    }
}

fn day_at(index: U32) -> Weekday {
    match all_days().get(index as I64) {
        Option::Some(d) => d
        Option::None => Weekday::monday
    }
}

fn day_short(day: Weekday) -> MessageKey {
    match day {
        Weekday::monday => "day_short.monday"
        Weekday::tuesday => "day_short.tuesday"
        Weekday::wednesday => "day_short.wednesday"
        Weekday::thursday => "day_short.thursday"
        Weekday::friday => "day_short.friday"
        Weekday::saturday => "day_short.saturday"
        Weekday::sunday => "day_short.sunday"
    }
}
