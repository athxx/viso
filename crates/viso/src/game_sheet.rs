//! A game run drawn as a sheet of frames: what `viso test game --sheet`
//! hands a headless build.
//!
//! The run is the scenario's: the module's systems on a world seeded by the
//! tape, the tape's input, one fixed tick a frame and then every
//! `FrameUpdate` with the frame lasting one tick. Each frame the sheet shows
//! is the World as a `GameViewport` draws it, through the stage's camera,
//! rendered by the software raster into a tile, its tick stamped in the
//! corner. The tiles are laid out in rows on a dark ground.
//!
//! Cold path: one run per sheet.

use std::rc::Rc;

use viso_behavior::game::{InputTape, Scheduler};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Module, Vm};
use viso_gpu::{CpuRaster, GpuBackend, TextureDesc, TextureFormat};
use viso_image::snapshot::Image;
use viso_render::scene3d::SceneFrame;
use viso_render::{Primitive, Rect, Renderer, SceneDraw, SceneSlot};
use viso_view::WorldScene;
use viso_view::headless::GameSheet;

/// The pixels between tiles and around them.
const GAP: u32 = 4;
/// The ground between the tiles, BGRA.
const GROUND: [u8; 4] = [0x1c, 0x18, 0x16, 0xff];

/// Draws `sheet`.
///
/// # Errors
///
/// When the module or the tape does not decode, the module does not link,
/// the start faults or the tape does not fit the build.
pub fn render(sheet: &GameSheet) -> Result<Image, String> {
    let module =
        Module::decode(&sheet.module).map_err(|e| format!("the module does not load: {e}"))?;
    let tape =
        InputTape::decode(&sheet.tape).map_err(|e| format!("the tape does not decode: {e}"))?;
    let grant: Vec<&str> = module.capabilities().iter().map(|c| &**c).collect();
    let mut vm = Vm::new(Rc::new(module.clone()), Budget::default());
    vm.link(&Natives::standard(), &grant)
        .map_err(|e| format!("the natives do not link: {e}"))?;
    let tick = 1.0 / f64::from(tape.tick_rate.max(1));
    let mut game = Scheduler::with_seed(vm, tape.seed)
        .map_err(|fault| viso_view::describe_fault(&module, &fault))?;
    game.play(tape)
        .map_err(|e| format!("the tape does not fit: {e}"))?;

    let scale = sheet.tile.scale as f32;
    let (lw, lh) = (sheet.tile.width as f32, sheet.tile.height as f32);
    let tw = (lw * scale).round() as u32;
    let th = (lh * scale).round() as u32;
    let mut gpu = CpuRaster::new();
    let mut renderer = Renderer::new(&mut gpu, TextureFormat::Bgra8UnormSrgb);
    renderer.set_surface_size([lw, lh], scale);
    let target = gpu.create_texture(&TextureDesc {
        width: tw,
        height: th,
        format: TextureFormat::Bgra8UnormSrgb,
        render_target: true,
        label: "game-sheet",
    });

    let ticks: Vec<u64> = sheet.ticks().collect();
    let columns = sheet.columns();
    let rows = (ticks.len() as u32).div_ceil(columns).max(1);
    let (width, height) = (
        columns * tw + (columns + 1) * GAP,
        rows * th + (rows + 1) * GAP,
    );
    let mut bgra = GROUND.repeat(width as usize * height as usize);

    let slot = SceneSlot(0);
    let mut scene = WorldScene::default();
    let mut frame = SceneFrame::new();
    let mut revision = 0;
    let mut shown = ticks.iter().copied().enumerate().peekable();
    for t in 0..sheet.frames {
        if shown.peek().is_none() {
            break;
        }
        game.step(1);
        game.present(tick);
        game.take_faults();
        let Some((index, _)) = shown.next_if(|&(_, at)| at == t) else {
            continue;
        };
        if scene.fill(&game, None, &mut frame) {
            renderer.swap_scene(slot, &mut frame);
            revision += 1;
        }
        let draw = Primitive::Scene(SceneDraw {
            rect: Rect::new(0.0, 0.0, lw, lh),
            slot,
            revision,
            mask: None,
        });
        renderer.upload(&mut gpu, std::slice::from_ref(&draw));
        renderer.render_to_texture(&mut gpu, target, [0.0, 0.0, 0.0, 1.0], [lw, lh]);
        let mut tile = gpu.read_texture(target);
        stamp(&mut tile, tw, t, (scale.round() as u32).max(1) * 2);
        let (col, row) = (index as u32 % columns, index as u32 / columns);
        let (x0, y0) = (GAP + col * (tw + GAP), GAP + row * (th + GAP));
        for y in 0..th {
            let from = (y * tw * 4) as usize;
            let to = (((y0 + y) * width + x0) * 4) as usize;
            bgra[to..to + tw as usize * 4].copy_from_slice(&tile[from..from + tw as usize * 4]);
        }
    }
    Ok(Image::from_bgra8(width, height, &bgra))
}

/// Runs the sheet a headless build is handed at `path`, writes its image
/// and ends the process: 0 once written, 2 when it cannot be.
#[cfg(feature = "headless")]
pub(crate) fn run(path: &str) -> ! {
    let fail = |message: String| -> ! {
        eprintln!("viso: {message}");
        std::process::exit(2);
    };
    let bytes = std::fs::read(path)
        .unwrap_or_else(|error| fail(format!("cannot read the sheet `{path}`: {error}")));
    let sheet = GameSheet::from_bytes(&bytes)
        .unwrap_or_else(|error| fail(format!("the sheet `{path}` does not decode: {error}")));
    let image = render(&sheet).unwrap_or_else(|message| fail(message));
    if let Err(error) = std::fs::write(&sheet.output, image.to_png()) {
        fail(format!("cannot write `{}`: {error}", sheet.output));
    }
    std::process::exit(0);
}

/// The digits as 3 × 5 bitmaps, a row of three bits each, top first.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b010, 0b010, 0b010],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// Stamps `tick` in white on a dark plate in the top-left corner of a BGRA
/// tile `width` pixels wide, each bitmap pixel `px` pixels large.
fn stamp(tile: &mut [u8], width: u32, tick: u64, px: u32) {
    let digits: Vec<usize> = tick
        .to_string()
        .bytes()
        .map(|b| (b - b'0') as usize)
        .collect();
    let pad = px;
    let plate_w = digits.len() as u32 * 4 * px - px + 2 * pad;
    let plate_h = 5 * px + 2 * pad;
    let height = tile.len() as u32 / 4 / width;
    let mut put = |x: u32, y: u32, bgra: [u8; 4]| {
        if x < width && y < height {
            let at = ((y * width + x) * 4) as usize;
            tile[at..at + 4].copy_from_slice(&bgra);
        }
    };
    for y in 0..plate_h {
        for x in 0..plate_w {
            put(x, y, [0x10, 0x10, 0x10, 0xff]);
        }
    }
    for (i, &d) in digits.iter().enumerate() {
        let left = pad + i as u32 * 4 * px;
        for (row, bits) in DIGITS[d].iter().enumerate() {
            for col in 0..3u32 {
                if bits & (0b100 >> col) == 0 {
                    continue;
                }
                for dy in 0..px {
                    for dx in 0..px {
                        put(left + col * px + dx, pad + row as u32 * px + dy, [0xff; 4]);
                    }
                }
            }
        }
    }
}
