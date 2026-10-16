// Generated from Viso source. This directory is an export artifact.
import { $, type Store } from "../viso/runtime";

/** `Counter`'s functions, translated from its Behavior IR, over its store. */
export function behavior(st: Store) {
  function entry1(): any {
    let r0: any, r1: any;
    r0 = st.get(0);
    r1 = $.show("signed", r0);
    return r1;
  }
  function handler3(r0: any): any {
    let r1: any, r2: any, r3: any, r4: any;
    r1 = 1n;
    r2 = st.get(0);
    r3 = $.ck("i64", r2 + r1);
    st.set(0, r3);
    r4 = 0n;
    return r4;
  }
  return { entry1, handler3 };
}
