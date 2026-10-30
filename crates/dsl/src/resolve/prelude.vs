// The standard types every module sees without an import: input geometry and state,
// the payloads of the standard events, the payloads of the built-in widget events and
// the adaptive environment a view reads as `env`.
// A module's own declarations and its imports shadow these names.

export record Point { x: Dp; y: Dp }
export record Offset { x: MixedLength = 0dp; y: MixedLength = 0dp }
impl Offset {
    fn zero() -> Offset { Offset {} }
}
export record SizeDp { width: Dp; height: Dp }

export record Modifiers { shift: Bool; control: Bool; alt: Bool; logo: Bool }

export enum PointerButton { primary; secondary; middle }
export record PointerButtons { primary: Bool; secondary: Bool; middle: Bool }
export enum PointerKind { mouse; touch; pen }

export enum Key {
    char(Char)
    enter
    escape
    tab
    backspace
    delete
    space
    arrow_up
    arrow_down
    arrow_left
    arrow_right
    home
    end
    page_up
    page_down
    function(U8)
    unidentified
}

export record KeyChord {
    key: Key
    primary: Bool = false
    shift: Bool = false
    alt: Bool = false
    control: Bool = false
}
export enum ShortcutScope { parent; window }

export record ClickEvent { position: Option<Point>; modifiers: Modifiers }
export record TapEvent { position: Point; pointer_kind: PointerKind }
export record LongPressEvent { position: Point; pointer_kind: PointerKind }
// `velocity` is how fast the pointer moves, per second; at `drag_end`, how
// fast it lifted.
export record DragEvent { position: Point; delta: Offset; total: Offset; velocity: Offset }
export enum SwipeDirection { left; right; up; down }
export record SwipeEvent { direction: SwipeDirection; position: Point; velocity: Offset }
// `scale` and `rotation` (clockwise) since the fingers landed, `*_delta` and
// `pan` (the focal point's travel) since the last sample.
export record PinchEvent {
    position: Point
    scale: F32
    rotation: Angle
    scale_delta: F32
    rotation_delta: Angle
    pan: Offset
}
export record PointerEvent {
    position: Point
    button: PointerButton
    buttons: PointerButtons
    pointer_kind: PointerKind
    modifiers: Modifiers
}
export record HoverEvent { position: Point; pointer_kind: PointerKind }
export record ScrollEvent { delta: Offset; modifiers: Modifiers }
export record KeyEvent { key: Key; repeat: Bool; modifiers: Modifiers }
export record FocusEvent { focus_visible: Bool }
export record ScrollChanged { offset: Offset; viewport: SizeDp; content: SizeDp }
export record VisibleRange { first: U64; last: U64; count: U64 }
// An edge of a `SwipeRow`, the way it reads, and which edges a full swipe
// runs the first action of.
export enum SwipeEdge { leading; trailing }
export enum FullSwipe { none; leading; trailing; both }
export record FullSwipeEvent { edge: SwipeEdge }
export record ReorderEvent { from: U32; to: U32 }
// The edge of the surface a `Sheet` slides in from.
export enum SheetEdge { bottom; top; leading; trailing }
// What a drop does with the data it takes. A `DropEvent`'s `kind` is the
// index among the target's `drop_accepts` of the kind it takes the data as;
// `text` is that kind's text, `files` the paths of `"files"`.
export enum DropOperation { move; copy; link }
export record DropEvent {
    position: Point
    kind: U32
    text: String
    files: List<String>
    operation: DropOperation
}
// How a drag of a node's data ended: what the target it was dropped on did,
// none for data put back.
export record DropEnd { operation: Option<DropOperation> }
// Where a `Video` is, how long it plays (none for a live stream or before it
// is ready), whether it plays and whether it waits for data to play on.
export record VideoProgress {
    position: Duration
    duration: Option<Duration>
    playing: Bool
    waiting: Bool
}

// How a move's progress follows its time: Penner's curves accelerating
// (`in_*`), decelerating (`out_*`) or both, `ease_*` the cubic ones, a CSS
// cubic Bézier through two control points, and a spring of stiffness,
// damping and mass, which takes the time it takes in place of the duration.
export enum Easing {
    linear
    ease_in
    ease_out
    ease_in_out
    in_quad
    out_quad
    in_out_quad
    in_cubic
    out_cubic
    in_out_cubic
    in_quart
    out_quart
    in_out_quart
    in_quint
    out_quint
    in_out_quint
    in_sine
    out_sine
    in_out_sine
    in_expo
    out_expo
    in_out_expo
    in_circ
    out_circ
    in_out_circ
    in_back
    out_back
    in_out_back
    in_elastic
    out_elastic
    in_out_elastic
    in_bounce
    out_bounce
    in_out_bounce
    cubic_bezier(F32, F32, F32, F32)
    spring(F32, F32, F32)
}
export enum ReducedMotion { instant; keep }
export record Transition {
    duration: Duration = 200ms
    delay: Duration = 0ms
    easing: Easing = Easing::ease_out
    reduced: ReducedMotion = ReducedMotion::instant
}
export enum Animate { translate(Offset); scale(F32); rotation(Angle); opacity(F32) }
// A stop of a `Keyframes` track: the value it reaches `at` this share of a
// cycle, eased in from the stop before.
export record Keyframe { at: F32; value: Animate; easing: Easing = Easing::linear }
export enum Repeat { times(U32); forever }
export enum PlayDirection { normal; reverse; alternate; alternate_reverse }
export record Keyframes {
    stops: List<Keyframe>
    duration: Duration = 200ms
    delay: Duration = 0ms
    repeat: Repeat = Repeat::times(1)
    direction: PlayDirection = PlayDirection::normal
    reduced: ReducedMotion = ReducedMotion::instant
}
export record AnimationEnd { target: Animate; finished: Bool }

export record ToggleChanged { value: Bool }
export record SliderChanged { value: F32 }
export record ColorChanged { value: Color }
export record TextChanged { value: String }
export record NumberChanged { value: F64 }
export record LinkEvent { url: String }
export record SelectionChanged { value: U32 }
// A system back action about to pop a `RouterView`'s page.
export record BackRequest {}

export record Rect { x: Dp; y: Dp; width: Dp; height: Dp }
export record Insets { top: Dp; right: Dp; bottom: Dp; left: Dp }

// A box's insets: `start` and `end` are the edges a line begins and ends at in
// the layout direction. A length where an `EdgeInsets` is wanted is `all` of it.
export record EdgeInsets {
    top: MixedLength = 0dp
    end: MixedLength = 0dp
    bottom: MixedLength = 0dp
    start: MixedLength = 0dp
}
impl EdgeInsets {
    fn zero() -> EdgeInsets { EdgeInsets {} }
    fn all(v: MixedLength) -> EdgeInsets { EdgeInsets { top: v, end: v, bottom: v, start: v } }
    fn axes(inline: MixedLength, block: MixedLength) -> EdgeInsets {
        EdgeInsets { top: block, end: inline, bottom: block, start: inline }
    }
}

export record WindowMetrics { logical_size: SizeDp; scale_factor: F32 }
export record LocalConstraints {
    min_width: Dp
    max_width: Option<Dp>
    min_height: Dp
    max_height: Option<Dp>
}
export enum SizeClass { Compact; Medium; Expanded }
export enum Orientation { Portrait; Landscape }
export record KeyboardInset { height: Dp }
export enum DisplayFeature {
    Hinge { bounds: Rect }
    Fold { bounds: Rect }
    Cutout { bounds: Rect }
}
export enum PointerPrecision { fine; coarse; unavailable }
export record InputCapabilities {
    primary_pointer_precision: PointerPrecision
    hover_available: Bool
    keyboard_available: Bool
    touch_available: Bool
    pen_available: Bool
    gamepad_available: Bool
}
export enum LayoutDirection { ltr; rtl }
export record Locale { tag: String }
// A day of the Gregorian calendar, a time of day, and the two together: what
// a message's `{d, date}` and `{t, time}` write in the reader's locale.
export record Date { year: I32; month: U32; day: U32 }
export record Time { hour: U32 = 0; minute: U32 = 0; second: U32 = 0 }
export record DateTime { date: Date; time: Time }

// Assets: only `asset("…")` makes an `ImageAsset` (a raster), which an `Image`
// shows, an `SvgAsset`, which an `Svg` shows, or a `VideoAsset` (MP4,
// QuickTime, WebM, Matroska), which a `Video` plays.
export record ImageAsset { resource: I64 }
export record SvgAsset { resource: I64 }
export record VideoAsset { resource: I64 }
export enum ImageFit { contain; cover; fill; tile }
// Which way a `CameraView`'s camera faces: toward the user, away, or
// wherever it is put.
export enum CameraFacing { front; back; external }

// The layout schema: an axis's sizing (a length stands for `fixed` of it),
// a grid's tracks and a box's border.
export enum Sizing {
    fixed(MixedLength)
    fill { weight: F32 = 1.0 }
    fit
    min_content
    max_content
}
export enum Track {
    fixed(MixedLength)
    fr(F32)
    auto
    minmax(MixedLength, TrackMax)
    fit_content(MixedLength)
}
export enum TrackMax { length(MixedLength); fr(F32) }
export enum AutoRepeat { fill; fit }
export record AdaptiveColumns {
    mode: AutoRepeat = AutoRepeat::fill
    min: MixedLength
    max: TrackMax = TrackMax::fr(1.0)
}
export record Border { width: Dp = 1dp; color: Color }
export record Shadow {
    x: Dp = 0dp
    y: Dp = 2dp
    blur: Dp = 8dp
    spread: Dp = 0dp
    color: Color
    inset: Bool = false
}
// What a box does to the content behind it, as glass: blurs it by `blur`
// (as CSS's blur radius), scales its saturation by `saturation`, lays
// `tint` over it by the tint's alpha, and adds a grain of `noise` (0..1).
export record Backdrop {
    blur: Dp = 30dp
    saturation: F32 = 1.8
    tint: Color = #ffffff40
    noise: F32 = 0.0
}

// The values of the standard widgets' enum properties.
export enum Axis { row; column }
export enum Snap { start; center; end; pages }
// How a navigation stack's pages trade places: `native` as this platform's
// own navigation does.
export enum PageTransition { native; slide; shared_axis; fade; none }
export enum Justify { start; center; end }
export enum Align { start; center; end; stretch }
export enum GridAlign { stretch; start; center; end }
export enum Alignment2D {
    top_start
    top
    top_end
    start
    center
    end
    bottom_start
    bottom
    bottom_end
}
export enum FontWeight { thin; light; regular; medium; semibold; bold; heavy }
export enum FontFamily { system_ui; serif; monospace; named(String) }
export enum TextOverflow { clip; ellipsis }
export enum TextAlign { start; center; end }
export enum FontStyle { normal; italic }
export record Span {
    text: String
    color: Option<Color> = None
    background: Option<Color> = None
    font_weight: Option<FontWeight> = None
    font_style: Option<FontStyle> = None
    font_family: Option<FontFamily> = None
    font_size: Option<MixedLength> = None
    raise: Option<MixedLength> = None
    underline: Bool = false
    strikethrough: Bool = false
    link: Option<String> = None
}

export enum LiveRegion { off; polite; assertive }
export enum InputKind { text; digits; integer; decimal; phone; email; url; search }
export enum Cursor {
    default
    pointer
    text
    vertical_text
    crosshair
    move
    grab
    grabbing
    not_allowed
    wait
    progress
    help
    context_menu
    copy
    alias
    zoom_in
    zoom_out
    resize_ew
    resize_ns
    resize_nesw
    resize_nwse
    resize_col
    resize_row
    hidden
}
export enum Role {
    group
    button
    check_box
    slider
    radio
    label
    image
    text_field
    tab
    tab_list
    navigation
    dialog
    status
    region
    tree
    tree_item
    heading
    list
    list_item
    secure_field
    spin_button
    menu
    menu_item
    link
    menu_item_check_box
    menu_item_radio
    menu_bar
    separator
    list_box
    list_option
    combo_box
    tooltip
    table
    row
    cell
    column_header
    editable_combo_box
    progress_indicator
    search_field
}

export record Environment {
    window: WindowMetrics
    constraints: LocalConstraints
    size_class: SizeClass
    safe_area: Insets
    keyboard_inset: KeyboardInset
    display_features: List<DisplayFeature>
    input: InputCapabilities
    text_scale: F32
    reduced_motion: Bool
    orientation: Orientation
    layout_direction: LayoutDirection
    locale: Locale
}

// The standard theme schema a view reads as `theme`.
export record ColorPalette {
    background: Color
    foreground: Color
    surface: Color
    on_surface: Color
    primary: Color
    on_primary: Color
    primary_hover: Color
    accent: Color
    muted: Color
    outline: Color
    error: Color
    on_error: Color
    focus_ring: Color
    scrim: Color
}
export record TypographyScale {
    base_size: Sp = 14sp
    caption_size: Em = 0.85em
    title_size: Em = 1.25em
    headline_size: Em = 1.6em
    family: FontFamily = FontFamily::system_ui
}
export record SpacingScale {
    xsmall: Dp = 2dp
    small: Dp = 4dp
    medium: Dp = 8dp
    large: Dp = 16dp
    xlarge: Dp = 24dp
}
export record RadiusScale { small: Dp = 4dp; medium: Dp = 8dp; large: Dp = 12dp }
export record ElevationScale { low: Shadow; medium: Shadow; high: Shadow }
export record MotionScale {
    short: Duration = 100ms
    medium: Duration = 200ms
    long: Duration = 350ms
    standard: Easing = Easing::ease_out
    emphasized: Easing = Easing::ease_in_out
}
export record Theme {
    colors: ColorPalette
    typography: TypographyScale = TypographyScale {}
    spacing: SpacingScale = SpacingScale {}
    radius: RadiusScale = RadiusScale {}
    elevation: ElevationScale
    motion: MotionScale = MotionScale {}
}

// The traits the compiler implements for every type its rule admits (§79):
// every value is `Clone`; every value but a function or a trait object is
// `Eq`; what is `Eq` and holds no float is `Hash`.
export trait Clone {}
export trait Eq {}
export trait Hash {}

// What `bind .. using C` converts through (§51): `to_view` shows the model's
// value, and `to_model` reads back what the view holds, rejecting the update
// with `None`.
export trait TwoWayConverter<Model, View> {
    fn to_view(model: Model) -> View
    fn to_model(view: View) -> Option<Model>
}
