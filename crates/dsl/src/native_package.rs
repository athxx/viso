//! Native packages: native libraries a Cargo dependency of the app carries,
//! which its `.vs` code imports as it imports the standard ones.
//!
//! Such a crate declares them in its manifest:
//!
//! ```toml
//! [package.metadata.viso.natives]
//! schema = "natives.json"   # the libraries' schema, beside the manifest
//! library = "NATIVES"       # the `&'static [&'static NativeLibrary]` they are, in the crate
//! feature = "dsl"           # the feature that builds them, where one does
//! ```
//!
//! The schema is the libraries as [`write`] writes them, lossless but for
//! each function's implementation; the crate's own tests write it from the
//! Rust libraries, so it never drifts from them. A tool that compiles the
//! app's `.vs` without building its Rust reads it with [`read`]: the
//! schemas enter the registry as stubs, and the host links the module to
//! the Rust libraries, where another signature is `E6101` again.
//!
//! [`discover`] finds the packages among the app's direct dependencies
//! through `cargo metadata`, once per manifest and lock file for the
//! process; [`registry`] is the standard libraries and theirs, and
//! [`compile_linked`] compiles against them once a source needs them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use viso_behavior::Value;
use viso_behavior::native::{
    Determinism, NativeCx, NativeError, NativeFunction, NativeKind, NativeLibrary, NativeType,
    Natives, Ownership, Param, SchemaConflict, SchemaTy, ThreadDomain,
};
use viso_ende::{JsonValue, JsonWriter};

use crate::diag::{Diagnostic, Severity};
use crate::frontend::Compiled;

/// The schema format's name and version.
const FORMAT: &str = "viso-natives";
const VERSION: u32 = 1;

/// A dependency's native libraries.
#[derive(Debug, Clone)]
pub struct NativePackage {
    /// The Cargo package's name.
    pub name: String,
    /// The Rust path of its libraries, as the app's crate names them:
    /// `::viso_net::NATIVES`.
    pub library: String,
    /// Their schemas.
    pub libraries: Vec<&'static NativeLibrary>,
}

impl NativePackage {
    /// Whether one of its libraries holds the native at `path`.
    pub fn holds(&self, path: &str) -> bool {
        self.libraries.iter().any(|library| {
            path.strip_prefix(library.path)
                .is_some_and(|rest| rest.starts_with("::"))
        })
    }
}

/// `libraries` as a native package's schema.
///
/// # Errors
///
/// A library declaring scheduler traits or widgets, which a native package
/// does not carry.
pub fn write(libraries: &[&NativeLibrary]) -> Result<String, String> {
    let mut out =
        format!("{{\n  \"format\": \"{FORMAT}\",\n  \"version\": {VERSION},\n  \"libraries\": [");
    for (i, library) in libraries.iter().enumerate() {
        if !library.traits.is_empty() || !library.widgets.is_empty() {
            return Err(format!(
                "`{}`: a native package carries functions and types, not traits or widgets",
                library.path
            ));
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
        out.push_str("]\n    }");
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
    w.begin_array();
    for p in f.params {
        w.begin_object();
        w.name("name");
        w.string(p.name);
        w.name("ty");
        ty(&mut w, &p.ty);
        w.end_object();
    }
    w.end_array();
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
            Ok(&*Box::leak(Box::new(NativeLibrary {
                path,
                version: number(library, "version")?,
                functions: Box::leak(functions.into_boxed_slice()),
                types: Box::leak(types.into_boxed_slice()),
                traits: &[],
                derives: Box::leak(derives.into_boxed_slice()),
                widgets: &[],
            })))
        })
        .collect()
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
    let params = array(json, "params")?
        .iter()
        .map(|p| {
            Ok(Param {
                name: leak(string(p, "name")?),
                ty: schema_ty(field(p, "ty")?)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
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
        params: Box::leak(params.into_boxed_slice()),
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
    Ok(NativeType {
        name: leak(string(json, "name")?),
        variants: Box::leak(variants.into_boxed_slice()),
        ownership,
        thread: thread(string(json, "thread")?)?,
        value: flag(json, "value")?,
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
        "Handle" => SchemaTy::Handle(path()?),
        "Value" => SchemaTy::Value(path()?),
        "Enum" => SchemaTy::Enum(path()?),
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

/// What a manifest's discovery was made from, so a change to either file
/// makes it again.
type Stamp = (Option<SystemTime>, Option<SystemTime>);

/// Each manifest's packages, with what they were found from.
type Found = HashMap<PathBuf, (Stamp, Arc<[NativePackage]>)>;

/// The native packages among the direct dependencies of the Cargo package
/// whose manifest is `manifest`, as `cargo metadata` resolves them, the
/// answer kept for the process until the manifest or the lock file changes.
///
/// # Errors
///
/// Cargo not running or failing, or a package's schema missing or not one.
pub fn discover(manifest: &Path) -> Result<Arc<[NativePackage]>, String> {
    let stamp = stamp(manifest);
    if let Some(packages) = known(manifest, stamp) {
        return Ok(packages);
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = std::process::Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(manifest)
        .output()
        .map_err(|error| format!("cannot run `cargo metadata`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`cargo metadata` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let metadata = JsonValue::parse(&String::from_utf8_lossy(&output.stdout))
        .map_err(|error| format!("`cargo metadata` wrote no JSON: {error}"))?;
    let packages: Arc<[NativePackage]> = packages(&metadata, |path| {
        std::fs::read_to_string(path).map_err(|e| format!("`{}`: {e}", path.display()))
    })?
    .into();
    found()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(manifest.to_owned(), (stamp, packages.clone()));
    Ok(packages)
}

/// What [`discover`] found for each manifest, and when.
fn found() -> &'static Mutex<Found> {
    static FOUND: OnceLock<Mutex<Found>> = OnceLock::new();
    FOUND.get_or_init(Mutex::default)
}

/// When `manifest` and the lock file beside it or above it last changed.
fn stamp(manifest: &Path) -> Stamp {
    let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let lock = manifest
        .ancestors()
        .skip(1)
        .map(|dir| dir.join("Cargo.lock"))
        .find(|lock| lock.is_file());
    (modified(manifest), lock.as_deref().and_then(modified))
}

/// What [`discover`] found for `manifest` as it is at `stamp`, if it looked.
fn known(manifest: &Path, stamp: Stamp) -> Option<Arc<[NativePackage]>> {
    let found = found().lock().unwrap_or_else(PoisonError::into_inner);
    let (was, packages) = found.get(manifest)?;
    (*was == stamp).then(|| packages.clone())
}

/// The manifest of the Cargo package `file` belongs to: the nearest
/// `Cargo.toml` above it.
pub fn manifest_of(file: &Path) -> Option<PathBuf> {
    file.ancestors()
        .skip(1)
        .map(|dir| dir.join("Cargo.toml"))
        .find(|manifest| manifest.is_file())
}

/// What was made of `.vs` code against the native packages of its Cargo
/// package.
pub struct Linked<T> {
    /// A compile, a loaded package.
    pub made: T,
    /// The packages it was made against: none when the standard libraries
    /// were enough.
    pub packages: Arc<[NativePackage]>,
    /// Why the packages could not be made against, when they were needed:
    /// their discovery failed or two of them conflict.
    pub unlinked: Option<String>,
}

/// What `compile` makes of `.vs` code of the Cargo package whose manifest
/// is `manifest`, as [`link_with`] makes it.
pub fn compile_linked(
    manifest: Option<&Path>,
    compile: impl Fn(Arc<Natives>) -> Compiled,
) -> Linked<Compiled> {
    link_with(manifest, compile, |compiled, visit| {
        compiled.diagnostics.iter_mut().for_each(visit);
    })
}

/// What `make` makes of `.vs` code of the Cargo package whose manifest is
/// `manifest`: against the standard libraries, or, when a path resolves to
/// nothing there (an `E2001` among the diagnostics `each` visits in what it
/// made),
/// against the package's native packages as well. Cargo runs only then,
/// and once per manifest and lock file for the process: later calls make it
/// against the packages found at once. Why they could not be found is a
/// note on each `E2001` it keeps.
pub fn link_with<T>(
    manifest: Option<&Path>,
    make: impl Fn(Arc<Natives>) -> T,
    each: impl Fn(&mut T, &mut dyn FnMut(&mut Diagnostic)),
) -> Linked<T> {
    let none = || Arc::from([]);
    let alone = |made| Linked {
        made,
        packages: none(),
        unlinked: None,
    };
    let Some(manifest) = manifest else {
        return alone(make(Natives::standard()));
    };
    let unresolved = |made: &mut T| {
        let mut found = false;
        each(made, &mut |d| {
            found |= d.code == "E2001" && d.severity == Severity::Error
        });
        found
    };
    let unlinked = |mut made: T, why: String| {
        let note = format!("the native packages are not known: {why}");
        each(&mut made, &mut |d| {
            if d.code == "E2001" {
                d.notes.push(note.clone());
            }
        });
        Linked {
            made,
            packages: none(),
            unlinked: Some(why),
        }
    };
    let packages = match known(manifest, stamp(manifest)) {
        Some(packages) => packages,
        None => {
            let mut made = make(Natives::standard());
            if !unresolved(&mut made) {
                return alone(made);
            }
            match discover(manifest) {
                Ok(packages) => packages,
                Err(why) => return unlinked(made, why),
            }
        }
    };
    if packages.is_empty() {
        return alone(make(Natives::standard()));
    }
    match registry(&packages) {
        Ok(natives) => Linked {
            made: make(natives),
            packages,
            unlinked: None,
        },
        Err(conflict) => unlinked(make(Natives::standard()), format!("E6101: {conflict}")),
    }
}

/// The native packages `metadata` says the root package depends on
/// directly, each schema read through `read_file`.
fn packages(
    metadata: &JsonValue,
    read_file: impl Fn(&Path) -> Result<String, String>,
) -> Result<Vec<NativePackage>, String> {
    let Some(resolve) = metadata.get("resolve").filter(|r| !r.is_null()) else {
        return Ok(Vec::new());
    };
    let Some(root) = resolve.get("root").and_then(JsonValue::as_str) else {
        return Ok(Vec::new());
    };
    let nodes = resolve
        .get("nodes")
        .and_then(JsonValue::as_array)
        .unwrap_or(&[]);
    let node = |id: &str| {
        nodes
            .iter()
            .find(|n| n.get("id").and_then(JsonValue::as_str) == Some(id))
    };
    let all = metadata
        .get("packages")
        .and_then(JsonValue::as_array)
        .unwrap_or(&[]);
    let package = |id: &str| {
        all.iter()
            .find(|p| p.get("id").and_then(JsonValue::as_str) == Some(id))
    };
    let deps = node(root)
        .and_then(|n| n.get("deps"))
        .and_then(JsonValue::as_array)
        .unwrap_or(&[]);
    let mut found = Vec::new();
    for dep in deps {
        // A normal dependency: a kind of `null`, for any platform.
        let normal = dep
            .get("dep_kinds")
            .and_then(JsonValue::as_array)
            .is_some_and(|kinds| {
                kinds
                    .iter()
                    .any(|k| k.get("kind").is_none_or(JsonValue::is_null))
            });
        let (Some(id), Some(extern_name)) = (
            dep.get("pkg").and_then(JsonValue::as_str),
            dep.get("name").and_then(JsonValue::as_str),
        ) else {
            continue;
        };
        let Some(declared) = package(id)
            .and_then(|p| p.get("metadata"))
            .and_then(|m| m.get("viso"))
            .and_then(|v| v.get("natives"))
        else {
            continue;
        };
        if !normal {
            continue;
        }
        let name = package(id)
            .and_then(|p| p.get("name"))
            .and_then(JsonValue::as_str)
            .unwrap_or(extern_name);
        if let Some(feature) = declared.get("feature").and_then(JsonValue::as_str) {
            let enabled = node(id)
                .and_then(|n| n.get("features"))
                .and_then(JsonValue::as_array)
                .is_some_and(|f| f.iter().any(|f| f.as_str() == Some(feature)));
            if !enabled {
                continue;
            }
        }
        let manifest = package(id)
            .and_then(|p| p.get("manifest_path"))
            .and_then(JsonValue::as_str)
            .ok_or_else(|| format!("`{name}` has no manifest path"))?;
        let dir = Path::new(manifest).parent().unwrap_or(Path::new("."));
        let schema = dir.join(string(declared, "schema").map_err(|e| format!("`{name}`: {e}"))?);
        let library = string(declared, "library").map_err(|e| format!("`{name}`: {e}"))?;
        let libraries =
            read(&read_file(&schema)?).map_err(|e| format!("`{}`: {e}", schema.display()))?;
        found.push(NativePackage {
            name: name.to_owned(),
            library: format!("::{extern_name}::{library}"),
            libraries,
        });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// The standard libraries and those of `packages`.
///
/// # Errors
///
/// Two of them declaring one path (`E6101`).
pub fn registry(packages: &[NativePackage]) -> Result<Arc<Natives>, SchemaConflict> {
    if packages.is_empty() {
        return Ok(Natives::standard());
    }
    let libraries: Vec<&'static NativeLibrary> = packages
        .iter()
        .flat_map(|p| p.libraries.iter().copied())
        .collect();
    Natives::shared(&libraries)
}

#[cfg(test)]
mod tests {
    use super::*;

    static RESPONSE_METHODS: [NativeFunction; 2] = [
        viso_behavior::native!(fn "status" |_cx, r: viso_behavior::native::Obj<Thing>| -> i64 { Ok(r.0) })
            .property(),
        viso_behavior::native!(fn "header" |_cx, r: viso_behavior::native::Obj<Thing>, name: String| -> Option<String> {
            Ok(Some(format!("{}{name}", r.0)))
        }),
    ];

    #[derive(Debug)]
    struct Thing(i64);

    impl viso_behavior::native::NativeObject for Thing {
        const PATH: &'static str = "net::Thing";
    }

    static LIBRARY: NativeLibrary = NativeLibrary {
        path: "net",
        version: 3,
        functions: &[
            viso_behavior::native!(task "get" |_cx, url: String, tries: Option<i64>| -> viso_behavior::native::Obj<Thing> {
                Ok(viso_behavior::native::Obj::new(Thing(url.len() as i64 + tries.unwrap_or(0))))
            })
            .requires(&["network.http"]),
            viso_behavior::native!(fn "lines" |_cx, text: String| -> Vec<String> {
                Ok(text.lines().map(str::to_owned).collect())
            })
            .deterministic(),
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
        let read = read(&text).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(write(&read).unwrap(), text);
        let (ours, theirs) = (
            Natives::with(&[&LIBRARY]).unwrap(),
            Natives::with(&read).unwrap(),
        );
        for path in [
            "net::get",
            "net::lines",
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
        assert!(std::ptr::eq(read[0], super::read(&text).unwrap()[0]));
        assert!(
            super::read("{\"format\":\"viso-natives\",\"version\":2,\"libraries\":[]}").is_err()
        );
    }

    #[test]
    fn the_packages_are_the_roots_direct_normal_dependencies_with_natives() {
        let metadata = JsonValue::parse(
            r#"{
              "packages": [
                {"id": "app", "name": "app", "manifest_path": "/w/app/Cargo.toml", "metadata": null},
                {"id": "net", "name": "viso-net", "manifest_path": "/w/net/Cargo.toml",
                 "metadata": {"viso": {"natives": {"schema": "natives.json", "library": "NATIVES", "feature": "dsl"}}}},
                {"id": "off", "name": "off", "manifest_path": "/w/off/Cargo.toml",
                 "metadata": {"viso": {"natives": {"schema": "n.json", "library": "N", "feature": "dsl"}}}},
                {"id": "dev", "name": "dev", "manifest_path": "/w/dev/Cargo.toml",
                 "metadata": {"viso": {"natives": {"schema": "n.json", "library": "N"}}}}
              ],
              "resolve": {"root": "app", "nodes": [
                {"id": "app", "deps": [
                  {"name": "net2", "pkg": "net", "dep_kinds": [{"kind": null, "target": null}]},
                  {"name": "off", "pkg": "off", "dep_kinds": [{"kind": null, "target": null}]},
                  {"name": "dev", "pkg": "dev", "dep_kinds": [{"kind": "dev", "target": null}]}
                ], "features": []},
                {"id": "net", "deps": [], "features": ["default", "dsl"]},
                {"id": "off", "deps": [], "features": []},
                {"id": "dev", "deps": [], "features": []}
              ]}
            }"#,
        )
        .unwrap();
        let schema = write(&[&LIBRARY]).unwrap();
        let found = packages(&metadata, |path| {
            assert_eq!(path, Path::new("/w/net/natives.json"));
            Ok(schema.clone())
        })
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "viso-net");
        assert_eq!(found[0].library, "::net2::NATIVES");
        assert!(found[0].holds("net::get") && !found[0].holds("network::get"));
        let natives = registry(&found).unwrap();
        assert!(
            natives.function("net::get").is_some()
                && natives.function("viso::text::upper").is_some()
        );
    }
}
