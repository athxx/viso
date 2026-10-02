//! The native registry: the libraries a compiler resolves native paths against
//! and an interpreter links to.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

use super::{
    NativeFunction, NativeId, NativeLibrary, NativeTrait, NativeType, NativeWidget, standard,
};

/// A registered native function or handle method.
#[derive(Debug, Clone)]
pub struct NativeEntry {
    /// Its full path: `viso::text::upper`, `viso::time::Stopwatch::elapsed_ms`.
    pub path: Box<str>,
    /// Its stable identity.
    pub id: NativeId,
    /// Its schema and implementation.
    pub function: &'static NativeFunction,
    /// The library declaring it.
    pub library: &'static NativeLibrary,
    /// The handle type it is a method of.
    pub owner: Option<NativeId>,
}

impl NativeEntry {
    /// Whether it is a method called on a handle: its first parameter is the
    /// owning type.
    pub fn is_method(&self, natives: &Natives) -> bool {
        let Some(owner) = self.owner.and_then(|id| natives.ty_by_id(id)) else {
            return false;
        };
        matches!(
            self.function.params.first().map(|p| p.ty),
            Some(super::SchemaTy::Handle(path)) if path == &*owner.path
        )
    }
}

/// A registered native handle type.
#[derive(Debug, Clone)]
pub struct NativeTypeEntry {
    /// Its full path, such as `viso::time::Stopwatch`.
    pub path: Box<str>,
    /// Its stable identity.
    pub id: NativeId,
    /// Its schema.
    pub ty: &'static NativeType,
    /// The library declaring it.
    pub library: &'static NativeLibrary,
}

/// A registered scheduler trait.
#[derive(Debug, Clone)]
pub struct NativeTraitEntry {
    /// Its full path, such as `viso::game::FixedUpdate`.
    pub path: Box<str>,
    /// Its stable identity.
    pub id: NativeId,
    /// Its schema.
    pub native_trait: &'static NativeTrait,
    /// The library declaring it.
    pub library: &'static NativeLibrary,
}

impl NativeTraitEntry {
    /// The identity of its hook `name`: [`NativeId::of`] the hook's full path,
    /// `viso::game::FixedUpdate::fixed_update`.
    pub fn hook_id(&self, name: &str) -> NativeId {
        NativeId::of(&format!("{}::{name}", self.path))
    }
}

/// A registered widget.
#[derive(Debug, Clone, Copy)]
pub struct NativeWidgetEntry {
    /// Its declaration.
    pub widget: &'static NativeWidget,
    /// The library declaring it.
    pub library: &'static NativeLibrary,
}

/// Two schemas for one native path (`E6101`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaConflict {
    /// The path both schemas declare.
    pub path: String,
    /// What conflicts.
    pub message: String,
}

impl SchemaConflict {
    /// The stable diagnostic code.
    pub fn code(&self) -> &'static str {
        "E6101"
    }
}

impl fmt::Display for SchemaConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "E6101: {}", self.message)
    }
}

impl std::error::Error for SchemaConflict {}

#[derive(Debug, Clone, Copy)]
enum Item {
    Function(u32),
    Type(u32),
    Trait(u32),
}

/// A set of native libraries, one version per library path, with every
/// function, method, handle type and trait addressable by path and by
/// [`NativeId`], and every widget by its type name.
#[derive(Debug, Default)]
pub struct Natives {
    libraries: Vec<&'static NativeLibrary>,
    functions: Vec<NativeEntry>,
    types: Vec<NativeTypeEntry>,
    traits: Vec<NativeTraitEntry>,
    ids: HashMap<NativeId, Item>,
    widgets: Vec<NativeWidgetEntry>,
    widget_names: HashMap<&'static str, u32>,
}

impl Natives {
    /// An empty registry.
    pub fn new() -> Natives {
        Natives::default()
    }

    /// The standard libraries, one registry shared by every caller.
    pub fn standard() -> Arc<Natives> {
        static STANDARD: OnceLock<Arc<Natives>> = OnceLock::new();
        STANDARD
            .get_or_init(|| {
                let mut natives = Natives::new();
                natives
                    .extend(standard::STANDARD)
                    .expect("the standard libraries do not conflict");
                Arc::new(natives)
            })
            .clone()
    }

    /// A registry of the standard libraries and `libraries`.
    ///
    /// # Errors
    ///
    /// A [`SchemaConflict`] if two libraries declare one path differently.
    pub fn with(libraries: &[&'static NativeLibrary]) -> Result<Natives, SchemaConflict> {
        let mut natives = Natives::new();
        natives.extend(standard::STANDARD)?;
        natives.extend(libraries)?;
        Ok(natives)
    }

    /// Registers every library of `libraries`.
    ///
    /// # Errors
    ///
    /// As [`Natives::register`]; the libraries before the conflicting one stay
    /// registered.
    pub fn extend(&mut self, libraries: &[&'static NativeLibrary]) -> Result<(), SchemaConflict> {
        libraries.iter().try_for_each(|l| self.register(l))
    }

    /// Registers `library`. Registering the same library again does nothing.
    ///
    /// # Errors
    ///
    /// A [`SchemaConflict`] if another version of its path is registered, if
    /// one of its paths is already registered with a different schema, or if
    /// one of its widget names is already declared differently; the registry
    /// is then unchanged.
    pub fn register(&mut self, library: &'static NativeLibrary) -> Result<(), SchemaConflict> {
        let conflict = |path: &str, message: String| SchemaConflict {
            path: path.to_owned(),
            message,
        };
        if let Some(other) = self.libraries.iter().find(|l| l.path == library.path) {
            if other.version != library.version {
                return Err(conflict(
                    library.path,
                    format!(
                        "`{}` is registered at schema version {} and {}",
                        library.path, other.version, library.version
                    ),
                ));
            }
            if std::ptr::eq(*other, library) {
                return Ok(());
            }
        }
        let mut functions = Vec::new();
        let mut types = Vec::new();
        for ty in library.types {
            let path = format!("{}::{}", library.path, ty.name);
            let id = NativeId::of(&path);
            for method in ty.methods {
                let path = format!("{path}::{}", method.name);
                functions.push(NativeEntry {
                    id: NativeId::of(&path),
                    path: path.into(),
                    function: method,
                    library,
                    owner: Some(id),
                });
            }
            types.push(NativeTypeEntry {
                path: path.into(),
                id,
                ty,
                library,
            });
        }
        let traits: Vec<NativeTraitEntry> = library
            .traits
            .iter()
            .map(|native_trait| {
                let path = format!("{}::{}", library.path, native_trait.name);
                NativeTraitEntry {
                    id: NativeId::of(&path),
                    path: path.into(),
                    native_trait,
                    library,
                }
            })
            .collect();
        for function in library.functions {
            let path = format!("{}::{}", library.path, function.name);
            functions.push(NativeEntry {
                id: NativeId::of(&path),
                path: path.into(),
                function,
                library,
                owner: None,
            });
        }
        let mut fresh: HashMap<NativeId, &str> = HashMap::new();
        let paths = functions
            .iter()
            .map(|f| (f.id, &*f.path))
            .chain(types.iter().map(|t| (t.id, &*t.path)))
            .chain(traits.iter().map(|t| (t.id, &*t.path)));
        for (id, path) in paths {
            if fresh.insert(id, path).is_some() {
                return Err(conflict(path, format!("`{path}` is declared twice")));
            }
            let Some(&item) = self.ids.get(&id) else {
                continue;
            };
            let same = match item {
                Item::Function(i) => {
                    let old = &self.functions[i as usize];
                    let new = functions.iter().find(|f| f.id == id);
                    &*old.path == path
                        && new.is_some_and(|new| {
                            old.function.kind == new.function.kind
                                && old.function.signature() == new.function.signature()
                        })
                }
                Item::Type(i) => {
                    let old = &self.types[i as usize];
                    let new = types.iter().find(|t| t.id == id);
                    &*old.path == path
                        && new.is_some_and(|new| {
                            old.ty.ownership == new.ty.ownership && old.ty.thread == new.ty.thread
                        })
                }
                Item::Trait(i) => {
                    let old = &self.traits[i as usize];
                    let new = traits.iter().find(|t| t.id == id);
                    &*old.path == path
                        && new.is_some_and(|new| old.native_trait == new.native_trait)
                }
            };
            if !same {
                return Err(conflict(
                    path,
                    format!("`{path}` is registered with two different schemas"),
                ));
            }
        }
        let mut names: HashMap<&str, ()> = HashMap::new();
        for widget in library.widgets {
            let path = format!("{}::{}", library.path, widget.name);
            if names.insert(widget.name, ()).is_some() {
                return Err(conflict(&path, format!("`{path}` is declared twice")));
            }
            if let Some(&i) = self.widget_names.get(widget.name)
                && self.widgets[i as usize].widget != widget
            {
                return Err(conflict(
                    &path,
                    format!(
                        "the widget `{}` is declared by `{}` and `{}` differently",
                        widget.name, self.widgets[i as usize].library.path, library.path
                    ),
                ));
            }
        }
        self.libraries.push(library);
        for widget in library.widgets {
            if !self.widget_names.contains_key(widget.name) {
                self.widget_names
                    .insert(widget.name, self.widgets.len() as u32);
                self.widgets.push(NativeWidgetEntry { widget, library });
            }
        }
        for f in functions {
            if !self.ids.contains_key(&f.id) {
                self.ids
                    .insert(f.id, Item::Function(self.functions.len() as u32));
                self.functions.push(f);
            }
        }
        for t in types {
            if !self.ids.contains_key(&t.id) {
                self.ids.insert(t.id, Item::Type(self.types.len() as u32));
                self.types.push(t);
            }
        }
        for t in traits {
            if !self.ids.contains_key(&t.id) {
                self.ids.insert(t.id, Item::Trait(self.traits.len() as u32));
                self.traits.push(t);
            }
        }
        Ok(())
    }

    /// Every registered library, in registration order.
    pub fn libraries(&self) -> &[&'static NativeLibrary] {
        &self.libraries
    }

    /// Every function and method, in registration order.
    pub fn functions(&self) -> &[NativeEntry] {
        &self.functions
    }

    /// Every handle type, in registration order.
    pub fn types(&self) -> &[NativeTypeEntry] {
        &self.types
    }

    /// Every scheduler trait, in registration order.
    pub fn traits(&self) -> &[NativeTraitEntry] {
        &self.traits
    }

    /// Every widget, in registration order.
    pub fn widgets(&self) -> &[NativeWidgetEntry] {
        &self.widgets
    }

    /// The widget a view names `name`.
    pub fn widget(&self, name: &str) -> Option<&'static NativeWidget> {
        let &i = self.widget_names.get(name)?;
        Some(self.widgets[i as usize].widget)
    }

    /// Whether `path` is a registered library path.
    pub fn is_library(&self, path: &str) -> bool {
        self.libraries.iter().any(|l| l.path == path)
    }

    /// The registered library at `path`.
    pub fn library(&self, path: &str) -> Option<&'static NativeLibrary> {
        self.libraries.iter().copied().find(|l| l.path == path)
    }

    /// The function or method at `path`.
    pub fn function(&self, path: &str) -> Option<&NativeEntry> {
        self.function_by_id(NativeId::of(path))
            .filter(|f| &*f.path == path)
    }

    /// The function or method `id`.
    pub fn function_by_id(&self, id: NativeId) -> Option<&NativeEntry> {
        match self.ids.get(&id)? {
            Item::Function(i) => self.functions.get(*i as usize),
            Item::Type(_) | Item::Trait(_) => None,
        }
    }

    /// The handle type at `path`.
    pub fn ty(&self, path: &str) -> Option<&NativeTypeEntry> {
        self.ty_by_id(NativeId::of(path))
            .filter(|t| &*t.path == path)
    }

    /// The handle type `id`.
    pub fn ty_by_id(&self, id: NativeId) -> Option<&NativeTypeEntry> {
        match self.ids.get(&id)? {
            Item::Type(i) => self.types.get(*i as usize),
            Item::Function(_) | Item::Trait(_) => None,
        }
    }

    /// The scheduler trait at `path`.
    pub fn native_trait(&self, path: &str) -> Option<&NativeTraitEntry> {
        self.native_trait_by_id(NativeId::of(path))
            .filter(|t| &*t.path == path)
    }

    /// The scheduler trait `id`.
    pub fn native_trait_by_id(&self, id: NativeId) -> Option<&NativeTraitEntry> {
        match self.ids.get(&id)? {
            Item::Trait(i) => self.traits.get(*i as usize),
            Item::Function(_) | Item::Type(_) => None,
        }
    }

    /// The method or associated function `name` of handle type `ty`.
    pub fn method(&self, ty: NativeId, name: &str) -> Option<&NativeEntry> {
        let ty = self.ty_by_id(ty)?;
        self.function(&format!("{}::{name}", ty.path))
    }
}
