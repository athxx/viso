// The standard types every module sees without an import: input geometry and state,
// the payloads of the standard events, and the payloads of the built-in widget events.
// A module's own declarations and its imports shadow these names.

export record Point { x: Dp; y: Dp; }
export record Offset { x: MixedLength = 0dp; y: MixedLength = 0dp; }
export record SizeDp { width: Dp; height: Dp; }

export record Modifiers { shift: Bool; control: Bool; alt: Bool; logo: Bool; }

export enum PointerButton { primary; secondary; middle; }
export record PointerButtons { primary: Bool; secondary: Bool; middle: Bool; }
export enum PointerKind { mouse; touch; pen; }

export enum Key {
    char(Char);
    enter;
    escape;
    tab;
    backspace;
    delete;
    space;
    arrow_up;
    arrow_down;
    arrow_left;
    arrow_right;
    home;
    end;
    page_up;
    page_down;
    function(U8);
    unidentified;
}

export record KeyChord {
    key: Key;
    primary: Bool = false;
    shift: Bool = false;
    alt: Bool = false;
    control: Bool = false;
}
export enum ShortcutScope { parent; window; }

export record ClickEvent { position: Option<Point>; modifiers: Modifiers; }
export record TapEvent { position: Point; pointer_kind: PointerKind; }
export record LongPressEvent { position: Point; pointer_kind: PointerKind; }
export record DragEvent { position: Point; delta: Offset; total: Offset; }
export record PointerEvent {
    position: Point;
    button: PointerButton;
    buttons: PointerButtons;
    pointer_kind: PointerKind;
    modifiers: Modifiers;
}
export record HoverEvent { position: Point; pointer_kind: PointerKind; }
export record ScrollEvent { delta: Offset; modifiers: Modifiers; }
export record KeyEvent { key: Key; repeat: Bool; modifiers: Modifiers; }
export record FocusEvent { focus_visible: Bool; }
export record ScrollChanged { offset: Offset; viewport: SizeDp; content: SizeDp; }

export enum Animate { translate(Offset); scale(F32); rotation(Angle); opacity(F32); }
export record AnimationEnd { target: Animate; finished: Bool; }

export record ToggleChanged { value: Bool; }
export record SliderChanged { value: F32; }
export record TextChanged { value: String; }
export record SelectionChanged { value: U32; }
