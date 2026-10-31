// The recipes: a searchable list, by course, and a recipe opened over it
// on a navigation stack, scaled to the servings asked for.

import viso::i18n::tr
import model::{Recipe, Ingredient, Step, Course, UnitSystem, course_key, course_color, scaled, unit_text, duration_text, matches, all_courses}
import kit::{Card, Section, PageHeader, Tag, EmptyState, Fact, Divider}

// One recipe as a row of the list: its picture, title, summary and tags.
component RecipeRow {
    input recipe: Recipe
    input compact: Bool = false
    event opened(id: I64)

    view {
        Row {
            width: fill
            gap: 14dp
            padding: if compact { 8dp } else { 12dp }
            align: Align::center
            corner_radius: 12dp
            focusable: true
            cursor: Cursor::pointer
            semantics.role: Role::button
            semantics.label: recipe.title
            on click { emit opened(recipe.id) }
            Stack {
                width: if compact { 48dp } else { 72dp }
                height: if compact { 48dp } else { 72dp }
                corner_radius: 10dp
                background: course_color(recipe.course)
                content_align: Alignment2D::center
                clip: true
                Svg(recipe.picture) { width: fill; height: fill; fit: ImageFit::contain }
            }
            Column {
                width: fill
                gap: 4dp
                Text(recipe.title) { font_size: 16sp; font_weight: FontWeight::semibold }
                if !compact {
                    Text(recipe.summary) {
                        font_size: 13sp
                        color: theme.colors.muted
                        max_lines: Option::Some(2)
                        overflow: TextOverflow::ellipsis
                        soft_wrap: true
                    }
                }
                Row {
                    gap: 6dp
                    align: Align::center
                    Text(duration_text(recipe.minutes)) { font_size: 12sp; color: theme.colors.muted }
                    Text("·") { font_size: 12sp; color: theme.colors.muted }
                    Text(tr("recipes.serves", count: recipe.serves)) {
                        font_size: 12sp
                        color: theme.colors.muted
                    }
                }
            }
        }
    }
}

// The courses as chips: the one picked lit, none for every course.
component CourseFilter {
    input picked: Option<Course>
    event picked_changed(course: Option<Course>)

    view {
        Wrap {
            width: fill
            gap: 8dp
            row_gap: 8dp
            Chip {
                label: tr("recipes.all")
                selected: picked == Option::None
                on changed { emit picked_changed(Option::None) }
            }
            for course in all_courses() key course {
                Chip {
                    label: tr(course_key(course))
                    selected: picked == Option::Some(course)
                    on changed(e) {
                        if e.value { emit picked_changed(Option::Some(course)) } else {
                            emit picked_changed(Option::None)
                        }
                    }
                }
            }
        }
    }
}

// The list of recipes, the search over it and the course filter.
export component RecipeList {
    input recipes: List<Recipe>
    input loading: Bool = false
    input failure: String = ""
    input compact: Bool = false
    event opened(id: I64)
    event retried()
    event writing()

    state query = ""
    state course: Option<Course> = Option::None

    computed shown: List<Recipe> = filtered(recipes, query, course)

    view {
        Column {
            width: fill
            height: fill
            gap: 16dp
            PageHeader {
                title: tr("recipes.title")
                subtitle: tr("recipes.count", count: shown.len())
                tools {
                    Button(tr("recipes.new")) { on click { emit writing() } }
                    SearchField {
                        width: 240dp
                        placeholder: tr("recipes.search")
                        label: tr("recipes.search")
                        bind value <=> query
                    }
                }
            }
            CourseFilter {
                picked: course
                on picked_changed(e) { course = e.course }
            }
            if loading {
                Column {
                    width: fill
                    padding: 32dp
                    gap: 12dp
                    align: Align::center
                    Spinner { label: tr("recipes.loading") }
                    Text(tr("recipes.loading")) { color: theme.colors.muted }
                }
            } else if failure != "" {
                EmptyState {
                    title: tr("recipes.failed")
                    message: failure
                    Button(tr("recipes.retry")) { on click { emit retried() } }
                }
            } else if shown.is_empty() {
                EmptyState {
                    title: tr("recipes.empty_title")
                    message: tr("recipes.empty_message")
                    Button(tr("recipes.clear_search")) {
                        on click {
                            query = ""
                            course = Option::None
                        }
                    }
                }
            } else {
                VirtualList {
                    width: fill
                    height: fill
                    estimated_extent: 96dp
                    overscan: 4
                    semantics.label: tr("recipes.title")
                    for recipe in shown key recipe.id {
                        RecipeRow {
                            recipe: recipe
                            compact: compact
                            on opened({ id }) { emit opened(id) }
                        }
                    }
                }
            }
        }
    }
}

fn filtered(recipes: List<Recipe>, query: String, course: Option<Course>) -> List<Recipe> {
    let mut out: List<Recipe> = []
    for recipe in recipes {
        if matches(recipe, query, course) { out.push(recipe) }
    }
    out
}

// One ingredient, scaled, with a box to tick it off while cooking.
component IngredientRow {
    input ingredient: Ingredient
    input base: I64
    input serves: I64
    input measures: UnitSystem
    state have = false

    view {
        Row {
            width: fill
            gap: 10dp
            align: Align::center
            CheckBox {
                semantics.label: ingredient.name
                bind checked <=> have
            }
            Text(ingredient.name) {
                width: fill
                color: if have { theme.colors.muted } else { theme.colors.on_surface }
            }
            Text(unit_text(ingredient.unit, scaled(ingredient.amount, base, serves), measures)) {
                font_weight: FontWeight::medium
                color: if have { theme.colors.muted } else { theme.colors.on_surface }
            }
        }
    }
}

// One step of the method: its number, its text, and a timer when it is
// timed.
component StepRow {
    input step: Step
    event timed(label: String, minutes: I64)

    view {
        Row {
            width: fill
            gap: 12dp
            Stack {
                width: 28dp
                height: 28dp
                corner_radius: 14dp
                background: theme.colors.primary
                content_align: Alignment2D::center
                Text("{step.index + 1}") { color: theme.colors.on_primary; font_size: 13sp }
            }
            Column {
                width: fill
                gap: 6dp
                Text(step.text) { soft_wrap: true; width: fill }
                if step.minutes > 0 {
                    Button(tr("recipe.start_timer", minutes: step.minutes)) {
                        on click { emit timed(step.text, step.minutes) }
                    }
                }
            }
        }
    }
}

// A recipe opened: its picture and facts, the servings it is scaled to,
// what goes in, and the method.
export component RecipeDetail {
    input recipe: Recipe
    input measures: UnitSystem = UnitSystem::metric
    input servings: I64 = 2
    input wide: Bool = true
    event closed()
    event shop(items: List<Ingredient>)
    event timer(label: String, minutes: I64)
    event planned(id: I64)

    state serves: F64 = 2.0
    state started = false

    computed people: I64 = serves as I64

    // A recipe opened anew is scaled to the servings the settings ask for.
    effect sync when (recipe.id) {
        transaction { serves = untracked(servings) as F64 }
    }

    view {
        Scroll {
            width: fill
            height: fill
            Column {
                width: fill
                gap: 20dp
                padding: EdgeInsets { bottom: 32dp }
                Row {
                    width: fill
                    gap: 8dp
                    align: Align::center
                    Button(tr("recipe.back")) { on click { emit closed() } }
                    Row { width: fill }
                    Button(tr("recipe.plan")) { on click { emit planned(recipe.id) } }
                }
                Stack {
                    width: fill
                    height: 220dp
                    corner_radius: 18dp
                    clip: true
                    background: [
                        course_color(recipe.course),
                        Fill::linear { colors: [#ffffff33, #00000026] },
                    ]
                    content_align: Alignment2D::center
                    Svg(recipe.picture) { width: 200dp; height: 200dp; fit: ImageFit::contain }
                }
                Column {
                    width: fill
                    gap: 8dp
                    Text(recipe.title) {
                        font_size: 28sp
                        font_weight: FontWeight::bold
                        semantics.role: Role::heading
                        semantics.heading_level: 1
                    }
                    Text(recipe.summary) { soft_wrap: true; width: fill; color: theme.colors.muted }
                    Wrap {
                        width: fill
                        gap: 6dp
                        row_gap: 6dp
                        Tag { label: tr(course_key(recipe.course)) }
                        for tag in recipe.tags key tag {
                            Tag { label: tag }
                        }
                    }
                }
                if wide {
                    Row {
                        width: fill
                        gap: 20dp
                        align: Align::start
                        Column {
                            width: 320dp
                            gap: 20dp
                            Facts { recipe: recipe; people: people }
                            Ingredients {
                                recipe: recipe
                                people: people
                                measures: measures
                                bind serves <=> serves
                                on shop({ items }) { emit shop(items) }
                            }
                        }
                        Method {
                            recipe: recipe
                            on timer({ label, minutes }) { emit timer(label, minutes) }
                        }
                    }
                } else {
                    Facts { recipe: recipe; people: people }
                    Ingredients {
                        recipe: recipe
                        people: people
                        measures: measures
                        bind serves <=> serves
                        on shop({ items }) { emit shop(items) }
                    }
                    Method {
                        recipe: recipe
                        on timer({ label, minutes }) { emit timer(label, minutes) }
                    }
                }
            }
        }
    }
}

// How long a recipe takes and how many it feeds, as asked.
component Facts {
    input recipe: Recipe
    input people: I64

    view {
        Card {
            Fact { label: tr("recipe.time"); value: duration_text(recipe.minutes) }
            Divider {}
            Fact { label: tr("recipe.written_for"); value: tr("recipes.serves", count: recipe.serves) }
            Divider {}
            Fact { label: tr("recipe.cooking_for"); value: tr("recipes.serves", count: people) }
        }
    }
}

// What goes into a recipe for `people`, and the servings stepper.
component Ingredients {
    input recipe: Recipe
    input people: I64
    input measures: UnitSystem
    @bindable(serves_changed)
    input serves: F64
    event serves_changed(value: F64)
    event shop(items: List<Ingredient>)

    view {
        Card {
            Section {
                title: tr("recipe.ingredients")
                caption: tr("recipe.items", count: recipe.ingredients.len())
                actions {
                    Stepper {
                        min: Option::Some(1.0)
                        max: Option::Some(24.0)
                        step: 1.0
                        value: serves
                        label: tr("recipe.servings")
                        on changed({ value }) { emit serves_changed(value) }
                    }
                }
                for ingredient in recipe.ingredients key ingredient.name {
                    IngredientRow {
                        ingredient: ingredient
                        base: recipe.serves
                        serves: people
                        measures: measures
                    }
                }
                Button(tr("recipe.add_all")) {
                    width: fill
                    on click { emit shop(scaled_all(recipe.ingredients, recipe.serves, people)) }
                }
            }
        }
    }
}

fn scaled_all(items: List<Ingredient>, base: I64, serves: I64) -> List<Ingredient> {
    let mut out: List<Ingredient> = []
    for item in items {
        out.push(Ingredient { amount: scaled(item.amount, base, serves), ..item })
    }
    out
}

// The steps, each told its place.
fn numbered(steps: List<Step>) -> List<Step> {
    let mut out: List<Step> = []
    let mut i = 0
    for step in steps {
        out.push(Step { index: i, ..step })
        i += 1
    }
    out
}

// The method, a step at a time.
component Method {
    input recipe: Recipe
    event timer(label: String, minutes: I64)

    view {
        Card {
            Section {
                title: tr("recipe.steps")
                caption: tr("recipe.step_count", count: recipe.steps.len())
                for step in numbered(recipe.steps) key step.index {
                    StepRow {
                        step: step
                        on timed({ label, minutes }) { emit timer(label, minutes) }
                    }
                }
            }
        }
    }
}
