// The shopping list: a form to add to it, checked as typed, and what is on
// it grouped by aisle, ticked off as it goes in the basket.

import viso::i18n::{tr, MessageKey}
import model::{ShoppingItem, Aisle, aisle_key, all_aisles, in_aisle, left_to_buy}
import kit::{Field, NumberWell, Card, Section, PageHeader, EmptyState}

@derive(Eq)
enum NameIssue {
    blank
    too_long
    listed
}

fn name_issue(name: String, items: List<ShoppingItem>) -> Option<NameIssue> {
    let trimmed = name.trim()
    if trimmed == "" { return Option::Some(NameIssue::blank) }
    if trimmed.len() > 40 { return Option::Some(NameIssue::too_long) }
    for item in items {
        if !item.bought && item.name == trimmed { return Option::Some(NameIssue::listed) }
    }
    Option::None
}

fn issue_key(issue: NameIssue) -> MessageKey {
    match issue {
        NameIssue::blank => "shopping.blank"
        NameIssue::too_long => "shopping.too_long"
        NameIssue::listed => "shopping.listed"
    }
}

// The form adding one item: its name, aisle and a note.
component AddItem {
    input items: List<ShoppingItem>
    event added(name: String, aisle: Aisle, note: String)

    state name = ""
    state note = ""
    state aisle: U32 = 0
    state touched = false

    computed issue: Option<NameIssue> = name_issue(name, items)
    computed valid: Bool = issue == Option::None

    action submit() {
        touched = true
        if !valid { return }
        emit added(name.trim(), aisle_at(aisle), note.trim())
        name = ""
        note = ""
        touched = false
        field.focus()
    }

    view {
        Card {
            Section {
                title: tr("shopping.add_title")
                Row {
                    width: fill
                    gap: 8dp
                    align: Align::center
                    node field: TextInput {
                        styles: [Field]
                        width: fill
                        placeholder: tr("shopping.name")
                        semantics.label: tr("shopping.name")
                        bind value <=> name
                        on changed({ value }) { touched = value != "" }
                        on submitted { submit() }
                    }
                    Button(tr("shopping.add")) {
                        enabled: valid
                        on click { submit() }
                    }
                }
                match issue {
                    Option::Some(found) if touched => {
                        Text(tr(issue_key(found))) {
                            font_size: 13sp
                            color: theme.colors.error
                            semantics.live: LiveRegion::polite
                        }
                    }
                    _ => {}
                }
                TextInput {
                    styles: [Field]
                    width: fill
                    placeholder: tr("shopping.note")
                    semantics.label: tr("shopping.note")
                    bind value <=> note
                    on submitted { submit() }
                }
                SegmentedControl {
                    width: fill
                    semantics.label: tr("shopping.aisle")
                    bind selected <=> aisle
                    for a in all_aisles() key a {
                        Text(tr(aisle_short(a)))
                    }
                }
            }
        }
    }
}

fn aisle_at(index: U32) -> Aisle {
    match all_aisles().get(index as I64) {
        Option::Some(a) => a
        Option::None => Aisle::pantry
    }
}

fn aisle_short(aisle: Aisle) -> MessageKey {
    match aisle {
        Aisle::produce => "aisle_short.produce"
        Aisle::dairy => "aisle_short.dairy"
        Aisle::bakery => "aisle_short.bakery"
        Aisle::pantry => "aisle_short.pantry"
        Aisle::spices => "aisle_short.spices"
        Aisle::frozen => "aisle_short.frozen"
    }
}

// One thing to buy: ticked once it is in the basket, swiped away when it
// is not wanted.
component ItemRow {
    input item: ShoppingItem
    event toggled(id: I64, bought: Bool)
    event removed(id: I64)

    view {
        SwipeRow {
            width: fill
            full_swipe: FullSwipe::trailing
            on full_swiped { emit removed(item.id) }
            SwipeActions {
                edge: SwipeEdge::trailing
                Button(tr("common.remove")) { on click { emit removed(item.id) } }
            }
            Row {
                width: fill
                gap: 10dp
                padding: EdgeInsets { top: 6dp, bottom: 6dp }
                align: Align::center
                background: theme.colors.surface
                CheckBox {
                    checked: item.bought
                    semantics.label: item.name
                    on changed({ value }) { emit toggled(item.id, value) }
                }
                Column {
                    width: fill
                    gap: 2dp
                    Text(item.name) {
                        color: if item.bought { theme.colors.muted } else { theme.colors.on_surface }
                    }
                    if item.note != "" {
                        Text(item.note) { font_size: 12sp; color: theme.colors.muted }
                    }
                }
            }
        }
    }
}

// One aisle's share of the list, hidden while it has none.
component AisleGroup {
    input aisle: Aisle
    input items: List<ShoppingItem>
    event toggled(id: I64, bought: Bool)
    event removed(id: I64)

    computed left: I64 = left_to_buy(items)

    view {
        Card {
            visible: !items.is_empty()
            Section {
                title: tr(aisle_key(aisle))
                caption: tr("shopping.left", count: left)
                for item in items key item.id {
                    ItemRow {
                        item: item
                        on toggled({ id, bought }) { emit toggled(id, bought) }
                        on removed({ id }) { emit removed(id) }
                    }
                }
            }
        }
    }
}

// The shopping page: the form, then the list by aisle.
export component ShoppingList {
    input items: List<ShoppingItem>
    input wide: Bool = true
    event added(name: String, aisle: Aisle, note: String)
    event toggled(id: I64, bought: Bool)
    event removed(id: I64)
    event cleared()

    computed left: I64 = left_to_buy(items)
    computed bought: I64 = items.len() - left

    view {
        Scroll {
            width: fill
            height: fill
            Column {
                width: fill
                gap: 16dp
                padding: EdgeInsets { bottom: 32dp }
                PageHeader {
                    title: tr("shopping.title")
                    subtitle: tr("shopping.left", count: left)
                    tools {
                        Button(tr("shopping.clear_bought", count: bought)) {
                            enabled: bought > 0
                            on click { emit cleared() }
                        }
                    }
                }
                ProgressBar {
                    width: fill
                    visible: !items.is_empty()
                    value: Option::Some(if items.is_empty() { 0.0 } else { (bought as F32) / (items.len() as F32) })
                    label: tr("shopping.progress")
                }
                AddItem {
                    items: items
                    on added({ name, aisle, note }) { emit added(name, aisle, note) }
                }
                if items.is_empty() {
                    EmptyState {
                        title: tr("shopping.empty_title")
                        message: tr("shopping.empty_message")
                    }
                } else {
                    Grid {
                        width: fill
                        columns: if wide { [Track::fr(1.0), Track::fr(1.0)] } else { [Track::fr(1.0)] }
                        column_gap: 16dp
                        row_gap: 16dp
                        for aisle in all_aisles() key aisle {
                            AisleGroup {
                                aisle: aisle
                                items: in_aisle(items, aisle)
                                on toggled({ id, bought }) { emit toggled(id, bought) }
                                on removed({ id }) { emit removed(id) }
                            }
                        }
                    }
                }
            }
        }
    }
}
