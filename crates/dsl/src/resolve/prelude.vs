// The standard types every module sees without an import: input geometry and state,
// the payloads of the standard events, the payloads of the built-in widget events and
// the adaptive environment a view reads as `env`.
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

export record Rect { x: Dp; y: Dp; width: Dp; height: Dp; }
export record Insets { top: Dp; right: Dp; bottom: Dp; left: Dp; }

export record WindowMetrics { logical_size: SizeDp; scale_factor: F32; }
export record LocalConstraints {
    min_width: Dp;
    max_width: Option<Dp>;
    min_height: Dp;
    max_height: Option<Dp>;
}
export enum SizeClass { Compact; Medium; Expanded; }
export enum Orientation { Portrait; Landscape; }
export record KeyboardInset { height: Dp; }
export enum DisplayFeature {
    Hinge { bounds: Rect; };
    Fold { bounds: Rect; };
    Cutout { bounds: Rect; };
}
export enum PointerPrecision { fine; coarse; unavailable; }
export record InputCapabilities {
    primary_pointer_precision: PointerPrecision;
    hover_available: Bool;
    keyboard_available: Bool;
    touch_available: Bool;
    pen_available: Bool;
    gamepad_available: Bool;
}
export enum LayoutDirection { ltr; rtl; }
export record Locale { tag: String; }

export record Environment {
    window: WindowMetrics;
    constraints: LocalConstraints;
    size_class: SizeClass;
    safe_area: Insets;
    keyboard_inset: KeyboardInset;
    display_features: List<DisplayFeature>;
    input: InputCapabilities;
    text_scale: F32;
    reduced_motion: Bool;
    orientation: Orientation;
    layout_direction: LayoutDirection;
    locale: Locale;
}
