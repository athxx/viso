import viso::math::Vec2F32

// Three soft lights drifting over a deep blue, across diagonal stripes: what
// the glass bends, frosts and tints. Reading `time` keeps it moving.
shader Aurora {
    fragment () -> ColorLinear {
        let t = time * 0.3
        let aspect = rect_size.x / rect_size.y
        let p = Vec2F32(uv.x * aspect, uv.y)
        let a = Vec2F32((0.30 + 0.18 * sin(t)) * aspect, 0.35 + 0.20 * cos(t * 1.3))
        let b = Vec2F32((0.65 + 0.20 * cos(t * 0.7)) * aspect, 0.60 + 0.25 * sin(t * 0.9))
        let c = Vec2F32((0.50 + 0.30 * sin(t * 0.5 + 2.0)) * aspect, 0.25 + 0.15 * sin(t * 1.7))
        let la = exp(-9.0 * pow(distance(p, a), 2.0))
        let lb = exp(-7.0 * pow(distance(p, b), 2.0))
        let lc = exp(-11.0 * pow(distance(p, c), 2.0))
        let along = (uv.x * rect_size.x + uv.y * rect_size.y) / 36.0
        let stripe = 0.06 * step(0.5, fract(along))
        let r = 0.03 + stripe + 0.95 * la + 0.10 * lb + 1.00 * lc
        let g = 0.05 + stripe + 0.20 * la + 0.70 * lb + 0.55 * lc
        let bl = 0.14 + stripe + 0.60 * la + 0.95 * lb + 0.08 * lc
        return ColorLinear(r, g, bl, 1.0)
    }
}

component Materials {
    state moving = true

    view {
        Row {
            // Drawn by nothing: the window's material shows through.
            Column {
                width: 220dp
                padding: EdgeInsets { start: 20dp, top: 44dp, end: 20dp, bottom: 20dp }
                gap: 12dp
                Text("Materials") { font_size: 22sp }
                Text("This sidebar is the window's own material: vibrancy on macOS, Mica on Windows 11, the theme's background elsewhere.") {
                    width: fill
                    soft_wrap: true
                    font_size: 13sp
                }
                Row {
                    gap: 8dp
                    align: Align::center
                    Toggle { bind checked <=> moving; semantics.label: "Moving picture" }
                    Text("Moving picture") { font_size: 13sp }
                }
            }
            Stack {
                width: fill
                height: fill
                clip: true
                if moving {
                    ShaderView { shader: Aurora {}; width: fill; height: fill }
                } else {
                    Column {
                        width: fill
                        height: fill
                        background: [
                            #0a1030,
                            Fill::radial { x: 0.3, y: 0.35, colors: [#f040a0, #f040a000] },
                            Fill::radial { x: 0.7, y: 0.6, colors: [#20b0f0, #20b0f000] },
                        ]
                    }
                }
                Grid {
                    width: fill
                    height: fill
                    padding: 28dp
                    columns: [Track::fr(1.0), Track::fr(1.0), Track::fr(1.0)]
                    rows: [Track::fr(1.0), Track::fr(1.0)]
                    column_gap: 24dp
                    row_gap: 24dp

                    // Skeuomorphism: a leather panel, stitched, raised off
                    // the picture, its title pressed in.
                    Column {
                        width: fill
                        height: fill
                        padding: 18dp
                        corner_radius: 16dp
                        background: [
                            #6b4426,
                            Fill::linear { colors: [#ffffff26, #00000033] },
                        ]
                        border: Border { width: 1dp, color: #2e1a0c }
                        shadow: [
                            Shadow { y: 8dp, blur: 18dp, color: #00000080 },
                            Shadow { y: 1dp, blur: 0dp, color: #ffffff38, inset: true },
                            Shadow { blur: 14dp, spread: -4dp, color: #00000066, inset: true },
                        ]
                        Text("Skeuomorphism") {
                            color: #f3dfc4
                            font_size: 17sp
                            text_shadow: TextShadow { y: -1dp, blur: 0dp, color: #00000099 }
                        }
                        Column {
                            width: fill
                            height: fill
                            margin: EdgeInsets { top: 12dp }
                            corner_radius: 10dp
                            border: Border { width: 1dp, color: #e8c89a55 }
                        }
                    }

                    // Aqua: a gel button, lit from inside, a gloss on its
                    // upper half, over brushed metal.
                    Column {
                        width: fill
                        height: fill
                        padding: 18dp
                        gap: 18dp
                        corner_radius: 16dp
                        background: [
                            #d9dce1,
                            Fill::linear { colors: [#f6f7f9, #c3c7ce] },
                        ]
                        shadow: Shadow { y: 6dp, blur: 16dp, color: #00000059 }
                        Text("Aqua") {
                            color: #2b2f36
                            font_size: 17sp
                            text_shadow: TextShadow { y: 1dp, blur: 0dp, color: #ffffffcc }
                        }
                        Row {
                            width: fill
                            height: 40dp
                            corner_radius: 20dp
                            justify: Justify::center
                            align: Align::center
                            background: [
                                #1666d8,
                                Fill::linear { colors: [#6fb0ff, #1666d8, #0a4cb0], stops: [0.0, 0.55, 1.0] },
                                Fill::linear {
                                    colors: [#ffffffd9, #ffffff26],
                                    inset: EdgeInsets { top: 2dp, start: 10dp, end: 10dp, bottom: 20dp },
                                },
                                Fill::radial { x: 0.5, y: 1.0, colors: [#8fd0ff99, #8fd0ff00] },
                            ]
                            border: Border { width: 1dp, color: #0a3d8c }
                            shadow: Shadow { y: 3dp, blur: 6dp, color: #0a4cb080 }
                            Text("Continue") {
                                color: #ffffff
                                font_size: 14sp
                                text_shadow: TextShadow { y: 1dp, blur: 1dp, color: #00000066 }
                            }
                        }
                    }

                    // Background blur: the picture behind, blurred, nothing
                    // else.
                    BlurView {
                        width: fill
                        height: fill
                        padding: 18dp
                        corner_radius: 16dp
                        backdrop: Option::Some(Backdrop { blur: 24dp, saturation: 1.0, tint: #00000000 })
                        Text("Background blur") { color: #ffffff; font_size: 17sp }
                    }

                    // Glassmorphism: frosted, brighter, a light rim and a soft
                    // shadow.
                    BlurView {
                        width: fill
                        height: fill
                        padding: 18dp
                        corner_radius: 16dp
                        backdrop: Option::Some(Backdrop { blur: 16dp, saturation: 1.8, tint: #ffffff2e, noise: 0.04 })
                        border: Border { width: 1dp, color: #ffffff66 }
                        shadow: Shadow { y: 10dp, blur: 28dp, color: #0000004d }
                        Text("Glassmorphism") { color: #ffffff; font_size: 17sp }
                    }

                    // Acrylic: a dense frost whose light is evened toward its
                    // tint, so text on it reads over anything, with a grain.
                    BlurView {
                        width: fill
                        height: fill
                        padding: 18dp
                        corner_radius: 8dp
                        backdrop: Option::Some(Backdrop {
                            blur: 30dp,
                            saturation: 1.25,
                            tint: #f3f3f38c,
                            luminosity: 0.65,
                            noise: 0.02,
                        })
                        border: Border { width: 1dp, color: #ffffff40 }
                        Text("Acrylic") { color: #1b1b1b; font_size: 17sp }
                    }

                    // Liquid glass: it bends what lies behind at its rim,
                    // follows its light, lights where pressed; the pills melt
                    // into one where they meet.
                    Column {
                        width: fill
                        height: fill
                        gap: 14dp
                        BlurView {
                            width: fill
                            height: fill
                            padding: 18dp
                            corner_radius: 24dp
                            backdrop: Option::Some(Backdrop { interactive: true, ..Backdrop::regular() })
                            Text("Liquid glass") { font_size: 17sp }
                        }
                        Row {
                            gap: 4dp
                            glass_merge: 16dp
                            justify: Justify::center
                            width: fill
                            BlurView {
                                width: 56dp; height: 36dp; corner_radius: 18dp
                                backdrop: Option::Some(Backdrop { interactive: true, ..Backdrop::clear() })
                            }
                            BlurView {
                                width: 56dp; height: 36dp; corner_radius: 18dp
                                backdrop: Option::Some(Backdrop { interactive: true, ..Backdrop::clear() })
                            }
                            BlurView {
                                width: 56dp; height: 36dp; corner_radius: 18dp
                                backdrop: Option::Some(Backdrop { interactive: true, ..Backdrop::clear() })
                            }
                        }
                    }
                }
            }
        }
    }
}
