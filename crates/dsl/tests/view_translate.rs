//! `tr` and `MessageKey` against a package's message catalog: the catalog's
//! own problems, the `E3706` checks of keys and arguments, the contexts `tr`
//! reads `env.locale` in, and at run time plurals, ordinals, selects and
//! numbers in the reader's locale, the CLDR fallback to the nearest
//! translated locale, a locale switch re-showing every translated label, and
//! the same under hot reload and in the release package.

use std::cell::RefCell;
use std::rc::Rc;

use viso_dsl::aot::build_view_package_for;
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::TargetProfile;
use viso_dsl::hotreload::{CandidatePlan, LiveRuntime, plan_view_for, transact};
use viso_dsl::i18n::{ArgKind, CatalogFile, Messages};
use viso_dsl::schema::Natives;
use viso_ui::adaptive::Environment;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, Rect, SemanticProjector, StateStore, TextEdits, run_structure_hooks,
};
use viso_view::{ViewHost, load_view_with};

const EN: &str = r#"
[inbox]
title = "Inbox"
unread = "{count, plural, =0 {No unread messages} one {# unread message} other {# unread messages}}"
place = "{n, selectordinal, one {#st} two {#nd} few {#rd} other {#th}}"
greet = "Hello, {name}!"
mood = "{mood, select, happy {Glad} other {Meh}}"
other = "Other"
size = "{bytes} bytes, {ratio}"
"#;

const AR: &str = r#"
inbox.unread = "{count, plural, zero {لا رسائل} one {رسالة واحدة} two {رسالتان} few {# رسائل} many {# رسالة} other {# رسالة}}"
"#;

const DE: &str = r#"
[inbox]
title = "Posteingang"
unread = "{count, plural, one {# ungelesene Nachricht} other {# ungelesene Nachrichten}}"
"#;

const ZH_HANT: &str = r#"
inbox = { title = "收件匣" }
"#;

fn file(locale: &str, text: &str) -> CatalogFile {
    CatalogFile {
        locale: locale.into(),
        path: format!("i18n/{locale}.toml"),
        text: text.into(),
    }
}

fn messages() -> Messages {
    Messages::compile(
        "en",
        &[
            file("ar", AR),
            file("de", DE),
            file("en", EN),
            file("zh-Hant", ZH_HANT),
        ],
    )
}

fn profile() -> TargetProfile {
    TargetProfile {
        messages: Some(Rc::new(messages())),
        ..TargetProfile::default()
    }
}

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["inbox".into()],
        language: None,
    }
}

fn codes(source: &str, profile: TargetProfile) -> Vec<(String, String)> {
    compile_file_for(source, &origin(), Natives::standard(), profile)
        .errors()
        .map(|d| (d.code.to_string(), d.message.clone()))
        .collect()
}

fn component(members: &str, view: &str) -> String {
    format!(
        "import viso::i18n::{{tr, MessageKey}};\n\
         enum Mood {{ happy; sad; }}\n\
         record Tag {{ n: I64 = 0; }}\n\
         component Inbox {{\n\
             state unread = 0;\n\
             state mood = Mood::happy;\n\
             state key: MessageKey = \"inbox.title\";\n\
             {members}\n\
             view {{ Column {{ {view} }} }}\n\
         }}\n"
    )
}

fn view_codes(view: &str) -> Vec<(String, String)> {
    codes(&component("", view), profile())
}

#[test]
fn the_catalog_compiles_its_messages_and_each_translation_fits_its_source() {
    let messages = messages();
    assert!(messages.issues().is_empty(), "{:?}", messages.issues());
    assert_eq!(messages.source(), "en");
    let (_, unread) = messages.message("inbox.unread").expect("a message");
    assert_eq!(unread.args, [("count".to_owned(), ArgKind::Number)]);
    let (_, size) = messages.message("inbox.size").expect("a message");
    assert_eq!(
        size.args,
        [
            ("bytes".to_owned(), ArgKind::Text),
            ("ratio".to_owned(), ArgKind::Text)
        ]
    );
    let locales: Vec<&str> = messages.catalog().locales().collect();
    assert_eq!(locales, ["en", "ar", "de", "zh-Hant"], "the source first");
    let untranslated = messages.untranslated();
    let zh = untranslated
        .iter()
        .find(|(l, _)| l == "zh-Hant")
        .expect("zh");
    assert!(zh.1.contains(&"inbox.unread".to_owned()));

    let issues = |files: &[CatalogFile]| -> Vec<(bool, String)> {
        Messages::compile("en", files)
            .issues()
            .iter()
            .map(|i| (i.error, i.message.clone()))
            .collect()
    };
    let found = issues(&[
        file("en", EN),
        file(
            "fr",
            "inbox.unread = \"{total, plural, other {#}}\"\ninbox.gone = \"x\"",
        ),
    ]);
    assert!(
        found
            .iter()
            .any(|(e, m)| *e && m.contains("`total`, which the source message does not take")),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|(e, m)| !*e && m.contains("`inbox.gone` is not a message")),
        "a stray key only warns: {found:?}"
    );
    let found = issues(&[file(
        "en",
        "a = \"{n, plural, one {x}}\"\nb = \"{n} {n, select, x {y} other {z}}\" c = 1",
    )]);
    assert!(
        !found.is_empty() && found.iter().all(|(e, _)| *e),
        "{found:?}"
    );
    let found = issues(&[file("en", "a = \"{n, plural, one {x}}\"")]);
    assert!(found[0].1.contains("`other`"), "{found:?}");
    let found = issues(&[file(
        "en",
        "a = \"{n, number} {n, select, x {y} other {z}}\"",
    )]);
    assert!(
        found[0].1.contains("both as a number and as a `select`"),
        "{found:?}"
    );
    let found = issues(&[file("de", DE)]);
    assert!(
        found[0].1.contains("no catalog of the source locale `en`"),
        "{found:?}"
    );
    let found = issues(&[file("en", EN), file("en-x-bad!", "")]);
    assert!(found[0].1.contains("no BCP 47 locale"), "{found:?}");
}

#[test]
fn a_key_and_its_arguments_match_the_catalog() {
    let ok = view_codes(
        "Text { text: tr(\"inbox.unread\", count: unread); }\
         Text { text: tr(\"inbox.title\"); }\
         Text { text: tr(key); }\
         Text { text: tr(\"inbox.mood\", mood: mood); }\
         Text { text: tr(\"inbox.mood\", mood: \"happy\"); }\
         Text { text: tr(\"inbox.size\", bytes: 12, ratio: 1.5dp); }\
         Text { text: tr(\"inbox.greet\", name: \"Ann\"); }",
    );
    assert!(ok.is_empty(), "{ok:?}");

    let e3706 = |view: &str, needle: &str| {
        let found = view_codes(view);
        assert!(
            found
                .iter()
                .any(|(c, m)| c == "E3706" && m.contains(needle)),
            "{view}: {found:?}"
        );
    };
    e3706(
        "Text { text: tr(\"inbox.titel\"); }",
        "not a message of the `en` catalog",
    );
    e3706("Text { text: tr(\"inbox.unread\"); }", "needs `count:`");
    e3706(
        "Text { text: tr(\"inbox.title\", n: 1); }",
        "takes no arguments",
    );
    e3706(
        "Text { text: tr(\"inbox.unread\", cuont: 1); }",
        "no placeholder `cuont`",
    );
    e3706(
        "Text { text: tr(\"inbox.unread\", count: 1, count: 2); }",
        "given twice",
    );
    e3706(
        "Text { text: tr(\"inbox.unread\", count: \"x\"); }",
        "as a number, found `String`",
    );
    e3706(
        "Text { text: tr(\"inbox.mood\", mood: 1); }",
        "as a `String` or an enum",
    );
    e3706(
        "Text { text: tr(\"inbox.greet\", name: Tag {}); }",
        "found `Tag`",
    );
    e3706("Text { text: tr(key, count: 1); }", "without arguments");
    e3706(
        "Text { text: tr(\"inbox.unread\", 1); }",
        "named after the message's placeholder",
    );

    let unknown = compile_file_for(
        &component("", "Text { text: tr(\"inbox.titel\"); }"),
        &origin(),
        Natives::standard(),
        profile(),
    );
    let d = unknown
        .diagnostics
        .iter()
        .find(|d| d.code == "E3706")
        .expect("E3706");
    assert!(d.notes.iter().any(|n| n.contains("`inbox.title`")), "{d:?}");
    assert_eq!(d.fixes[0].edits[0].replacement, "inbox.title");

    let held = codes(
        &component("state other: MessageKey = \"inbox.unread\";", ""),
        profile(),
    );
    assert!(
        held.iter()
            .any(|(c, m)| c == "E3706" && m.contains("cannot carry")),
        "{held:?}"
    );
    let held = codes(
        &component("state other: MessageKey = \"nope\";", ""),
        profile(),
    );
    assert!(held.iter().any(|(c, _)| c == "E3706"), "{held:?}");
    let dynamic = codes(
        &component("fn pick(name: String) -> MessageKey { return name; }", ""),
        profile(),
    );
    assert!(dynamic.iter().any(|(c, _)| c == "E2103"), "{dynamic:?}");

    let none = codes(
        &component("", "Text { text: tr(\"inbox.title\"); }"),
        TargetProfile::default(),
    );
    assert!(
        none.iter()
            .any(|(c, m)| c == "E3706" && m.contains("has none")),
        "{none:?}"
    );
}

#[test]
fn tr_reads_the_locale_only_where_a_component_instance_runs() {
    let ok = codes(
        &component(
            "computed heading = tr(\"inbox.title\");\n\
             state shown = \"\";\n\
             action show() { shown = tr(\"inbox.unread\", count: unread); }\n\
             fn pick(n: I64) -> MessageKey { if n > 0 { return \"inbox.title\"; } return \"inbox.title\"; }\n\
             fn of(m: Mood) -> MessageKey { return match m { Mood::happy => \"inbox.title\", Mood::sad => \"inbox.other\" }; }\n\
             computed same = key == of(mood);",
            "Text { text: heading; on click { show(); } }",
        ),
        profile(),
    );
    assert!(ok.is_empty(), "{ok:?}");
    let in_fn = codes(
        &component("fn label() -> String { return tr(\"inbox.title\"); }", ""),
        profile(),
    );
    assert!(in_fn.iter().any(|(c, _)| c == "E2111"), "{in_fn:?}");
    let in_init = codes(&component("state t = tr(\"inbox.title\");", ""), profile());
    assert!(in_init.iter().any(|(c, _)| c == "E2111"), "{in_init:?}");
    let module_fn = codes(
        "import viso::i18n::tr;\nfn label() -> String { return tr(\"inbox.title\"); }\n\
         component A { view { Column {} } }",
        profile(),
    );
    assert!(module_fn.iter().any(|(c, _)| c == "E2111"), "{module_fn:?}");
}

const SOURCE: &str = r#"
import viso::i18n::{tr, MessageKey};

enum Mood { happy; sad; }

component Inbox {
    state unread = 0;
    state mood = Mood::happy;
    state key: MessageKey = "inbox.title";
    computed heading = tr("inbox.title");
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Text {
                width: 20dp;
                height: 20dp;
                text: tr("inbox.unread", count: unread);
                on click { unread += 1; }
            }
            Text {
                width: 20dp;
                height: 20dp;
                text: tr(key);
                on click { key = "inbox.other"; }
            }
            Text {
                width: 20dp;
                height: 20dp;
                text: tr("inbox.mood", mood: mood);
                on click { mood = Mood::sad; }
            }
            Text { width: 20dp; height: 20dp; text: tr("inbox.place", n: unread + 2); }
            Text { width: 20dp; height: 20dp; text: tr("inbox.greet", name: "Ann"); }
            Text { width: 20dp; height: 20dp; text: tr("inbox.size", bytes: 1234567, ratio: 0.5); }
            Text { width: 20dp; height: 20dp; text: heading; }
        }
    }
}
"#;

/// A mounted view and the stores it runs over.
#[derive(Default)]
struct Rt {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    text_edits: TextEdits,
    projectors: SemanticProjector,
    root: Option<NodeId>,
    scratch: Vec<NodeId>,
    nodes: Vec<Option<NodeId>>,
    view: Option<Rc<RefCell<ViewHost>>>,
    last_good: CandidatePlan,
    shown: Vec<(NodeId, String)>,
}

impl Rt {
    fn reloaded(source: &str) -> Self {
        let mut rt = Rt::default();
        rt.reload(source, profile());
        rt
    }

    fn packaged(source: &str) -> Self {
        let blob = build_view_package_for(source, &origin(), profile()).expect("packages");
        let mut rt = Rt::default();
        let view = load_view_with(
            &blob,
            &mut rt.store,
            &mut rt.states,
            &mut rt.bindings,
            &mut rt.lists,
            &mut |_| {},
        )
        .expect("loads");
        rt.root = view.root;
        rt.view = view.host;
        rt.layout();
        rt
    }

    fn reload(&mut self, source: &str, profile: TargetProfile) {
        let candidate = plan_view_for(source, &origin(), profile).expect("compiles");
        let mut live = LiveRuntime {
            store: &mut self.store,
            states: &mut self.states,
            bindings: &mut self.bindings,
            effects: &mut self.effects,
            lists: &mut self.lists,
            text_edits: &mut self.text_edits,
            projectors: &mut self.projectors,
            root: self.root,
            scratch: &mut self.scratch,
            nodes: &mut self.nodes,
            view: &mut self.view,
        };
        let done = transact(&mut live, &self.last_good, candidate);
        self.root = live.root;
        self.last_good = done.candidate;
        self.layout();
    }

    fn layout(&mut self) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };
        if let Some(root) = self.root {
            self.store.layout(root, surface, &mut Vec::new());
        }
    }

    fn click(&mut self, x: f32, y: f32) {
        let root = self.root.expect("mounted");
        let mut chain = Vec::new();
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let event = PointerEvent {
                x,
                y,
                phase,
                buttons: PointerButtons::PRIMARY,
                modifiers: Default::default(),
            };
            PointerRouter::route(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                root,
                event,
                &mut chain,
            );
        }
        self.flush();
    }

    fn locale(&mut self, tag: &str) {
        self.states
            .update_env(|e: &mut Environment| e.locale = tag.into());
        self.states.settle_env(&self.store);
        self.flush();
    }

    fn flush(&mut self) {
        let mut changed = Vec::new();
        self.states.take_pending(&mut changed);
        self.store
            .flush_state_transactions(&changed, &self.bindings);
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
            &changed,
        );
        self.layout();
    }

    /// The texts declared since the last call, in node order.
    fn texts(&mut self) -> Vec<String> {
        let mut requests = Vec::new();
        self.store.take_text_requests(&mut requests);
        requests.sort_by_key(|(id, _)| id.index());
        for (id, request) in &requests {
            match self.shown.iter_mut().find(|(n, _)| n == id) {
                Some(entry) => entry.1 = request.text.clone(),
                None => self.shown.push((*id, request.text.clone())),
            }
        }
        requests.into_iter().map(|(_, r)| r.text).collect()
    }

    /// The text each label shows, in node order.
    fn labels(&mut self) -> Vec<String> {
        let _ = self.texts();
        self.shown.sort_by_key(|(id, _)| id.index());
        self.shown.iter().map(|(_, t)| t.clone()).collect()
    }
}

#[test]
fn messages_show_in_the_readers_locale_under_every_target() {
    for mut rt in [Rt::reloaded(SOURCE), Rt::packaged(SOURCE)] {
        assert_eq!(
            rt.texts(),
            [
                "No unread messages",
                "Inbox",
                "Glad",
                "2nd",
                "Hello, Ann!",
                "1,234,567 bytes, 0.5",
                "Inbox"
            ],
            "an undetermined reader reads the source locale"
        );
        rt.click(5.0, 5.0);
        assert_eq!(
            rt.texts(),
            ["1 unread message", "3rd"],
            "only what `unread` reaches"
        );
        rt.click(5.0, 5.0);
        assert_eq!(rt.texts(), ["2 unread messages", "4th"]);

        rt.locale("ar-EG");
        let ar = rt.labels();
        assert_eq!(ar[0], "رسالتان", "Arabic's dual: {ar:?}");
        assert_eq!(
            ar[1], "Inbox",
            "an untranslated message falls back to the source"
        );
        assert_eq!(ar[3], "٤th", "the reader's digits in the source's pattern");

        rt.locale("de-AT");
        let de = rt.labels();
        assert_eq!(de[0], "2 ungelesene Nachrichten");
        assert_eq!(de[1], "Posteingang");
        assert_eq!(de[6], "Posteingang", "a computed reads the locale too");
        assert_eq!(
            de[5], "1\u{a0}234\u{a0}567 bytes, 0,5",
            "Austrian grouping, though the pattern is the source's"
        );

        rt.locale("zh-TW");
        let zh = rt.labels();
        assert_eq!(zh[1], "收件匣", "zh-TW falls back to zh-Hant: {zh:?}");
        assert_eq!(zh[0], "2 unread messages", "then to the source");

        rt.click(5.0, 45.0);
        assert_eq!(rt.texts(), ["Meh"], "a select by an enum variant");
        rt.click(5.0, 25.0);
        assert_eq!(rt.texts(), ["Other"], "a key held in state");
        rt.locale("de");
        let de = rt.labels();
        assert_eq!(de[1], "Other", "de leaves it to the source");
        assert_eq!(de[5], "1.234.567 bytes, 0,5", "German grouping");
    }
}

#[test]
fn a_message_key_held_in_state_shows_its_key_once_the_catalog_lost_it() {
    let mut rt = Rt::reloaded(SOURCE);
    let _ = rt.texts();
    rt.click(5.0, 25.0);
    assert_eq!(rt.texts(), ["Other"], "a key set by a handler");
    let source = SOURCE.replace("key = \"inbox.other\";", "key = \"inbox.title\";");
    let edited = EN.replace("other = \"Other\"\n", "");
    let messages = Messages::compile("en", &[file("en", &edited)]);
    rt.reload(
        &source,
        TargetProfile {
            messages: Some(Rc::new(messages)),
            ..TargetProfile::default()
        },
    );
    let texts = rt.texts();
    assert!(
        texts.contains(&"inbox.other".to_owned()),
        "the kept state's key shows: {texts:?}"
    );
}

#[test]
fn a_catalog_edit_reloads_the_texts() {
    let mut rt = Rt::reloaded(SOURCE);
    let _ = rt.texts();
    let edited = EN.replace("title = \"Inbox\"", "title = \"Mail\"");
    let messages = Messages::compile("en", &[file("en", &edited)]);
    assert!(messages.issues().is_empty());
    rt.reload(
        SOURCE,
        TargetProfile {
            messages: Some(Rc::new(messages)),
            ..TargetProfile::default()
        },
    );
    let texts = rt.texts();
    assert!(texts.contains(&"Mail".to_owned()), "{texts:?}");
}
