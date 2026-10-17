import type { Accessor, JSX, Setter } from "solid-js";

/** A Viso value: integers as `bigint`, floats as `number`, text as `string`,
 * nothing as `null`, records and enum variants as `{ t, f }`, lists as arrays. */
export type Value = any;

/** How a prop reaches its input: as is, or as the Viso value it denotes. */
export type InputKind = "bool" | "int" | "float" | "string" | "value";

export interface Store {
  readonly work: Value[] | null;
  get(slot: number): Value;
  set(slot: number, value: Value): void;
  input(slot: number): Value;
  emit(event: number, args: Value[]): void;
  tr(message: Value, args: Value[]): string;
  memo<T>(f: () => T): Accessor<T>;
  effect(f: () => void): void;
  run(handler: () => void): void;
  on(el: Element, route: number, capture: boolean, control: number, handler: (payload: Value) => void): void;
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
  assets: [string, string][];
}): Store;

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
  maskSize(v: Value): string;
  mask(href: string): string;
  opacity(v: Value): string;
  move(v: Value, property: string): string;
  select(el: Element, v: Value, radio: boolean): void;
};

export declare const $: any;
export declare function fault(message: string): Error;
