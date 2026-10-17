// Generated from Viso source. This directory is an export artifact.
import { createSignal } from "solid-js";
import { type Messages, settle, show, store, type Value } from "../viso/runtime";
import { behavior } from "./Counter.behavior";
import "../styles.css";

export interface CounterProps {
}

const MESSAGES: Messages = { Ls: [""], Ms: [[]], L: "", M: [], MK: new Map() };
settle(MESSAGES);

export default function Counter(_props: CounterProps) {
  const [count, setCount] = createSignal<Value>(0n, { equals: false });
  const st = store({
    states: [[count, setCount]],
    inputs: [],
    events: [],
    messages: MESSAGES,
    assets: [],
  });
  const f = behavior(st);
  return (
    <div class="vx-flex s0">
      <span class="vx-text s1">
        {show.text(f.entry1())}
      </span>
      <button
        class="vx-button s1"
        type="button"
        onClick={(e) => st.run(() => f.handler3(st.click(e)))}
      >
        {"Add"}
      </button>
    </div>
  );
}
