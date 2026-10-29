//! A module split in two by `wasm-split`: the primary part, the functions a
//! training run of the app called, which the page loads and starts, and the
//! lazy part, everything else.
//!
//! The glue `wasm-bindgen` generated is patched in two places: the imports it
//! instantiates the primary part with gain the placeholders `wasm-split`
//! leaves for each lazy function, and the primary part's exports are handed
//! over before the app starts. The lazy part is fetched, at low priority, as
//! the page loads and compiled in the background, then instantiated over the
//! primary part's memory and tables, which fills its functions in; a worker
//! of a threaded module loads it when it first calls into it. A call to
//! one before then lands on its placeholder, which loads the lazy part on the
//! spot (a synchronous request when its bytes are not in yet) and forwards
//! the call.
//!
//! A training run loads the instrumented module with [`training_glue`]: once
//! the app has run for [`TRAINING`], the functions it called are written into
//! memory the glue grows for them, and posted to [`PROFILE_PATH`].

use std::time::Duration;

use super::WasmReader;

/// The namespace the primary part imports its placeholders from, before the
/// `.<name>` `wasm-split` appends.
pub const NAMESPACE: &str = "lazy";

/// The namespace the lazy part imports the primary part's exports from.
pub const IMPORTS: &str = "primary";

/// The `wasm-split` options a split takes, besides its features, profile
/// and outputs.
pub const SPLIT_ARGS: &[&str] = &[
    "--export-prefix=%",
    "--placeholder-namespace=lazy",
    "--import-namespace=primary",
    "--placeholdermap",
];

/// How long a training run lets the app run before writing its profile.
pub const TRAINING: Duration = Duration::from_secs(3);

/// Where a training page posts its profile.
pub const PROFILE_PATH: &str = "/__viso_train/profile";

/// Where a training page posts what went wrong in it.
pub const LOG_PATH: &str = "/__viso_train/log";

/// The memory a training page grows to write the profile into: 64 pages,
/// room for a million functions.
const PROFILE_PAGES: u32 = 64;

/// What the patched glue needs of a split module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lazy {
    /// The lazy part's file, beside the glue.
    pub module: String,
    /// The namespace the primary part imports its placeholders from.
    pub namespace: String,
    /// The primary part's export of the table the placeholders sit in.
    pub table: String,
}

impl Lazy {
    /// The split of `primary`, whose placeholder map `wasm-split` wrote as
    /// `placeholders`, its lazy part the file `module`.
    ///
    /// # Errors
    ///
    /// A map without its table, or a module that does not export it or
    /// import placeholders.
    pub fn read(primary: &[u8], placeholders: &str, module: String) -> Result<Lazy, String> {
        let table = placeholders
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("table "))
            .and_then(|index| index.trim().parse::<u32>().ok())
            .ok_or("the placeholder map names no table")?;
        let unreadable = || "the split module does not read as a module".to_owned();
        let exports = WasmReader::exports(primary).ok_or_else(unreadable)?;
        let table = exports
            .into_iter()
            .find(|(_, kind, index)| *kind == 0x01 && *index == table)
            .map(|(name, _, _)| name)
            .ok_or_else(|| format!("the split module does not export its table {table}"))?;
        let namespace = WasmReader::import_modules(primary)
            .ok_or_else(unreadable)?
            .into_iter()
            .find(|module| {
                module == NAMESPACE
                    || module
                        .strip_prefix(NAMESPACE)
                        .is_some_and(|rest| rest.starts_with('.'))
            })
            .ok_or("the split module imports no placeholders")?;
        Ok(Lazy {
            module,
            namespace,
            table,
        })
    }
}

/// The glue `glue` loading a split module's primary part, with `lazy`'s
/// loader.
///
/// # Errors
///
/// Glue this does not know how to patch: another `wasm-bindgen`'s.
pub fn glue(glue: &str, lazy: &Lazy) -> Result<String, String> {
    const IMPORTS_FN: &str = "function __wbg_get_imports(";
    if glue.matches(IMPORTS_FN).count() != 1 {
        return Err(unknown_glue("one `__wbg_get_imports`"));
    }
    let start = start_line(glue)?;
    let mut patched = String::with_capacity(glue.len() + LOADER.len() + 256);
    for line in glue.split_inclusive('\n') {
        if line.starts_with(IMPORTS_FN) {
            // The generated function keeps its body under another name.
            patched.push_str(
                "function __wbg_get_imports(...args) {\n    return __visoLazy.imports(__wbg_get_imports_split(...args));\n}\n",
            );
            patched.push_str(&line.replacen("__wbg_get_imports(", "__wbg_get_imports_split(", 1));
        } else if std::ptr::eq(line, start) {
            let indent = &line[..line.len() - line.trim_start().len()];
            patched.push_str(indent);
            patched.push_str("__visoLazy.ready(wasm);\n");
            patched.push_str(line);
        } else {
            patched.push_str(line);
        }
    }
    patched.push_str(
        &LOADER
            .replace("{module}", &js_string(&lazy.module))
            .replace("{namespace}", &js_string(&lazy.namespace))
            .replace("{table}", &js_string(&lazy.table)),
    );
    Ok(patched)
}

/// The glue `glue` loading an instrumented module, which posts the profile
/// of a training run.
///
/// # Errors
///
/// Glue whose start this does not find.
pub fn training_glue(glue: &str) -> Result<String, String> {
    start_line(glue)?;
    Ok(format!(
        "{glue}{}",
        TRAINER
            .replace("{wait}", &TRAINING.as_millis().to_string())
            .replace("{pages}", &PROFILE_PAGES.to_string())
            .replace("{profile}", PROFILE_PATH)
            .replace("{log}", LOG_PATH)
    ))
}

/// The one line of `glue` starting the app.
fn start_line(glue: &str) -> Result<&str, String> {
    let mut starts = glue
        .split_inclusive('\n')
        .filter(|line| line.contains("wasm.__wbindgen_start("));
    match (starts.next(), starts.next()) {
        (Some(line), None) => Ok(line),
        _ => Err(unknown_glue("one `wasm.__wbindgen_start` call")),
    }
}

fn unknown_glue(missing: &str) -> String {
    format!("the `wasm-bindgen` glue does not have {missing}; the module cannot be split")
}

/// `text` as the contents of a double-quoted JavaScript string.
fn js_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The lazy part's loader, appended to the glue.
const LOADER: &str = r#"
const __visoLazy = (() => {
    const url = new URL("./{module}", import.meta.url);
    let compiled, instance, primary;
    const settle = () => {
        if (instance === undefined && compiled !== undefined && primary !== undefined) {
            instance = new WebAssembly.Instance(compiled, { primary });
        }
    };
    const fetched = () => {
        const request = new XMLHttpRequest();
        request.open("GET", url, false);
        request.overrideMimeType("text/plain; charset=x-user-defined");
        request.send();
        if (request.status !== 200) {
            throw new Error(`the lazy module ${url} answered ${request.status}`);
        }
        const text = request.responseText;
        const bytes = new Uint8Array(text.length);
        for (let i = 0; i < text.length; i++) bytes[i] = text.charCodeAt(i);
        return bytes;
    };
    // A worker of a threaded module loads it when it first calls into it.
    if (typeof window === "object") {
        WebAssembly.compileStreaming(fetch(url, { priority: "low" })).then(
            (module) => {
                compiled ??= module;
                try {
                    settle();
                } catch (error) {
                    console.warn("the lazy module did not instantiate:", error);
                }
            },
            (error) => console.warn("the lazy module did not load ahead:", error),
        );
    }
    const load = () => {
        if (instance === undefined) {
            compiled ??= new WebAssembly.Module(fetched());
            settle();
        }
    };
    return {
        imports(imports) {
            imports["{namespace}"] = new Proxy({}, {
                get: (_, index) => (...args) => {
                    load();
                    return primary["{table}"].get(Number(index))(...args);
                },
            });
            return imports;
        },
        ready(exports) {
            primary = exports;
            if (compiled !== undefined) settle();
        },
    };
})();
"#;

/// A training page's profile writer, appended to the glue.
const TRAINER: &str = r#"
(async () => {
    const post = (path, body) => fetch(path, { method: "POST", body });
    addEventListener("error", (e) => post("{log}", `error: ${e.message}`));
    addEventListener("unhandledrejection", (e) => post("{log}", `rejection: ${e.reason?.stack ?? e.reason}`));
    while (wasm === undefined) await new Promise((done) => setTimeout(done, 20));
    await new Promise((done) => setTimeout(done, {wait}));
    const at = wasm.memory.grow({pages}) * 65536;
    const size = wasm.__write_profile(at, {pages} * 65536);
    await post("{profile}", new Uint8Array(wasm.memory.buffer, at, size).slice());
})().catch((e) => fetch("{log}", { method: "POST", body: `training: ${e?.stack ?? e}` }));
"#;

#[cfg(test)]
mod tests {
    use super::*;

    const GLUE: &str = "function __wbg_get_imports() {\n    return { wbg: {} };\n}\nlet wasm;\nfunction __wbg_finalize_init(instance, module) {\n    wasm = instance.exports;\n    wasm.__wbindgen_start();\n    return wasm;\n}\n";

    fn lazy() -> Lazy {
        Lazy {
            module: "app_lazy.0a1b.wasm".to_owned(),
            namespace: "lazy.deferred".to_owned(),
            table: "%ln".to_owned(),
        }
    }

    #[test]
    fn the_glue_hands_the_placeholders_in_and_the_exports_over() {
        let patched = glue(GLUE, &lazy()).unwrap();
        assert!(patched.starts_with(
            "function __wbg_get_imports(...args) {\n    return __visoLazy.imports(__wbg_get_imports_split(...args));\n}\nfunction __wbg_get_imports_split() {\n"
        ));
        assert!(patched.contains("    __visoLazy.ready(wasm);\n    wasm.__wbindgen_start();\n"));
        assert!(patched.contains(r#"new URL("./app_lazy.0a1b.wasm", import.meta.url)"#));
        assert!(patched.contains(r#"imports["lazy.deferred"]"#));
        assert!(patched.contains(r#"primary["%ln"].get(Number(index))"#));
        assert!(
            !["{module}", "{namespace}", "{table}"]
                .iter()
                .any(|k| patched.contains(k))
        );
    }

    #[test]
    fn glue_of_another_shape_is_refused() {
        let twice = format!("{GLUE}{GLUE}");
        assert!(
            glue(&twice, &lazy())
                .unwrap_err()
                .contains("cannot be split")
        );
        assert!(glue("let wasm;\n", &lazy()).is_err());
        assert!(training_glue("function __wbg_get_imports() {}\n").is_err());
    }

    #[test]
    fn the_training_glue_posts_the_profile() {
        let trained = training_glue(GLUE).unwrap();
        assert!(trained.starts_with(GLUE));
        assert!(trained.contains("setTimeout(done, 3000)"));
        assert!(trained.contains("wasm.memory.grow(64)"));
        assert!(trained.contains(r#"post("/__viso_train/profile""#));
    }

    /// A module importing `imports` (module, field) functions of type 0 and
    /// exporting `tables` (name, index).
    fn module(imports: &[(&str, &str)], tables: &[(&str, u8)]) -> Vec<u8> {
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        let name = |out: &mut Vec<u8>, text: &str| {
            out.push(text.len() as u8);
            out.extend_from_slice(text.as_bytes());
        };
        let mut section = vec![imports.len() as u8];
        for (module, field) in imports {
            name(&mut section, module);
            name(&mut section, field);
            section.extend_from_slice(&[0x00, 0x00]);
        }
        wasm.extend_from_slice(&[2, section.len() as u8]);
        wasm.extend_from_slice(&section);
        let mut section = vec![tables.len() as u8];
        for (export, index) in tables {
            name(&mut section, export);
            section.extend_from_slice(&[0x01, *index]);
        }
        wasm.extend_from_slice(&[7, section.len() as u8]);
        wasm.extend_from_slice(&section);
        wasm
    }

    #[test]
    fn a_split_names_its_placeholders_and_their_table() {
        let primary = module(
            &[("./app_bg.js", "__wbg_now"), ("lazy.deferred", "0")],
            &[("%a", 0), ("%ln", 1)],
        );
        let read = Lazy::read(&primary, "table 1\n0:3804\n", "app_lazy.wasm".to_owned()).unwrap();
        assert_eq!(
            read,
            Lazy {
                module: "app_lazy.wasm".to_owned(),
                namespace: "lazy.deferred".to_owned(),
                table: "%ln".to_owned(),
            }
        );
        assert!(Lazy::read(&primary, "table 2\n", String::new()).is_err());
        assert!(Lazy::read(&primary, "0:1\n", String::new()).is_err());
        let plain = module(&[("./app_bg.js", "f"), ("lazyish", "0")], &[("%a", 0)]);
        assert!(Lazy::read(&plain, "table 0\n", String::new()).is_err());
    }
}
