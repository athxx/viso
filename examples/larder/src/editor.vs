// A recipe of one's own: a form for its title, summary, course and times,
// a list of ingredients and a list of steps to grow and trim, each checked
// as it is typed, and a recipe built from it once nothing is amiss.

import viso::i18n::{tr, MessageKey}
import model::{Recipe, Ingredient, Step, Course, Measure, Aisle, course_key, aisle_key, all_courses, all_aisles}
import kit::{Field, NumberWell, Card, Section, PageHeader}

// An ingredient as the form holds it, before it is one.
export record IngredientDraft {
    key: I64
    name: String = ""
    amount: F64 = 1.0
    unit: U32 = 2
    aisle: U32 = 3
}

// A step as the form holds it.
export record StepDraft {
    key: I64
    text: String = ""
    minutes: F64 = 0.0
}

// What keeps the form from making a recipe.
@derive(Eq)
enum Issue {
    no_title
    long_title
    no_time
    no_ingredient
    no_amount
    no_step
}

fn issue_key(issue: Issue) -> MessageKey {
    match issue {
        Issue::no_title => "editor.no_title"
        Issue::long_title => "editor.long_title"
        Issue::no_time => "editor.no_time"
        Issue::no_ingredient => "editor.no_ingredient"
        Issue::no_amount => "editor.no_amount"
        Issue::no_step => "editor.no_step"
    }
}

fn issues(
    title: String,
    minutes: F64,
    ingredients: List<IngredientDraft>,
    steps: List<StepDraft>,
) -> List<Issue> {
    let mut out: List<Issue> = []
    let name = title.trim()
    if name == "" {
        out.push(Issue::no_title)
    } else if name.len() > 48 {
        out.push(Issue::long_title)
    }
    if minutes < 1.0 { out.push(Issue::no_time) }
    let mut named = 0
    let mut unmeasured = 0
    for draft in ingredients {
        if draft.name.trim() != "" {
            named += 1
            if draft.amount <= 0.0 { unmeasured += 1 }
        }
    }
    if named == 0 { out.push(Issue::no_ingredient) }
    if unmeasured > 0 { out.push(Issue::no_amount) }
    let mut written = 0
    for draft in steps {
        if draft.text.trim() != "" { written += 1 }
    }
    if written == 0 { out.push(Issue::no_step) }
    out
}

fn measures() -> List<Measure> {
    [Measure::gram, Measure::millilitre, Measure::piece, Measure::spoon, Measure::pinch]
}

fn measure_key(unit: Measure) -> MessageKey {
    match unit {
        Measure::gram => "measure.gram"
        Measure::millilitre => "measure.millilitre"
        Measure::piece => "measure.piece"
        Measure::spoon => "measure.spoon"
        Measure::pinch => "measure.pinch"
    }
}

fn measure_at(index: U32) -> Measure {
    match measures().get(index as I64) {
        Option::Some(unit) => unit
        Option::None => Measure::piece
    }
}

fn aisle_at(index: U32) -> Aisle {
    match all_aisles().get(index as I64) {
        Option::Some(aisle) => aisle
        Option::None => Aisle::pantry
    }
}

fn course_at(index: U32) -> Course {
    match all_courses().get(index as I64) {
        Option::Some(course) => course
        Option::None => Course::dinner
    }
}

// The picture a recipe of one's own gets, by its course.
fn picture_of(course: Course) -> SvgAsset {
    match course {
        Course::breakfast => asset("dishes/oats.svg")
        Course::lunch => asset("dishes/salad.svg")
        Course::dinner => asset("dishes/risotto.svg")
        Course::dessert => asset("dishes/mousse.svg")
        Course::snack => asset("dishes/nuts.svg")
    }
}

fn built(
    id: I64,
    title: String,
    summary: String,
    course: Course,
    minutes: F64,
    serves: F64,
    ingredients: List<IngredientDraft>,
    steps: List<StepDraft>,
) -> Recipe {
    let mut items: List<Ingredient> = []
    for draft in ingredients {
        if draft.name.trim() != "" {
            items.push(Ingredient {
                name: draft.name.trim()
                amount: draft.amount
                unit: measure_at(draft.unit)
                aisle: aisle_at(draft.aisle)
            })
        }
    }
    let mut method: List<Step> = []
    for draft in steps {
        if draft.text.trim() != "" {
            method.push(Step { text: draft.text.trim(), minutes: draft.minutes as I64 })
        }
    }
    Recipe {
        id: id
        title: title.trim()
        summary: summary.trim()
        course: course
        minutes: minutes as I64
        serves: serves as I64
        picture: picture_of(course)
        ingredients: items
        steps: method
    }
}

fn replaced_ingredient(drafts: List<IngredientDraft>, draft: IngredientDraft) -> List<IngredientDraft> {
    let mut out: List<IngredientDraft> = []
    for d in drafts {
        if d.key == draft.key { out.push(draft) } else { out.push(d) }
    }
    out
}

fn replaced_step(drafts: List<StepDraft>, draft: StepDraft) -> List<StepDraft> {
    let mut out: List<StepDraft> = []
    for d in drafts {
        if d.key == draft.key { out.push(draft) } else { out.push(d) }
    }
    out
}

// One ingredient of the form: its name, how much of what, the aisle it is
// bought in, and a way to drop it.
component IngredientEditor {
    input draft: IngredientDraft
    input removable: Bool = true
    event edited(draft: IngredientDraft)
    event removed(key: I64)

    view {
        Column {
            width: fill
            gap: 6dp
            padding: EdgeInsets { top: 6dp, bottom: 6dp }
            Row {
                width: fill
                gap: 8dp
                align: Align::center
                TextInput {
                    styles: [Field]
                    width: fill
                    value: draft.name
                    placeholder: tr("editor.ingredient")
                    semantics.label: tr("editor.ingredient")
                    on changed(e) { emit edited(IngredientDraft { name: e.value, ..draft }) }
                }
                NumberField {
                    styles: [NumberWell]
                    width: 88dp
                    value: draft.amount
                    min: Option::Some(0.0)
                    max: Option::Some(5000.0)
                    step: 0.25
                    semantics.label: tr("editor.amount")
                    on changed(e) { emit edited(IngredientDraft { amount: e.value, ..draft }) }
                }
                Button(tr("common.remove")) {
                    enabled: removable
                    on click { emit removed(draft.key) }
                }
            }
            SegmentedControl {
                width: fill
                selected: draft.unit
                semantics.label: tr("editor.measure")
                on selected_changed(e) { emit edited(IngredientDraft { unit: e.value, ..draft }) }
                for unit in measures() key unit {
                    Text(tr(measure_key(unit))) { font_size: 12sp }
                }
            }
            SegmentedControl {
                width: fill
                selected: draft.aisle
                semantics.label: tr("editor.aisle")
                on selected_changed(e) { emit edited(IngredientDraft { aisle: e.value, ..draft }) }
                for aisle in all_aisles() key aisle {
                    Text(tr(aisle_key(aisle))) { font_size: 12sp; max_lines: Option::Some(1) }
                }
            }
        }
    }
}

// One step of the form: what to do, and minutes for a timer if it wants one.
component StepEditor {
    input draft: StepDraft
    input number: I64
    input removable: Bool = true
    event edited(draft: StepDraft)
    event removed(key: I64)

    view {
        Row {
            width: fill
            gap: 8dp
            align: Align::start
            Text("{number}") {
                width: 24dp
                margin: EdgeInsets { top: 6dp }
                color: theme.colors.muted
            }
            TextInput {
                styles: [Field]
                width: fill
                multiline: true
                value: draft.text
                placeholder: tr("editor.step")
                semantics.label: tr("editor.step")
                on changed(e) { emit edited(StepDraft { text: e.value, ..draft }) }
            }
            NumberField {
                styles: [NumberWell]
                width: 72dp
                value: draft.minutes
                min: Option::Some(0.0)
                max: Option::Some(600.0)
                step: 1.0
                semantics.label: tr("editor.step_minutes")
                on changed(e) { emit edited(StepDraft { minutes: e.value, ..draft }) }
            }
            Button(tr("common.remove")) {
                enabled: removable
                on click { emit removed(draft.key) }
            }
        }
    }
}

// The form making a recipe of one's own.
export component RecipeEditor {
    // The id the recipe it makes takes.
    input next_id: I64
    event saved(recipe: Recipe)
    event cancelled()

    state title = ""
    state summary = ""
    state course: U32 = 2
    state minutes: F64 = 30.0
    state serves: F64 = 2.0
    state ingredients: List<IngredientDraft> = [IngredientDraft { key: 1 }]
    state steps: List<StepDraft> = [StepDraft { key: 1 }]
    state next_key = 2
    state tried = false

    computed found: List<Issue> = issues(title, minutes, ingredients, steps)
    computed ready: Bool = found.is_empty()

    action add_ingredient() {
        ingredients.push(IngredientDraft { key: next_key })
        next_key += 1
    }

    action add_step() {
        steps.push(StepDraft { key: next_key })
        next_key += 1
    }

    action save() {
        tried = true
        if !ready { return }
        emit saved(built(
            next_id,
            title,
            summary,
            course_at(course),
            minutes,
            serves,
            ingredients,
            steps,
        ))
        title = ""
        summary = ""
        ingredients = [IngredientDraft { key: next_key }]
        steps = [StepDraft { key: next_key + 1 }]
        next_key += 2
        tried = false
    }

    view {
        Scroll {
            width: fill
            height: fill
            Column {
                width: fill
                max_width: 760dp
                gap: 16dp
                padding: EdgeInsets { bottom: 32dp }
                PageHeader {
                    title: tr("editor.title")
                    subtitle: tr("editor.subtitle")
                    tools {
                        Button(tr("common.cancel")) { on click { emit cancelled() } }
                        Button(tr("editor.save")) {
                            enabled: ready || !tried
                            on click { save() }
                        }
                    }
                }
                if tried && !ready {
                    Card {
                        semantics.live: LiveRegion::polite
                        Text(tr("editor.fix")) { font_weight: FontWeight::semibold; color: theme.colors.error }
                        for issue in found key issue {
                            Text(tr(issue_key(issue))) { color: theme.colors.error; soft_wrap: true; width: fill }
                        }
                    }
                }
                Card {
                    Section {
                        title: tr("editor.about")
                        TextInput {
                            styles: [Field]
                            width: fill
                            placeholder: tr("editor.name")
                            semantics.label: tr("editor.name")
                            max_length: Option::Some(60)
                            bind value <=> title
                        }
                        TextInput {
                            styles: [Field]
                            width: fill
                            multiline: true
                            placeholder: tr("editor.summary")
                            semantics.label: tr("editor.summary")
                            bind value <=> summary
                        }
                        SegmentedControl {
                            width: fill
                            semantics.label: tr("editor.course")
                            bind selected <=> course
                            for c in all_courses() key c {
                                Text(tr(course_key(c)))
                            }
                        }
                        Row {
                            width: fill
                            gap: 16dp
                            align: Align::center
                            Text(tr("editor.minutes")) { width: fill }
                            NumberField {
                                styles: [NumberWell]
                                width: 96dp
                                min: Option::Some(0.0)
                                max: Option::Some(1440.0)
                                step: 5.0
                                semantics.label: tr("editor.minutes")
                                bind value <=> minutes
                            }
                        }
                        Row {
                            width: fill
                            gap: 16dp
                            align: Align::center
                            Text(tr("editor.serves")) { width: fill }
                            Stepper {
                                min: Option::Some(1.0)
                                max: Option::Some(24.0)
                                step: 1.0
                                label: tr("editor.serves")
                                bind value <=> serves
                            }
                        }
                    }
                }
                Card {
                    Section {
                        title: tr("recipe.ingredients")
                        caption: tr("recipe.items", count: ingredients.len())
                        actions {
                            Button(tr("editor.add_ingredient")) { on click { add_ingredient() } }
                        }
                        for draft in ingredients key draft.key {
                            IngredientEditor {
                                draft: draft
                                removable: ingredients.len() > 1
                                on edited({ draft }) { ingredients = replaced_ingredient(ingredients, draft) }
                                on removed(e) { ingredients.retain(|d| d.key != e.key) }
                            }
                        }
                    }
                }
                Card {
                    Section {
                        title: tr("recipe.steps")
                        caption: tr("recipe.step_count", count: steps.len())
                        actions {
                            Button(tr("editor.add_step")) { on click { add_step() } }
                        }
                        for (i, step) in steps.enumerate() key step.key {
                            StepEditor {
                                draft: step
                                number: i + 1
                                removable: steps.len() > 1
                                on edited({ draft }) { steps = replaced_step(steps, draft) }
                                on removed(e) { steps.retain(|d| d.key != e.key) }
                            }
                        }
                    }
                }
            }
        }
    }
}
