//! The schema of a native package: its libraries as JSON, every field of
//! every function, type, scheduler trait and widget but the implementation.
//!
//! A tool that compiles `.vs` without building the package's Rust reads it
//! ([`read`]) to resolve and check calls; the package's own tests write it
//! from the Rust libraries ([`write`]) and fail when it differs ([`check`]),
//! so it never drifts from them.

use std::borrow::ToOwned;
use std::boxed::Box;
use std::collections::HashMap;
use std::format;
use std::path::Path;
use std::string::{String, ToString};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::vec::Vec;

use viso_ende::{JsonValue, JsonWriter};

use super::{
    Determinism, HookDomain, NativeCx, NativeError, NativeFunction, NativeHook, NativeKind,
    NativeLibrary, NativeTrait, NativeType, NativeWidget, Natives, Ownership, Param, SchemaTy,
    ThreadDomain, VariantFields, WidgetEvent, WidgetProperty,
};
use crate::value::Value;

/// The schema format's name and version.
const FORMAT: &str = "viso-natives";
const VERSION: u32 = 1;

/// `libraries` as a native package's schema.
///
/// # Errors
///
/// A library declaring a widget a package cannot carry: one that is not a
/// Rust widget ([`NativeWidget::rust`]), or one with slots, groups of its own,
/// a group it provides or an event that bubbles.
pub fn write(libraries: &[&NativeLibrary]) -> Result<String, String> {
    let mut out =
        format!("{{\n  \"format\": \"{FORMAT}\",\n  \"version\": {VERSION},\n  \"libraries\": [");
    for (i, library) in libraries.iter().enumerate() {
        for widget in library.widgets {
            carried(library, widget)?;
        }
        let mut head = JsonWriter::new();
        head.begin_object();
        head.name("path");
        head.string(library.path);
        head.name("version");
        head.uint(library.version.into());
        head.name("derives");
        head.begin_array();
        for derive in library.derives {
            head.string(derive);
        }
        head.end_array();
        head.end_object();
        let head = head.into_string();
        out.push_str(if i == 0 { "\n    " } else { ",\n    " });
        // The head's members, then the functions and types a line each.
        out.push_str(&head[..head.len() - 1]);
        out.push_str(",\n      \"functions\": [");
        lines(&mut out, library.functions.iter().map(function), "        ");
        out.push_str("],\n      \"types\": [");
        for (j, ty) in library.types.iter().enumerate() {
            let mut w = JsonWriter::new();
            w.begin_object();
            w.name("name");
            w.string(ty.name);
            w.name("variants");
            w.begin_array();
            for variant in ty.variants {
                w.string(variant);
            }
            w.end_array();
            w.name("ownership");
            w.string(ty.ownership.name());
            w.name("thread");
            w.string(ty.thread.name());
            w.name("value");
            w.bool(ty.value);
            w.name("fields");
            params(&mut w, ty.fields);
            if !ty.params.is_empty() {
                w.name("params");
                w.begin_array();
                for param in ty.params {
                    w.string(param);
                }
                w.end_array();
            }
            if !ty.payloads.is_empty() {
                w.name("payloads");
                w.begin_array();
                for payload in ty.payloads {
                    w.begin_object();
                    w.name("named");
                    w.bool(payload.named);
                    w.name("fields");
                    params(&mut w, payload.fields);
                    w.end_object();
                }
                w.end_array();
            }
            w.end_object();
            let ty_head = w.into_string();
            out.push_str(if j == 0 { "\n        " } else { ",\n        " });
            out.push_str(&ty_head[..ty_head.len() - 1]);
            out.push_str(", \"methods\": [");
            lines(&mut out, ty.methods.iter().map(function), "          ");
            out.push_str("]}");
        }
        if !library.types.is_empty() {
            out.push_str("\n      ");
        }
        out.push(']');
        if !library.traits.is_empty() {
            out.push_str(",\n      \"traits\": [");
            lines(
                &mut out,
                library.traits.iter().map(native_trait),
                "        ",
            );
            out.push(']');
        }
        if !library.widgets.is_empty() {
            out.push_str(",\n      \"widgets\": [");
            lines(&mut out, library.widgets.iter().map(widget), "        ");
            out.push(']');
        }
        out.push_str("\n    }");
    }
    if !libraries.is_empty() {
        out.push_str("\n  ");
    }
    out.push_str("]\n}\n");
    Ok(out)
}

/// `items` into `out` a line each, indented by `indent`.
fn lines(out: &mut String, items: impl Iterator<Item = String>, indent: &str) {
    let mut any = false;
    for (i, item) in items.enumerate() {
        out.push_str(if i == 0 { "\n" } else { ",\n" });
        out.push_str(indent);
        out.push_str(&item);
        any = true;
    }
    if any {
        out.push('\n');
        out.push_str(&indent[..indent.len() - 2]);
    }
}

fn function(f: &NativeFunction) -> String {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.name("name");
    w.string(f.name);
    w.name("kind");
    w.string(f.kind.keyword());
    w.name("params");
    params(&mut w, f.params);
    w.name("ret");
    ty(&mut w, &f.ret);
    w.name("capabilities");
    w.begin_array();
    for c in f.capabilities {
        w.string(c);
    }
    w.end_array();
    w.name("thread");
    w.string(f.thread.name());
    w.name("deterministic");
    w.bool(f.deterministic);
    w.name("realtime_safe");
    w.bool(f.realtime_safe);
    w.name("cost");
    w.uint(f.cost.into());
    w.name("constant");
    w.bool(f.constant);
    w.name("property");
    w.bool(f.property);
    w.name("determinism");
    w.string(f.determinism.name());
    w.name("presentation");
    w.bool(f.presentation);
    w.name("debug_draw");
    w.bool(f.debug_draw);
    w.end_object();
    w.into_string()
}

/// Whether a package may carry `widget` of `library`: a Rust widget with no
/// slots, groups or provided group of its own and no event that bubbles.
fn carried(library: &NativeLibrary, widget: &NativeWidget) -> Result<(), String> {
    let path = format!("{}::{}", library.path, widget.name);
    if !widget.is_rust() {
        return Err(format!(
            "`{path}`: a native package's widget is a Rust widget (`NativeWidget::rust`)"
        ));
    }
    if !widget.slots.is_empty() || !widget.groups.is_empty() || widget.provides.is_some() {
        return Err(format!(
            "`{path}`: a Rust widget builds its own content, so it has no slots, and takes the box's groups"
        ));
    }
    if let Some(event) = widget.own_events().find(|e| e.bubbles) {
        return Err(format!(
            "`{path}`: its event `{}` bubbles; a Rust widget's events reach its own node",
            event.name
        ));
    }
    Ok(())
}

fn native_trait(t: &NativeTrait) -> String {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.name("name");
    w.string(t.name);
    w.name("hooks");
    w.begin_array();
    for hook in t.hooks {
        w.begin_object();
        w.name("name");
        w.string(hook.name);
        w.name("params");
        params(&mut w, hook.params);
        w.name("domain");
        w.string(domain_name(hook.domain));
        w.end_object();
    }
    w.end_array();
    w.end_object();
    w.into_string()
}

fn domain_name(domain: HookDomain) -> &'static str {
    match domain {
        HookDomain::Simulation => "simulation",
        HookDomain::Presentation => "presentation",
        HookDomain::Realtime => "realtime",
    }
}

fn widget(widget: &NativeWidget) -> String {
    let mut w = JsonWriter::new();
    w.begin_object();
    w.name("name");
    w.string(widget.name);
    w.name("properties");
    w.begin_array();
    for p in widget.own_properties() {
        w.begin_object();
        w.name("name");
        w.string(p.name);
        w.name("ty");
        w.string(p.ty);
        w.name("two_way");
        w.bool(p.two_way);
        w.name("percent_basis");
        w.bool(p.percent_basis);
        w.name("styleable");
        w.bool(p.styleable);
        w.name("localizable");
        w.bool(p.localizable);
        w.end_object();
    }
    w.end_array();
    w.name("events");
    w.begin_array();
    for e in widget.own_events() {
        w.begin_object();
        w.name("name");
        w.string(e.name);
        if let Some(payload) = e.payload {
            w.name("payload");
            w.string(payload);
        }
        w.end_object();
    }
    w.end_array();
    w.name("interactive");
    w.bool(widget.interactive);
    w.end_object();
    w.into_string()
}

/// Parameters or fields, each its name and type.
fn params(w: &mut JsonWriter, params: &[Param]) {
    w.begin_array();
    for p in params {
        w.begin_object();
        w.name("name");
        w.string(p.name);
        w.name("ty");
        ty(w, &p.ty);
        w.end_object();
    }
    w.end_array();
}

fn ty(w: &mut JsonWriter, ty: &SchemaTy) {
    let leaf = match ty {
        SchemaTy::Unit => "Unit",
        SchemaTy::Bool => "Bool",
        SchemaTy::I64 => "I64",
        SchemaTy::F32 => "F32",
        SchemaTy::F64 => "F64",
        SchemaTy::Duration => "Duration",
        SchemaTy::Ticks => "Ticks",
        SchemaTy::String => "String",
        SchemaTy::Action => "Action",
        SchemaTy::Tag => "Tag",
        SchemaTy::AudioCommand => "AudioCommand",
        SchemaTy::AudioEvent => "AudioEvent",
        SchemaTy::Param(index) => {
            w.begin_object();
            w.name("Param");
            w.uint(u64::from(*index));
            w.end_object();
            return;
        }
        SchemaTy::List(inner) | SchemaTy::Option(inner) => {
            w.begin_object();
            w.name(if matches!(ty, SchemaTy::List(_)) {
                "List"
            } else {
                "Option"
            });
            self::ty(w, inner);
            w.end_object();
            return;
        }
        SchemaTy::Result(ok, err) => {
            w.begin_object();
            w.name("Result");
            w.begin_array();
            self::ty(w, ok);
            self::ty(w, err);
            w.end_array();
            w.end_object();
            return;
        }
        SchemaTy::Handle(path) | SchemaTy::Value(path) | SchemaTy::Enum(path) => {
            w.begin_object();
            w.name(match ty {
                SchemaTy::Handle(_) => "Handle",
                SchemaTy::Value(_) => "Value",
                _ => "Enum",
            });
            w.string(path);
            w.end_object();
            return;
        }
    };
    w.string(leaf);
}

/// The libraries a native package's schema holds, interned for the process:
/// the same text gives the same libraries.
///
/// # Errors
///
/// Text that is not such a schema.
pub fn read(text: &str) -> Result<Vec<&'static NativeLibrary>, String> {
    static READ: OnceLock<Mutex<HashMap<String, Vec<&'static NativeLibrary>>>> = OnceLock::new();
    let read = READ.get_or_init(Mutex::default);
    if let Some(libraries) = read
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(text)
    {
        return Ok(libraries.clone());
    }
    let libraries = parse(text)?;
    // Read meanwhile on another thread, the first kept.
    Ok(read
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(text.to_owned())
        .or_insert(libraries)
        .clone())
}

fn parse(text: &str) -> Result<Vec<&'static NativeLibrary>, String> {
    let json = JsonValue::parse(text).map_err(|e| format!("not JSON: {e}"))?;
    if json.get("format").and_then(JsonValue::as_str) != Some(FORMAT) {
        return Err(format!("not a `{FORMAT}` schema"));
    }
    let version = json.get("version").and_then(JsonValue::as_f64);
    if version != Some(f64::from(VERSION)) {
        return Err(format!(
            "schema version {} is not {VERSION}, the one this compiler reads",
            version.map_or("none".to_owned(), |v| v.to_string())
        ));
    }
    array(&json, "libraries")?
        .iter()
        .map(|library| {
            let path = leak(string(library, "path")?);
            let functions = array(library, "functions")?
                .iter()
                .map(native_function)
                .collect::<Result<Vec<_>, _>>()?;
            let types = array(library, "types")?
                .iter()
                .map(native_type)
                .collect::<Result<Vec<_>, _>>()?;
            let derives = array(library, "derives")?
                .iter()
                .map(|d| d.as_str().map(leak).ok_or("a derive is not a string"))
                .collect::<Result<Vec<_>, _>>()?;
            let traits = match library.get("traits") {
                None => Vec::new(),
                Some(_) => array(library, "traits")?
                    .iter()
                    .map(read_trait)
                    .collect::<Result<Vec<_>, _>>()?,
            };
            let widgets = match library.get("widgets") {
                None => Vec::new(),
                Some(_) => array(library, "widgets")?
                    .iter()
                    .map(read_widget)
                    .collect::<Result<Vec<_>, _>>()?,
            };
            Ok(&*Box::leak(Box::new(NativeLibrary {
                path,
                version: number(library, "version")?,
                functions: Box::leak(functions.into_boxed_slice()),
                types: Box::leak(types.into_boxed_slice()),
                traits: Box::leak(traits.into_boxed_slice()),
                derives: Box::leak(derives.into_boxed_slice()),
                widgets: Box::leak(widgets.into_boxed_slice()),
            })))
        })
        .collect()
}

fn read_trait(json: &JsonValue) -> Result<NativeTrait, String> {
    let hooks = array(json, "hooks")?
        .iter()
        .map(|hook| {
            let domain = string(hook, "domain")?;
            Ok(NativeHook {
                name: leak(string(hook, "name")?),
                params: read_params(hook, "params")?,
                domain: [
                    HookDomain::Simulation,
                    HookDomain::Presentation,
                    HookDomain::Realtime,
                ]
                .into_iter()
                .find(|d| domain_name(*d) == domain)
                .ok_or_else(|| format!("`{domain}` is not a hook domain"))?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(NativeTrait {
        name: leak(string(json, "name")?),
        hooks: Box::leak(hooks.into_boxed_slice()),
    })
}

fn read_widget(json: &JsonValue) -> Result<NativeWidget, String> {
    let properties = array(json, "properties")?
        .iter()
        .map(|p| {
            Ok(WidgetProperty {
                name: leak(string(p, "name")?),
                ty: leak(string(p, "ty")?),
                two_way: flag(p, "two_way")?,
                percent_basis: flag(p, "percent_basis")?,
                styleable: flag(p, "styleable")?,
                localizable: flag(p, "localizable")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let events = array(json, "events")?
        .iter()
        .map(|e| {
            Ok(WidgetEvent {
                name: leak(string(e, "name")?),
                payload: match e.get("payload") {
                    None => None,
                    Some(_) => Some(leak(string(e, "payload")?)),
                },
                bubbles: false,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let properties: &'static [WidgetProperty] = Box::leak(properties.into_boxed_slice());
    let events: &'static [WidgetEvent] = Box::leak(events.into_boxed_slice());
    let widget = NativeWidget::rust(leak(string(json, "name")?))
        .properties(Box::leak(Box::new([properties])))
        .events(Box::leak(Box::new([events])));
    Ok(if flag(json, "interactive")? {
        widget.interactive()
    } else {
        widget
    })
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.into())
}

fn field<'a>(json: &'a JsonValue, name: &str) -> Result<&'a JsonValue, String> {
    json.get(name).ok_or_else(|| format!("`{name}` is missing"))
}

fn string<'a>(json: &'a JsonValue, name: &str) -> Result<&'a str, String> {
    field(json, name)?
        .as_str()
        .ok_or_else(|| format!("`{name}` is not a string"))
}

fn array<'a>(json: &'a JsonValue, name: &str) -> Result<&'a [JsonValue], String> {
    field(json, name)?
        .as_array()
        .ok_or_else(|| format!("`{name}` is not an array"))
}

fn flag(json: &JsonValue, name: &str) -> Result<bool, String> {
    field(json, name)?
        .as_bool()
        .ok_or_else(|| format!("`{name}` is not a boolean"))
}

fn number(json: &JsonValue, name: &str) -> Result<u32, String> {
    field(json, name)?
        .as_f64()
        .filter(|n| n.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(n))
        .map(|n| n as u32)
        .ok_or_else(|| format!("`{name}` is not a whole number"))
}

fn thread(name: &str) -> Result<ThreadDomain, String> {
    [ThreadDomain::Any, ThreadDomain::Ui, ThreadDomain::Worker]
        .into_iter()
        .find(|t| t.name() == name)
        .ok_or_else(|| format!("`{name}` is not a thread domain"))
}

fn native_function(json: &JsonValue) -> Result<NativeFunction, String> {
    let name = string(json, "name")?;
    let kind = match string(json, "kind")? {
        "fn" => NativeKind::Fn,
        "action" => NativeKind::Action,
        "task" => NativeKind::Task,
        other => return Err(format!("`{other}` is not a native kind")),
    };
    let params = read_params(json, "params")?;
    let capabilities = array(json, "capabilities")?
        .iter()
        .map(|c| c.as_str().map(leak).ok_or("a capability is not a string"))
        .collect::<Result<Vec<_>, _>>()?;
    let determinism = match string(json, "determinism")? {
        "none" => Determinism::None,
        "same_binary" => Determinism::SameBinary,
        "cross_platform" => Determinism::CrossPlatform,
        other => return Err(format!("`{other}` is not a determinism")),
    };
    Ok(NativeFunction {
        name: leak(name),
        kind,
        params,
        ret: schema_ty(field(json, "ret")?)?,
        capabilities: Box::leak(capabilities.into_boxed_slice()),
        thread: thread(string(json, "thread")?)?,
        deterministic: flag(json, "deterministic")?,
        realtime_safe: flag(json, "realtime_safe")?,
        cost: number(json, "cost")?,
        constant: flag(json, "constant")?,
        property: flag(json, "property")?,
        determinism,
        presentation: flag(json, "presentation")?,
        debug_draw: flag(json, "debug_draw")?,
        call: unlinked,
    })
}

fn read_params(json: &JsonValue, name: &str) -> Result<&'static [Param], String> {
    let params = array(json, name)?
        .iter()
        .map(|p| {
            Ok(Param {
                name: leak(string(p, "name")?),
                ty: schema_ty(field(p, "ty")?)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Box::leak(params.into_boxed_slice()))
}

fn native_type(json: &JsonValue) -> Result<NativeType, String> {
    let variants = array(json, "variants")?
        .iter()
        .map(|v| v.as_str().map(leak).ok_or("a variant is not a string"))
        .collect::<Result<Vec<_>, _>>()?;
    let methods = array(json, "methods")?
        .iter()
        .map(native_function)
        .collect::<Result<Vec<_>, _>>()?;
    let ownership = match string(json, "ownership")? {
        "shared" => Ownership::Shared,
        "borrowed" => Ownership::Borrowed,
        other => return Err(format!("`{other}` is not an ownership")),
    };
    let payloads = match json.get("payloads") {
        None => Vec::new(),
        Some(_) => array(json, "payloads")?
            .iter()
            .map(|p| {
                Ok(VariantFields {
                    fields: read_params(p, "fields")?,
                    named: flag(p, "named")?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
    };
    let params = match json.get("params") {
        None => Vec::new(),
        Some(_) => array(json, "params")?
            .iter()
            .map(|p| {
                p.as_str()
                    .map(leak)
                    .ok_or("a type parameter is not a string")
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    if !payloads.is_empty() && payloads.len() != variants.len() {
        return Err("a payload for each variant, or none".into());
    }
    Ok(NativeType {
        name: leak(string(json, "name")?),
        variants: Box::leak(variants.into_boxed_slice()),
        payloads: Box::leak(payloads.into_boxed_slice()),
        params: Box::leak(params.into_boxed_slice()),
        ownership,
        thread: thread(string(json, "thread")?)?,
        value: flag(json, "value")?,
        fields: read_params(json, "fields")?,
        methods: Box::leak(methods.into_boxed_slice()),
    })
}

fn schema_ty(json: &JsonValue) -> Result<SchemaTy, String> {
    if let Some(leaf) = json.as_str() {
        return Ok(match leaf {
            "Unit" => SchemaTy::Unit,
            "Bool" => SchemaTy::Bool,
            "I64" => SchemaTy::I64,
            "F32" => SchemaTy::F32,
            "F64" => SchemaTy::F64,
            "Duration" => SchemaTy::Duration,
            "Ticks" => SchemaTy::Ticks,
            "String" => SchemaTy::String,
            "Action" => SchemaTy::Action,
            "Tag" => SchemaTy::Tag,
            "AudioCommand" => SchemaTy::AudioCommand,
            "AudioEvent" => SchemaTy::AudioEvent,
            other => return Err(format!("`{other}` is not a schema type")),
        });
    }
    let Some(JsonValue::Object(members)) = Some(json) else {
        return Err("a type is neither a name nor an object".into());
    };
    let [(kind, inner)] = members.as_slice() else {
        return Err("a type object has one member".into());
    };
    let path = || {
        inner
            .as_str()
            .map(leak)
            .ok_or_else(|| format!("`{kind}` names no path"))
    };
    Ok(match kind.as_str() {
        "List" => SchemaTy::List(Box::leak(Box::new(schema_ty(inner)?))),
        "Option" => SchemaTy::Option(Box::leak(Box::new(schema_ty(inner)?))),
        "Result" => match inner.as_array() {
            Some([ok, err]) => SchemaTy::Result(
                Box::leak(Box::new(schema_ty(ok)?)),
                Box::leak(Box::new(schema_ty(err)?)),
            ),
            _ => return Err("`Result` is not a pair of types".into()),
        },
        "Handle" => SchemaTy::Handle(path()?),
        "Value" => SchemaTy::Value(path()?),
        "Enum" => SchemaTy::Enum(path()?),
        "Param" => SchemaTy::Param(
            inner
                .as_f64()
                .filter(|n| n.fract() == 0.0 && (0.0..=f64::from(u8::MAX)).contains(n))
                .ok_or("`Param` is not a parameter index")? as u8,
        ),
        other => return Err(format!("`{other}` is not a schema type")),
    })
}

/// A package native's implementation until the host links the Rust one; a
/// linked module never calls it.
fn unlinked(_: &mut NativeCx<'_>, _: &[Value]) -> Result<Value, NativeError> {
    Err(NativeError::new(
        "a native package's function runs only once its library is linked",
    ))
}

/// Fails unless the schema at `path` is `libraries`, written as [`write`]
/// writes them, and they register beside the standard libraries; with
/// `VISO_WRITE_NATIVES` set it writes the schema instead. A relative `path`
/// is the package's: beside its manifest (`CARGO_MANIFEST_DIR`).
///
/// A native package's test:
///
/// ```ignore
/// #[test]
/// fn schema() {
///     viso::native::check(NATIVES, "natives.json");
/// }
/// ```
///
/// # Panics
///
/// When the schema is missing or differs, when `libraries` conflict with
/// the standard ones (`E6101`), or when they declare a widget a package
/// cannot carry ([`write`]).
#[track_caller]
pub fn check(libraries: &[&'static NativeLibrary], path: impl AsRef<Path>) {
    let path = match std::env::var_os("CARGO_MANIFEST_DIR") {
        Some(dir) if path.as_ref().is_relative() => Path::new(&dir).join(path),
        _ => path.as_ref().to_owned(),
    };
    if let Err(conflict) = Natives::with(libraries) {
        panic!("the libraries do not register beside the standard ones: {conflict}");
    }
    let text = write(libraries).unwrap_or_else(|e| panic!("{e}"));
    if std::env::var_os("VISO_WRITE_NATIVES").is_some() {
        if let Err(e) = std::fs::write(&path, &text) {
            panic!("`{}`: {e}", path.display());
        }
        return;
    }
    match std::fs::read_to_string(&path) {
        Ok(found) if found == text => {}
        Ok(_) => panic!(
            "`{}` is not the libraries' schema: rerun with `VISO_WRITE_NATIVES=1`",
            path.display()
        ),
        Err(e) => panic!(
            "`{}`: {e}: write it with `VISO_WRITE_NATIVES=1`",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::ToOwned;
    use std::format;
    use std::string::String;
    use std::vec::Vec;

    use super::super::{NativeObject, Obj};
    use super::*;

    static RESPONSE_METHODS: [NativeFunction; 2] = [
        crate::native!(fn "status" |_cx, r: Obj<Thing>| -> i64 { Ok(r.0) }).property(),
        crate::native!(fn "header" |_cx, r: Obj<Thing>, name: String| -> Option<String> {
            Ok(Some(format!("{}{name}", r.0)))
        }),
    ];

    #[derive(Debug)]
    struct Thing(i64);

    impl NativeObject for Thing {
        const PATH: &'static str = "net::Thing";
    }

    static LIBRARY: NativeLibrary = NativeLibrary {
        path: "net",
        version: 3,
        functions: &[
            crate::native!(task "get" |_cx, url: String, tries: Option<i64>| -> Obj<Thing> {
                Ok(Obj::new(Thing(url.len() as i64 + tries.unwrap_or(0))))
            })
            .requires(&["network.http"]),
            crate::native!(fn "lines" |_cx, text: String| -> Vec<String> {
                Ok(text.lines().map(str::to_owned).collect())
            })
            .deterministic(),
            crate::native!(fn "parse" |_cx, text: String| -> Result<Option<i64>, Obj<Thing>> {
                Ok(Ok(text.parse().ok()))
            }),
        ],
        types: &[
            NativeType::new("Thing", &RESPONSE_METHODS),
            NativeType::enumeration("Method", &["get", "post"]),
        ],
        traits: &[],
        derives: &["Tagged"],
        widgets: &[],
    };

    #[test]
    fn a_schema_reads_back_as_written() {
        let text = write(&[&LIBRARY]).unwrap();
        assert!(
            text.contains("\n        {\"name\":\"get\",\"kind\":\"task\""),
            "{text}"
        );
        let back = read(&text).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(write(&back).unwrap(), text);
        let (ours, theirs) = (
            Natives::with(&[&LIBRARY]).unwrap(),
            Natives::with(&back).unwrap(),
        );
        for path in [
            "net::get",
            "net::lines",
            "net::parse",
            "net::Thing::status",
            "net::Thing::header",
        ] {
            let (a, b) = (ours.function(path).unwrap(), theirs.function(path).unwrap());
            assert_eq!(a.function.signature(), b.function.signature(), "{path}");
            assert_eq!(a.function.capabilities, b.function.capabilities, "{path}");
        }
        assert!(
            theirs
                .function("net::Thing::status")
                .unwrap()
                .function
                .property
        );
        // Read again, the same libraries.
        assert!(core::ptr::eq(back[0], read(&text).unwrap()[0]));
        assert!(read("{\"format\":\"viso-natives\",\"version\":2,\"libraries\":[]}").is_err());
    }

    static GAUGE_PROPS: [WidgetProperty; 2] = [
        WidgetProperty::new("value", "F32"),
        WidgetProperty::new("label", "String").localizable(),
    ];
    static GAUGE_EVENTS: [WidgetEvent; 1] = [WidgetEvent::new("picked", "F32")];
    static TICK: [NativeHook; 1] = [NativeHook {
        name: "tick",
        params: &[Param {
            name: "dt",
            ty: SchemaTy::F32,
        }],
        domain: HookDomain::Presentation,
    }];

    static DIALS: NativeLibrary = NativeLibrary {
        path: "dials",
        version: 1,
        functions: &[],
        types: &[],
        traits: &[NativeTrait {
            name: "Ticking",
            hooks: &TICK,
        }],
        derives: &[],
        widgets: &[NativeWidget::rust("Gauge")
            .properties(&[&GAUGE_PROPS])
            .events(&[&GAUGE_EVENTS])
            .interactive()],
    };

    #[test]
    fn traits_and_rust_widgets_read_back_as_written() {
        let text = write(&[&DIALS]).unwrap();
        let back = read(&text).unwrap();
        assert_eq!(write(&back).unwrap(), text);
        assert_eq!(back[0].traits, DIALS.traits);
        assert_eq!(back[0].widgets, DIALS.widgets);
        let natives = Natives::with(&back).unwrap();
        let gauge = natives.widget("Gauge").unwrap();
        assert_eq!(gauge.own_property("label"), Some(1));
        // The box comes with it.
        assert!(gauge.property("width").is_some() && gauge.event("click").is_some());
    }

    #[test]
    fn a_package_carries_only_rust_widgets() {
        static STACKED: NativeLibrary = NativeLibrary {
            path: "stacked",
            version: 1,
            functions: &[],
            types: &[],
            traits: &[],
            derives: &[],
            widgets: &[NativeWidget::new("Pile", super::super::WidgetNode::Stack)],
        };
        let error = write(&[&STACKED]).unwrap_err();
        assert!(error.contains("`stacked::Pile`"), "{error}");
    }
}
