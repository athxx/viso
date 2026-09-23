//! The semantic cross-check of the WGSL emitter on real hardware: every built-in's
//! web WGSL is translated to MSL by `naga`, compiled on the Metal device, and the
//! golden scenes rendered through it must match the same scenes rendered through
//! the native MSL. The two programs share only the IR, so a body the WGSL
//! printer lowers differently from the MSL one shows up as a pixel difference.

#![cfg(target_vendor = "apple")]

mod golden_scenes;

use std::collections::BTreeMap;

use golden_scenes::{GoldenScene, H, W, diff, render_to_target};
use naga::back::msl;
use viso_gpu::{
    AttrFormat, BindGroupDesc, BindGroupId, BufferDesc, BufferId, BuiltinShader, Caps, DrawList,
    Frame, GpuBackend, InstanceLayout, LayoutError, MetalBackend, PipelineDesc, PipelineId,
    RawWindowHandle, SamplerDesc, SamplerId, ShaderCode, ShaderLang, SurfaceId, TextureDesc,
    TextureFormat, TextureId,
};

/// The Metal device fed MSL that `naga` produced from the web WGSL.
struct NagaMetal {
    metal: MetalBackend,
}

/// The Metal backend's argument table: generated-geometry built-ins read their
/// instances from buffer 1 and the uniforms from buffer 0; mesh built-ins read
/// vertices from buffer 0 and the uniforms from buffer 1. Textures and the
/// sampler take fragment slots 0, 1 and sampler 0.
fn slots(builtin: BuiltinShader) -> (u8, u8, msl::VertexBufferStepMode) {
    match builtin {
        BuiltinShader::Path | BuiltinShader::Mesh => (0, 1, msl::VertexBufferStepMode::ByVertex),
        _ => (1, 0, msl::VertexBufferStepMode::ByInstance),
    }
}

fn vertex_format(format: AttrFormat) -> naga_types::VertexFormat {
    use naga_types::VertexFormat as V;
    match format {
        AttrFormat::Float1 => V::Float32,
        AttrFormat::Float2 => V::Float32x2,
        AttrFormat::Float3 => V::Float32x3,
        AttrFormat::Float4 => V::Float32x4,
        AttrFormat::Uint1 => V::Uint32,
        AttrFormat::Uint2 => V::Uint32x2,
        AttrFormat::Uint4 => V::Uint32x4,
    }
}

fn translate(desc: &PipelineDesc, layout: &InstanceLayout) -> (String, String, String) {
    let ShaderCode::Wgsl(wgsl) = desc.code else {
        panic!(
            "{}: the renderer handed over {:?}",
            desc.label,
            desc.code.lang()
        );
    };
    let module = naga::front::wgsl::parse_str(wgsl)
        .unwrap_or_else(|e| panic!("{}: {}", desc.label, e.emit_to_string(wgsl)));
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{}: {e:?}", desc.label));

    let (vertex_buffer, uniform_buffer, step_mode) = slots(desc.builtin);
    let resource = |group, binding| naga::ResourceBinding { group, binding };
    let mut resources = BTreeMap::new();
    resources.insert(
        resource(0, 0),
        msl::BindTarget {
            buffer: Some(uniform_buffer),
            ..Default::default()
        },
    );
    resources.insert(
        resource(1, 0),
        msl::BindTarget {
            texture: Some(0),
            ..Default::default()
        },
    );
    resources.insert(
        resource(1, 1),
        msl::BindTarget {
            texture: Some(1),
            ..Default::default()
        },
    );
    resources.insert(
        resource(1, 2),
        msl::BindTarget {
            sampler: Some(msl::BindSamplerTarget::Resource(0)),
            ..Default::default()
        },
    );
    let entry = msl::EntryPointResources {
        resources,
        immediates_buffer: None,
        // Never bound: the pulled-vertex bounds checks that would read it are
        // removed below, and nothing else declares a runtime-sized array.
        sizes_buffer: Some(30),
    };
    let options = msl::Options {
        lang_version: (2, 3),
        per_entry_point_map: [
            (desc.vertex_entry.to_owned(), entry.clone()),
            (desc.fragment_entry.to_owned(), entry),
        ]
        .into(),
        fake_missing_bindings: false,
        ..Default::default()
    };

    let mut offset = 0;
    let attributes = desc
        .instance_schema
        .attributes
        .iter()
        .enumerate()
        .map(|(location, a)| {
            let mapping = msl::AttributeMapping {
                shader_location: location as u32,
                offset,
                format: vertex_format(a.format),
            };
            offset += a.format.size() as u32;
            mapping
        })
        .collect::<Vec<_>>();
    let pipeline = msl::PipelineOptions {
        vertex_pulling_transform: !attributes.is_empty(),
        vertex_buffer_mappings: if attributes.is_empty() {
            Vec::new()
        } else {
            vec![msl::VertexBufferMapping {
                id: u32::from(vertex_buffer),
                stride: layout.stride as u32,
                step_mode,
                attributes,
            }]
        },
        ..Default::default()
    };
    let (source, translation) = msl::write_string(&module, &info, &options, &pipeline)
        .unwrap_or_else(|e| panic!("{}: {e:?}", desc.label));
    let name = |entry: &str| {
        let index = module
            .entry_points
            .iter()
            .position(|e| e.name == entry)
            .unwrap_or_else(|| panic!("{}: no entry `{entry}`", desc.label));
        translation.entry_point_names[index]
            .clone()
            .unwrap_or_else(|e| panic!("{}: {e:?}", desc.label))
    };
    let vertex = name(desc.vertex_entry);
    let fragment = name(desc.fragment_entry);
    let source = strip_pull_bounds(&source);
    assert!(
        !source.contains("_buffer_sizes.buffer_size"),
        "{}: a pulled-vertex bounds check survived",
        desc.label
    );
    (source, vertex, fragment)
}

/// Remove the pulled-vertex bounds checks: they compare the index with a
/// buffer-size table the Metal backend has no reason to bind, and the renderer
/// never draws past the data it wrote.
fn strip_pull_bounds(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("(_buffer_sizes.buffer_size") {
        let Some(len) = rest[start..].find(')') else {
            break;
        };
        // `index < (_buffer_sizes.buffer_sizeN / stride)`: drop back to the
        // start of the comparison.
        let head = &rest[..start];
        let cut = head.rfind('<').expect("a bounds check is a comparison");
        let lhs = head[..cut].trim_end();
        let lhs_start = lhs
            .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map_or(0, |i| i + 1);
        out.push_str(&lhs[..lhs_start]);
        out.push_str("true");
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    out
}

impl GpuBackend for NagaMetal {
    const SHADER_LANG: ShaderLang = ShaderLang::Wgsl;

    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId {
        self.metal.create_buffer(desc)
    }

    fn create_texture(&mut self, desc: &TextureDesc) -> TextureId {
        self.metal.create_texture(desc)
    }

    fn create_sampler(&mut self, desc: &SamplerDesc) -> SamplerId {
        self.metal.create_sampler(desc)
    }

    fn create_pipeline(
        &mut self,
        desc: &PipelineDesc,
        layout: &InstanceLayout,
    ) -> Result<PipelineId, LayoutError> {
        layout.validate_against(&desc.instance_schema)?;
        let (source, vertex, fragment) = translate(desc, layout);
        // The Metal backend takes `'static` program text, as the frozen MSL is.
        let native = PipelineDesc {
            code: ShaderCode::Msl(Box::leak(source.into_boxed_str())),
            vertex_entry: Box::leak(vertex.into_boxed_str()),
            fragment_entry: Box::leak(fragment.into_boxed_str()),
            ..*desc
        };
        self.metal.create_pipeline(&native, layout)
    }

    fn create_bind_group(&mut self, desc: &BindGroupDesc) -> BindGroupId {
        self.metal.create_bind_group(desc)
    }

    fn destroy_buffer(&mut self, id: BufferId) {
        self.metal.destroy_buffer(id);
    }

    fn destroy_texture(&mut self, id: TextureId) {
        self.metal.destroy_texture(id);
    }

    fn destroy_sampler(&mut self, id: SamplerId) {
        self.metal.destroy_sampler(id);
    }

    fn destroy_pipeline(&mut self, id: PipelineId) {
        self.metal.destroy_pipeline(id);
    }

    fn destroy_bind_group(&mut self, id: BindGroupId) {
        self.metal.destroy_bind_group(id);
    }

    fn write_buffer(&mut self, id: BufferId, offset: usize, bytes: &[u8]) {
        self.metal.write_buffer(id, offset, bytes);
    }

    fn write_texture(&mut self, id: TextureId, x: u32, y: u32, w: u32, h: u32, bytes: &[u8]) {
        self.metal.write_texture(id, x, y, w, h, bytes);
    }

    fn create_surface(&mut self, raw: RawWindowHandle, width: u32, height: u32) -> SurfaceId {
        self.metal.create_surface(raw, width, height)
    }

    fn resize_surface(&mut self, id: SurfaceId, width: u32, height: u32) {
        self.metal.resize_surface(id, width, height);
    }

    fn destroy_surface(&mut self, id: SurfaceId) {
        self.metal.destroy_surface(id);
    }

    fn begin_frame(&mut self, surface: SurfaceId) -> Option<Frame> {
        self.metal.begin_frame(surface)
    }

    fn encode(&mut self, list: &DrawList<'_>) {
        self.metal.encode(list);
    }

    fn present(&mut self, frame: Frame) {
        self.metal.present(frame);
    }

    fn device_lost(&mut self, surface: SurfaceId) {
        self.metal.device_lost(surface);
    }

    fn caps(&self) -> &Caps {
        self.metal.caps()
    }

    fn surface_format(&self, surface: SurfaceId) -> TextureFormat {
        self.metal.surface_format(surface)
    }
}

/// Both programs run on the same device, so they differ only where the two
/// compilers order floating-point work differently.
const CROSS_TOL: u8 = 1;

#[test]
fn naga_msl_from_the_wgsl_matches_the_native_msl_on_every_golden_scene() {
    for scene in GoldenScene::ALL {
        let mut native = MetalBackend::new();
        let target = render_to_target(&mut native, scene);
        let expected = native.read_texture(target);

        let mut translated = NagaMetal {
            metal: MetalBackend::new(),
        };
        let target = render_to_target(&mut translated, scene);
        let actual = translated.metal.read_texture(target);

        assert_eq!(actual.len(), (W * H * 4) as usize);
        let d = diff(&actual, &expected, CROSS_TOL);
        assert_eq!(
            d.pixels_over, 0,
            "{scene:?}: the WGSL lowering diverges from the MSL (worst {}, first at {:?})",
            d.worst, d.first_over
        );
    }
}

#[test]
fn the_bounds_check_strip_keeps_the_surrounding_expression() {
    assert_eq!(
        strip_pull_bounds("if (vid < (_buffer_sizes.buffer_size1 / 16)) { x; }"),
        "if (true) { x; }"
    );
    assert_eq!(
        strip_pull_bounds("a && b_2 < (_buffer_sizes.buffer_size0 / 8u) ? v : w"),
        "a && true ? v : w"
    );
    assert_eq!(strip_pull_bounds("no checks"), "no checks");
}
