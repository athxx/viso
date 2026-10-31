// Settings: how amounts read, how many a recipe opens for, how dense the
// lists are, and what the app knows of the window it runs in.

import viso::i18n::{tr, MessageKey}
import model::{Preferences, UnitSystem}
import kit::{Card, Section, PageHeader, Fact, Divider}

fn class_key(c: SizeClass) -> MessageKey {
    match c {
        SizeClass::Compact => "settings.compact_window"
        SizeClass::Medium => "settings.medium_window"
        SizeClass::Expanded => "settings.expanded_window"
    }
}

// One setting: its name and what it means, its control at the end.
component Setting {
    input title: String
    input detail: String = ""

    view {
        Row {
            width: fill
            gap: 12dp
            align: Align::center
            Column {
                width: fill
                gap: 2dp
                Text(title)
                if detail != "" {
                    Text(detail) {
                        font_size: 12sp
                        color: theme.colors.muted
                        soft_wrap: true
                        width: fill
                    }
                }
            }
            slot
        }
    }
}

export component Settings {
    @bindable(changed)
    input prefs: Preferences
    event changed(value: Preferences)
    event reset_data()

    state measures: U32 = 0
    state servings: F64 = 2.0
    state compact = false
    state sounds = true
    state confirming = false

    effect load when (prefs) {
        transaction {
            measures = if prefs.measures == UnitSystem::imperial { 1 } else { 0 }
            servings = prefs.servings as F64
            compact = prefs.compact_lists
            sounds = prefs.sounds
        }
    }

    action save() {
        emit changed(Preferences {
            measures: if measures == 1 { UnitSystem::imperial } else { UnitSystem::metric }
            servings: servings as I64
            compact_lists: compact
            sounds: sounds
        })
    }

    view {
        Scroll {
            width: fill
            height: fill
            Column {
                width: fill
                max_width: 720dp
                gap: 16dp
                padding: EdgeInsets { bottom: 32dp }
                PageHeader { title: tr("settings.title") }
                Card {
                    Section {
                        title: tr("settings.cooking")
                        Setting {
                            title: tr("settings.measures")
                            detail: tr("settings.measures_detail")
                            SegmentedControl {
                                width: 200dp
                                semantics.label: tr("settings.measures")
                                bind selected <=> measures
                                on selected_changed(e) { save() }
                                Text(tr("settings.metric"))
                                Text(tr("settings.imperial"))
                            }
                        }
                        Divider {}
                        Setting {
                            title: tr("settings.servings")
                            detail: tr("settings.servings_detail")
                            Stepper {
                                min: Option::Some(1.0)
                                max: Option::Some(12.0)
                                step: 1.0
                                label: tr("settings.servings")
                                bind value <=> servings
                                on changed(e) { save() }
                            }
                        }
                    }
                }
                Card {
                    Section {
                        title: tr("settings.display")
                        Setting {
                            title: tr("settings.compact_lists")
                            detail: tr("settings.compact_detail")
                            Toggle {
                                semantics.label: tr("settings.compact_lists")
                                bind checked <=> compact
                                on changed(e) { save() }
                            }
                        }
                        Divider {}
                        Setting {
                            title: tr("settings.sounds")
                            detail: tr("settings.sounds_detail")
                            Toggle {
                                semantics.label: tr("settings.sounds")
                                bind checked <=> sounds
                                on changed(e) { save() }
                            }
                        }
                    }
                }
                Card {
                    Section {
                        title: tr("settings.window")
                        caption: tr("settings.window_detail")
                        Fact { label: tr("settings.size_class"); value: tr(class_key(env.size_class)) }
                        Divider {}
                        Fact {
                            label: tr("settings.orientation")
                            value: if env.orientation == Orientation::Landscape {
                                tr("settings.landscape")
                            } else {
                                tr("settings.portrait")
                            }
                        }
                        Divider {}
                        Fact { label: tr("settings.text_scale"); value: "{env.text_scale}" }
                        Divider {}
                        Fact { label: tr("settings.locale"); value: env.locale.tag }
                    }
                }
                Card {
                    Section {
                        title: tr("settings.data")
                        Disclosure {
                            width: fill
                            bind expanded <=> confirming
                            Text(tr("settings.reset")) { color: theme.colors.error }
                            Column {
                                width: fill
                                gap: 8dp
                                padding: EdgeInsets { top: 8dp }
                                Text(tr("settings.reset_detail")) { soft_wrap: true; width: fill }
                                Row {
                                    gap: 8dp
                                    Button(tr("settings.reset_confirm")) {
                                        on click {
                                            confirming = false
                                            emit reset_data()
                                        }
                                    }
                                    Button(tr("common.cancel")) { on click { confirming = false } }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
