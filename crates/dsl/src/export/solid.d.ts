import type { Accessor, JSX, Setter } from "solid-js";

/** A Viso value: integers as `bigint`, floats as `number`, text as `string`,
 * nothing as `null`, records and enum variants as `{ t, f }`, lists as arrays. */
export type Value = any;

/** A mount of region content mounting a component: it keeps that
 * component's states, runs its effects and owns the tasks it starts. */
export type Mount = object;

/** How a resource loads: its state and the handler writing it, its debounce
 * and cache time in seconds, whether it caches errors and cancels a load in
 * flight when its key changes. */
export interface Load {
  s: () => Value;
  w: (value: Value) => void;
  d: number | null;
  c: number | null;
  e: boolean;
  k: boolean;
}

/** An effect: its dependency list and the state slots that list reads (null
 * for any), its body, its run policy (0 mount, 1 change, 2 both), and how a
 * resource loads. */
export type Effect = [(() => Value) | null, number[] | null, () => Value, number, Load | null];

/** How a prop reaches its input: as is, or as the Viso value it denotes. */
export type InputKind = "bool" | "int" | "float" | "string" | "value";

export interface Store {
  readonly work: Value[] | null;
  get(slot: number): Value;
  set(slot: number, value: Value): void;
  input(slot: number): Value;
  emit(event: number, args: Value[]): void;
  tr(message: Value, args: Value[]): string;
  start(
    task: (...args: Value[]) => Promise<Value>,
    args: Value[],
    done: ((value: Value) => void) | null,
    cancelled: (() => void) | null,
    instance: number,
    slot: number | null,
    policy: number,
  ): void;
  memo<T>(f: () => T): Accessor<T>;
  effect(f: () => void): void;
  at<T>(m: Mount, f: () => T): T;
  run(handler: () => void, m?: Mount): void;
  fx(effects: Effect[]): void;
  mount(
    p: Mount | null,
    own: number[],
    locals: [number, (() => Value) | null][],
    fx: ((m: Mount) => Effect[]) | null,
    render: (m: Mount) => JSX.Element,
  ): JSX.Element;
  on(el: Element, route: number, capture: boolean, control: number, handler: (payload: Value) => void, m?: Mount): void;
  anchor(el: Element, reads: [number, number][], m?: Mount): void;
  click(e: MouseEvent): Value;
  sync(el: Element, control: number, value: (() => Value) | null): void;
}

export interface Messages {
  /** Each locale's tag, the source's first. */
  Ls: string[];
  /** Each locale's patterns, by message id. */
  Ms: Value[][];
  /** The locale `settle` picked, its patterns, and the reader's language. */
  L: string;
  M: Value[];
  R?: string;
  MK: Map<string, number>;
}

/** Has `messages` speak the reader's language, as near as they can. */
export declare function settle(messages: Messages): void;

export declare function store(o: {
  states: [Accessor<Value>, Setter<Value>][];
  inputs: [() => unknown, InputKind, Value][];
  events: (() => ((...args: Value[]) => void) | undefined)[];
  messages: Messages;
  env?: [number, number][];
  assets: [string, string][];
}): Store;

/** The browser's value of the `env` field of tag `tag`, for a reader whose
 * place in the layout is not known yet. */
export declare function envField(tag: number): Value;

export declare function Each(props: {
  each: () => Value;
  by: ((item: Value) => Value) | null;
  children: (item: Accessor<Value>) => JSX.Element;
}): JSX.Element;

export declare function With(props: {
  value: () => Value;
  children: (value: Accessor<Value>) => JSX.Element;
}): JSX.Element;

export declare function Choose(props: {
  arm: () => Value;
  keep: boolean[];
  arms: (() => JSX.Element)[];
}): JSX.Element;

export declare const show: {
  text(v: Value): string;
  truth(v: Value): boolean;
  color(v: Value): string;
  opt(v: Value): string | undefined;
  num(v: Value): string | undefined;
  url(v: Value): string;
  fit(v: Value): string;
  mode(v: Value): string | undefined;
  pattern(v: Value): string | undefined;
  cursor(v: Value): string | undefined;
  number(el: HTMLInputElement, v: Value): void;
  maskSize(v: Value): string;
  mask(href: string): string;
  opacity(v: Value): string;
  radius(v: Value): string;
  stroke(v: Value): string;
  offset(v: Value): string;
  scale(v: Value): string;
  rotate(v: Value): string;
  origin(v: Value, rtl: boolean): string;
  focusable(el: HTMLElement, v: Value): void;
  enabled(el: HTMLElement, v: Value): void;
  autofocus(el: HTMLElement, v: Value): void;
  reaches(e: Event): boolean;
  move(v: Value, property: string): string;
  select(el: Element, v: Value, radio: boolean): void;
};

export declare const $: any;
export declare function fault(message: string): Error;
