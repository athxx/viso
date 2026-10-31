// Larder's shell: what the kitchen holds, and the page showing it. On a
// wide window the sections are a sidebar beside the page; on a narrow one,
// a tab bar under it. The recipe box loads as a resource; a recipe opens
// over its list on a navigation stack; the shopping list, the plan and the
// settings outlive the session.

import viso::i18n::tr
import viso::time
import model::{Recipe, Ingredient, ShoppingItem, PlannedMeal, KitchenTimer, Preferences, Section, Course, Weekday, Aisle, section_key, find_recipe, left_to_buy}
import data::{catalog, starter_shopping, starter_plan}
import recipes::{RecipeList, RecipeDetail}
import editor::{RecipeEditor}
import shopping::{ShoppingList}
import planner::{Planner}
import timers::{Timers}
import settings::{Settings}
import kit::{Notice}

fn sections() -> List<Section> {
    [Section::recipes, Section::shopping, Section::planner, Section::timers, Section::settings]
}

fn icon(section: Section) -> SvgAsset {
    match section {
        Section::recipes => asset("icons/book.svg")
        Section::shopping => asset("icons/basket.svg")
        Section::planner => asset("icons/calendar.svg")
        Section::timers => asset("icons/timer.svg")
        Section::settings => asset("icons/gear.svg")
    }
}

// One section in the sidebar or the tab bar, lit while it shows.
component NavItem {
    input section: Section
    input current: Bool
    input count: I64 = 0
    input stacked: Bool = false
    event chosen(section: Section)

    view {
        Flex {
            axis: if stacked { Axis::column } else { Axis::row }
            width: fill
            gap: if stacked { 2dp } else { 10dp }
            padding: if stacked { 6dp } else { 10dp }
            align: Align::center
            justify: if stacked { Justify::center } else { Justify::start }
            corner_radius: 10dp
            background: if current { theme.colors.primary } else { #00000000 }
            focusable: true
            cursor: Cursor::pointer
            semantics.role: Role::tab
            semantics.label: tr(section_key(section))
            on click { emit chosen(section) }
            Svg(icon(section)) {
                width: 20dp
                height: 20dp
                tint: Option::Some(if current { theme.colors.on_primary } else { theme.colors.on_surface })
            }
            Text(tr(section_key(section))) {
                width: if stacked { Sizing::fit } else { Sizing::fill }
                font_size: if stacked { 11sp } else { 14sp }
                color: if current { theme.colors.on_primary } else { theme.colors.on_surface }
            }
            if !stacked && count > 0 {
                Badge { count: Option::Some(count as I32) }
            }
        }
    }
}

export component Larder {
    state section: Section = Section::recipes
    state page: U32 = 0
    state opened: I64 = 0
    state attempt = 0

    @persist("larder.shopping")
    state shopping: List<ShoppingItem> = starter_shopping()
    @persist("larder.next_item")
    state next_item = 100
    @persist("larder.plan")
    state plan: List<PlannedMeal> = starter_plan()
    @persist("larder.prefs")
    state prefs: Preferences = Preferences {}
    @persist("larder.mine")
    state mine: List<Recipe> = []

    state timers: List<KitchenTimer> = []
    state next_timer = 1
    state notice = ""
    state showing = false

    task fetch(attempt: I64) -> Result<List<Recipe>, String> {
        // The recipe box is at hand; fetching it again after a failure
        // takes a moment.
        if attempt > 0 { await time::sleep(400ms) }
        Ok(catalog())
    }

    resource box: Resource<List<Recipe>, String> {
        load = fetch(attempt)
        key = attempt
        policy = [ResourcePolicy::keep_latest]
    }

    computed recipes: List<Recipe> = match box.state {
        ResourceState::ready(found) => joined(mine, found)
        _ => []
    }
    computed loading: Bool = match box.state {
        ResourceState::ready(_) => false
        ResourceState::error(_) => false
        _ => true
    }
    computed failure: String = match box.state {
        ResourceState::error(message) => message
        _ => ""
    }
    computed left: I64 = left_to_buy(shopping)
    computed running: I64 = running_timers(timers)

    action show(message: String) {
        notice = message
        showing = true
    }

    action keep(recipe: Recipe) {
        mine.push(recipe)
        show(tr("notice.saved", title: recipe.title))
        open(recipe.id)
    }

    action open(id: I64) {
        opened = id
        section = Section::recipes
        page = 1
    }

    action shop(items: List<Ingredient>) {
        let mut added = 0
        for item in items {
            if !listed(shopping, item.name) {
                shopping.push(ShoppingItem { id: next_item, name: item.name, aisle: item.aisle })
                next_item += 1
                added += 1
            }
        }
        show(tr("notice.added", count: added))
    }

    action add_item(name: String, aisle: Aisle, note: String) {
        shopping.push(ShoppingItem { id: next_item, name: name, aisle: aisle, note: note })
        next_item += 1
    }

    action mark(id: I64, bought: Bool) {
        let mut out: List<ShoppingItem> = []
        for item in shopping {
            if item.id == id { out.push(ShoppingItem { bought: bought, ..item }) } else { out.push(item) }
        }
        shopping = out
    }

    action drop_item(id: I64) {
        shopping.retain(|item| item.id != id)
    }

    action clear_bought() {
        shopping.retain(|item| !item.bought)
    }

    action plan_meal(day: Weekday, course: Course, recipe: I64) {
        plan.retain(|meal| !(meal.day == day && meal.course == course))
        plan.push(PlannedMeal { day: day, course: course, recipe: recipe })
    }

    action unplan(day: Weekday, course: Course) {
        plan.retain(|meal| !(meal.day == day && meal.course == course))
    }

    action plan_tonight(id: I64) {
        plan_meal(Weekday::friday, Course::dinner, id)
        show(tr("notice.planned"))
    }

    action shop_week() {
        let mut items: List<Ingredient> = []
        for meal in plan {
            match find_recipe(recipes, meal.recipe) {
                Option::Some(recipe) => {
                    for item in recipe.ingredients { items.push(item) }
                }
                Option::None => {}
            }
        }
        shop(items)
    }

    action add_timer(label: String, minutes: I64) {
        timers.push(KitchenTimer { id: next_timer, label: label, total: minutes * 60, left: minutes * 60 })
        next_timer += 1
    }

    action timer_from_step(label: String, minutes: I64) {
        add_timer(label, minutes)
        show(tr("notice.timer", minutes: minutes))
    }

    action tick() {
        let mut out: List<KitchenTimer> = []
        let mut rang = false
        for timer in timers {
            if timer.running && timer.left > 0 {
                let left = timer.left - 1
                if left == 0 { rang = true }
                out.push(KitchenTimer { left: left, ..timer })
            } else {
                out.push(timer)
            }
        }
        timers = out
        if rang { show(tr("notice.rang")) }
    }

    action set_running(id: I64, running: Bool) {
        let mut out: List<KitchenTimer> = []
        for timer in timers {
            if timer.id == id { out.push(KitchenTimer { running: running, ..timer }) } else { out.push(timer) }
        }
        timers = out
    }

    action restart(id: I64) {
        let mut out: List<KitchenTimer> = []
        for timer in timers {
            if timer.id == id {
                out.push(KitchenTimer { left: timer.total, running: true, ..timer })
            } else {
                out.push(timer)
            }
        }
        timers = out
    }

    action drop_timer(id: I64) {
        timers.retain(|timer| timer.id != id)
    }

    action reset_data() {
        mine = []
        shopping = starter_shopping()
        plan = starter_plan()
        prefs = Preferences {}
        show(tr("notice.reset"))
    }

    view {
        Stack {
            width: fill
            height: fill
            background: theme.colors.background
            Flex {
                axis: if wide(env.size_class) { Axis::row } else { Axis::column }
                width: fill
                height: fill
                Column {
                    visible: wide(env.size_class)
                    width: 232dp
                    height: fill
                    padding: EdgeInsets { start: 16dp, end: 16dp, top: 28dp, bottom: 16dp }
                    gap: 4dp
                    background: theme.colors.surface
                    border: Border { width: 1dp, color: theme.colors.outline }
                    semantics.role: Role::navigation
                    Text(tr("app.name")) { font_size: 22sp; font_weight: FontWeight::bold }
                    Text(tr("app.tagline")) {
                        font_size: 12sp
                        color: theme.colors.muted
                        soft_wrap: true
                        width: fill
                        margin: EdgeInsets { bottom: 16dp }
                    }
                    for s in sections() key s {
                        NavItem {
                            section: s
                            current: section == s
                            count: badge(s, left, running)
                            on chosen(e) { section = e.section }
                        }
                    }
                }
                Column {
                    width: fill
                    height: fill
                    padding: if wide(env.size_class) {
                        EdgeInsets { start: 28dp, end: 28dp, top: 20dp }
                    } else {
                        EdgeInsets { start: 16dp, end: 16dp, top: 12dp }
                    }
                    match section {
                        Section::recipes => {
                            NavigationStack {
                                width: fill
                                height: fill
                                bind current <=> page
                                RecipeList {
                                    recipes: recipes
                                    loading: loading
                                    failure: failure
                                    compact: prefs.compact_lists
                                    on opened(e) { open(e.id) }
                                    on retried { attempt += 1 }
                                    on writing { page = 2 }
                                }
                                match find_recipe(recipes, opened) {
                                    Option::Some(recipe) => {
                                        RecipeDetail {
                                            recipe: recipe
                                            measures: prefs.measures
                                            servings: prefs.servings
                                            wide: wide(env.size_class)
                                            on closed { page = 0 }
                                            on shop(e) { shop(e.items) }
                                            on timer(e) { timer_from_step(e.label, e.minutes) }
                                            on planned(e) { plan_tonight(e.id) }
                                        }
                                    }
                                    Option::None => {
                                        Column {}
                                    }
                                }
                                RecipeEditor {
                                    next_id: 1000 + mine.len()
                                    on saved(e) { keep(e.recipe) }
                                    on cancelled { page = 0 }
                                }
                            }
                        }
                        Section::shopping => {
                            ShoppingList {
                                items: shopping
                                wide: wide(env.size_class)
                                on added(e) { add_item(e.name, e.aisle, e.note) }
                                on toggled(e) { mark(e.id, e.bought) }
                                on removed(e) { drop_item(e.id) }
                                on cleared { clear_bought() }
                            }
                        }
                        Section::planner => {
                            Planner {
                                recipes: recipes
                                plan: plan
                                wide: wide(env.size_class)
                                on planned(e) { plan_meal(e.day, e.course, e.recipe) }
                                on unplanned(e) { unplan(e.day, e.course) }
                                on opened(e) { open(e.id) }
                                on shop_week { shop_week() }
                            }
                        }
                        Section::timers => {
                            Timers {
                                timers: timers
                                wide: wide(env.size_class)
                                on created(e) { add_timer(e.label, e.minutes) }
                                on ticked { tick() }
                                on paused(e) { set_running(e.id, false) }
                                on resumed(e) { set_running(e.id, true) }
                                on reset(e) { restart(e.id) }
                                on removed(e) { drop_timer(e.id) }
                            }
                        }
                        Section::settings => {
                            Settings {
                                bind prefs <=> prefs
                                on reset_data { reset_data() }
                            }
                        }
                    }
                }
                Row {
                    visible: !wide(env.size_class)
                    width: fill
                    padding: 6dp
                    gap: 4dp
                    background: theme.colors.surface
                    border: Border { width: 1dp, color: theme.colors.outline }
                    semantics.role: Role::tab_list
                    for s in sections() key s {
                        NavItem {
                            section: s
                            current: section == s
                            stacked: true
                            on chosen(e) { section = e.section }
                        }
                    }
                }
            }
            Toast {
                open: showing
                Notice {
                    message: notice
                    on dismissed { showing = false }
                }
            }
        }
    }
}

// Whether a window of class `c` has room for a sidebar beside the page.
fn wide(c: SizeClass) -> Bool {
    c == SizeClass::Expanded
}

fn listed(items: List<ShoppingItem>, name: String) -> Bool {
    for item in items {
        if !item.bought && item.name == name { return true }
    }
    false
}

fn running_timers(timers: List<KitchenTimer>) -> I64 {
    let mut n = 0
    for timer in timers {
        if timer.running && timer.left > 0 { n += 1 }
    }
    n
}

fn badge(section: Section, left: I64, running: I64) -> I64 {
    match section {
        Section::shopping => left
        Section::timers => running
        _ => 0
    }
}

// One's own recipes first, then the box's.
fn joined(first: List<Recipe>, then: List<Recipe>) -> List<Recipe> {
    let mut out = first
    for recipe in then { out.push(recipe) }
    out
}
