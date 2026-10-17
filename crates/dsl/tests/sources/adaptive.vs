fn class_name(c: SizeClass) -> String {
    match c {
        SizeClass::Compact => "compact",
        SizeClass::Medium => "medium",
        SizeClass::Expanded => "expanded",
    }
}

component Panel {
    input label: String;
    view {
        Column {
            width: 300dp;
            Text { text: label + "=" + class_name(env.size_class); }
            Text {
                text: match env.constraints.max_width {
                    Option::Some(w) => label + " max=" + format("{}", w),
                    Option::None => label + " max=none",
                };
            }
        }
    }
}

export component Shell {
    state open = true;
    view {
        Column {
            Text { text: "window=" + class_name(env.size_class); }
            Text {
                text: if env.orientation == Orientation::Landscape { "landscape" } else { "portrait" };
            }
            Text { text: format("scale={}", env.text_scale); }
            AdaptiveScope {
                basis: 700dp;
                Panel { label: "based"; }
            }
            Row {
                width: 500dp;
                padding: 10dp;
                AdaptiveScope {
                    Panel { label: "scoped"; }
                }
            }
            if open {
                Panel { label: "regional"; }
            }
        }
    }
}
