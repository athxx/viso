# Exporting to web projects

`viso export` turns a view into source code that a web project owns: plain HTML and
CSS, or a SolidJS component. It is one-way — the export is generated from your
`.vs` source and is not read back — and you can copy it out and maintain it on its
own.

```bash
viso export html           # dist-html/: index.html, styles.css, app.js
viso export solid          # web-solid/: a Vite project with a SolidJS component
```

Both export the component `src/app.vs` exports, or the one you name, with its
inputs at their defaults:

```bash
viso export html ui::Card --out card
viso export solid ui::Card --out card-solid
```

## What exports

The exporter reads what the compiler made of your view — its nodes, styles,
behavior and resources — and classifies everything before writing anything. Each
part either maps onto the web directly (a `Button` is a `<button>`, a column is a
flex box, a label is its accessible name), maps through a small generated helper, or
cannot be exported. Nothing is silently dropped: what cannot be exported is an error
at its line, naming the node, what it needs and what to do instead.

```text
error[EXPORT_HTML_GPU_ONLY_NODE]: a `Ripple` paints with a shader, which no HTML element runs
 --> src/app.vs:14:13
   |
14 |             Ripple { width: 120dp; height: 120dp; }
   |             ^^^^^^
 = note: build for the browser with the Viso renderer: `viso build web-gpu`, or with DOM and GPU islands: `viso build web-hybrid`
 = note: or rewrite the node with widgets HTML has
```

Exported behavior includes states, actions and event handlers, computed values,
lists and conditionals, components a region mounts, tasks, effects and resources,
the theme, localized text in every locale with the nearest to the browser's
languages picked at load, and the adaptive environment — window size, size classes,
safe areas, reduced motion — as the browser reports it.

Game systems do not export.

## `viso export html`

```bash
viso export html
viso export html --static          # no JavaScript; fail on anything that needs it
viso export html --out site
```

The output is semantic HTML, CSS with classes and variables, and — only when the
view changes at run time — `app.js`, plain JavaScript that updates the elements
directly. It depends on no framework. The assets the page shows are copied under
`assets/`. The same source gives the same bytes.

`--static` is for pages that must work without scripts: a view that needs JavaScript
to change is an error (`EXPORT_HTML_DYNAMIC_REQUIRED`) instead.

## `viso export solid`

```bash
viso export solid
cd web-solid
npm install
npm run dev
```

```text
web-solid/
├── package.json        solid-js, vite, vite-plugin-solid, typescript
├── tsconfig.json
├── vite.config.ts
├── index.html
└── src/
    ├── index.tsx
    ├── App.tsx                          mounts the exported component
    ├── components/<Name>.tsx            JSX, signals, props
    ├── components/<Name>.behavior.ts    the behavior, as TypeScript
    ├── viso/runtime.js (+ .d.ts)        value semantics and the store
    ├── styles.css
    └── assets/
```

A component's inputs become its props. Viso does not run the package manager;
`node_modules/`, `dist/` and the lockfile are left alone when you export again.
Vite needs Node.js 20.19 or newer. When it is done, `viso export solid` prints the
commands to run next, for the package manager `export.solid.package_manager` names
(`npm`, `pnpm`, `yarn` or `bun`; `npm` by default).

The Solid export makes the same choices as the HTML one — the two share one lowering
— so what one supports, the other does too.

## Exporting again

The export directory is replaced as a whole: written beside it, then swapped in, so
a failure leaves the old one intact. A `.viso-generated.json` inside records each
file it wrote and its hash. If the directory holds a file the export did not write,
or one you edited since, the export stops with `EXPORT_CONFLICT` and lists them;
`--force` replaces it anyway.

```bash
viso export html --force
```

The project root, its parents, `src/` and `assets/` cannot be export directories.

## Configuration

```toml
[export.html]
out_dir = "dist-html"

[export.solid]
out_dir = "web-solid"
package_manager = "pnpm"
```

`--out` is relative to the current directory; `out_dir` to the project root.
