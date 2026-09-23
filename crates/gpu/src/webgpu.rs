//! `WebGpuBackend` — the WebGPU implementation of [`GpuBackend`], the
//! `wasm32-unknown-unknown` path.
//!
//! ## Design decisions
//!
//! - **Programs are WGSL** ([`ShaderCode::Wgsl`]) consumed as-is by
//!   `createShaderModule`. Each module reads the uniforms from
//!   `@group(0) @binding(0)` and `tex`/`dst_tex`/`samp` from bindings 0/1/2 of
//!   `@group(1)`, so **one pipeline layout** serves every pipeline.
//! - **Uniforms ride one dynamic-offset uniform ring.** A draw's
//!   [`InlineUniforms`] are copied into a CPU scratch at the next
//!   `minUniformBufferOffsetAlignment` slot — consecutive identical payloads (the
//!   viewport, almost always) share one slot and skip the rebind — and the
//!   scratch goes to the GPU in one `writeBuffer` right before the submission
//!   that reads it. Queue operations execute in order, so the ring is rewound
//!   after every submission and only ever holds one encode's worth of slots; it
//!   grows (by powers of two) before an encode that needs more.
//! - **Buffers and textures are device memory written through the queue**
//!   (`writeBuffer` / `writeTexture`): the browser stages the copy, and every
//!   write is ordered before any later submission, exactly the ordering the
//!   renderer relies on from Metal's shared memory.
//! - **Every encode is one command buffer, submitted at its end.** The browser
//!   presents a canvas on its own when the task that drew into it returns, so
//!   [`present`](GpuBackend::present) only closes the frame.
//! - **Destruction is immediate on the WebGPU side**: `destroy()` after a
//!   submission is valid and the implementation keeps the memory until that
//!   work finishes. The slot itself is reclaimed at the next frame boundary so
//!   stale handles keep the same lifetime as on the other backends.
//! - **Device bring-up is asynchronous** in WebGPU and nowhere else. Await
//!   [`prepare`] once before the synchronous [`create_device`](crate::create_device)
//!   (the web entry point does this), or build a backend directly with
//!   [`WebGpuBackend::request`].
//! - **Surfaces are canvases** found by their `data-viso-canvas` attribute
//!   ([`RawWindowHandle::WebCanvas`]), configured opaque in the adapter's
//!   preferred format.
//! - **Device loss** (the `lost` promise) makes [`begin_frame`](GpuBackend::begin_frame)
//!   return `None` for good; validation errors surface through
//!   `uncapturederror` and are counted in [`WebGpuBackend::validation_errors`].

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Array, Object, Promise, Reflect, Uint8Array};
use viso_handle::RawWindowHandle;
use wasm_bindgen::prelude::*;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::backend::{
    DrawCommand, DrawList, Frame, Geometry, GpuBackend, IndexFormat, InlineUniforms, LoadOp,
    RenderPass, RenderTarget,
};
use crate::instance::{AttrFormat, InstanceLayout};
use crate::resource::{
    AddressMode, BindGroupDesc, Binding, BlendMode, BufferDesc, BuiltinShader, Caps, FilterMode,
    PipelineDesc, SamplerDesc, ShaderCode, ShaderLang, TextureDesc, TextureFormat,
};
use crate::retire::{Epoch, Fence, ResourceKind, RetireQueue, Retired};
use crate::slots::SlotMap;
use crate::{BindGroupId, BufferId, PipelineId, SamplerId, SurfaceId, TextureId};

/// Uniform-ring slots allocated up front.
const INITIAL_UNIFORM_SLOTS: usize = 256;

// GPUBufferUsage bits.
const BUFFER_MAP_READ: u32 = 0x0001;
const BUFFER_COPY_DST: u32 = 0x0008;
const BUFFER_INDEX: u32 = 0x0010;
const BUFFER_VERTEX: u32 = 0x0020;
const BUFFER_UNIFORM: u32 = 0x0040;
// GPUTextureUsage bits.
const TEXTURE_COPY_SRC: u32 = 0x01;
const TEXTURE_COPY_DST: u32 = 0x02;
const TEXTURE_BINDING: u32 = 0x04;
const TEXTURE_RENDER_ATTACHMENT: u32 = 0x10;
// GPUShaderStage bits.
const STAGE_VERTEX: u32 = 0x1;
const STAGE_FRAGMENT: u32 = 0x2;
// GPUMapMode bits.
const MAP_READ: u32 = 0x1;
/// `copyTextureToBuffer` row-pitch alignment.
const COPY_ROW_ALIGN: usize = 256;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn console_error(message: &JsValue);

    /// `navigator.gpu`.
    type Gpu;
    #[wasm_bindgen(method, js_name = requestAdapter)]
    fn request_adapter(this: &Gpu, options: &Object) -> Promise;
    #[wasm_bindgen(method, js_name = getPreferredCanvasFormat)]
    fn preferred_canvas_format(this: &Gpu) -> String;

    type GpuAdapter;
    #[wasm_bindgen(method, getter)]
    fn features(this: &GpuAdapter) -> SupportedFeatures;
    #[wasm_bindgen(method, getter)]
    fn limits(this: &GpuAdapter) -> SupportedLimits;
    #[wasm_bindgen(method, js_name = requestDevice)]
    fn request_device(this: &GpuAdapter, desc: &Object) -> Promise;

    type SupportedFeatures;
    #[wasm_bindgen(method)]
    fn has(this: &SupportedFeatures, name: &str) -> bool;

    type SupportedLimits;
    #[wasm_bindgen(method, getter, js_name = maxTextureDimension2D)]
    fn max_texture_dimension_2d(this: &SupportedLimits) -> u32;
    #[wasm_bindgen(method, getter, js_name = minUniformBufferOffsetAlignment)]
    fn min_uniform_buffer_offset_alignment(this: &SupportedLimits) -> u32;

    type GpuDevice;
    #[wasm_bindgen(method, getter)]
    fn queue(this: &GpuDevice) -> GpuQueue;
    #[wasm_bindgen(method, getter)]
    fn lost(this: &GpuDevice) -> Promise;
    #[wasm_bindgen(method, getter, js_name = limits)]
    fn device_limits(this: &GpuDevice) -> SupportedLimits;
    #[wasm_bindgen(method, js_name = addEventListener)]
    fn add_event_listener(this: &GpuDevice, kind: &str, listener: &js_sys::Function);
    #[wasm_bindgen(method, js_name = removeEventListener)]
    fn remove_event_listener(this: &GpuDevice, kind: &str, listener: &js_sys::Function);
    #[wasm_bindgen(method, js_name = createBuffer)]
    fn create_buffer(this: &GpuDevice, desc: &Object) -> GpuBuffer;
    #[wasm_bindgen(method, js_name = createTexture)]
    fn create_texture(this: &GpuDevice, desc: &Object) -> GpuTexture;
    #[wasm_bindgen(method, js_name = createSampler)]
    fn create_sampler(this: &GpuDevice, desc: &Object) -> GpuSampler;
    #[wasm_bindgen(method, js_name = createShaderModule)]
    fn create_shader_module(this: &GpuDevice, desc: &Object) -> GpuShaderModule;
    #[wasm_bindgen(method, js_name = createBindGroupLayout)]
    fn create_bind_group_layout(this: &GpuDevice, desc: &Object) -> GpuBindGroupLayout;
    #[wasm_bindgen(method, js_name = createPipelineLayout)]
    fn create_pipeline_layout(this: &GpuDevice, desc: &Object) -> GpuPipelineLayout;
    #[wasm_bindgen(method, js_name = createRenderPipeline)]
    fn create_render_pipeline(this: &GpuDevice, desc: &Object) -> GpuRenderPipeline;
    #[wasm_bindgen(method, js_name = createBindGroup)]
    fn create_bind_group(this: &GpuDevice, desc: &Object) -> GpuBindGroup;
    #[wasm_bindgen(method, js_name = createCommandEncoder)]
    fn create_command_encoder(this: &GpuDevice) -> GpuCommandEncoder;
    #[wasm_bindgen(method, js_name = destroy)]
    fn destroy_device(this: &GpuDevice);

    type GpuQueue;
    #[wasm_bindgen(method, js_name = writeBuffer)]
    fn write_buffer(this: &GpuQueue, buffer: &GpuBuffer, offset: f64, data: &[u8]);
    #[wasm_bindgen(method, js_name = writeTexture)]
    fn write_texture(this: &GpuQueue, dst: &Object, data: &[u8], layout: &Object, size: &Object);
    #[wasm_bindgen(method)]
    fn submit(this: &GpuQueue, buffers: &Array);

    type GpuBuffer;
    #[wasm_bindgen(method, js_name = destroy)]
    fn destroy_buffer(this: &GpuBuffer);
    #[wasm_bindgen(method, js_name = mapAsync)]
    fn map_async(this: &GpuBuffer, mode: u32) -> Promise;
    #[wasm_bindgen(method, js_name = getMappedRange)]
    fn get_mapped_range(this: &GpuBuffer) -> js_sys::ArrayBuffer;
    #[wasm_bindgen(method)]
    fn unmap(this: &GpuBuffer);

    type GpuTexture;
    #[wasm_bindgen(method, js_name = createView)]
    fn create_view(this: &GpuTexture) -> GpuTextureView;
    #[wasm_bindgen(method, js_name = destroy)]
    fn destroy_texture(this: &GpuTexture);

    type GpuTextureView;
    type GpuSampler;
    type GpuShaderModule;
    type GpuBindGroupLayout;
    type GpuPipelineLayout;
    type GpuRenderPipeline;
    type GpuBindGroup;
    type GpuCommandBuffer;

    type GpuCommandEncoder;
    #[wasm_bindgen(method, js_name = beginRenderPass)]
    fn begin_render_pass(this: &GpuCommandEncoder, desc: &Object) -> GpuRenderPassEncoder;
    #[wasm_bindgen(method, js_name = copyTextureToBuffer)]
    fn copy_texture_to_buffer(this: &GpuCommandEncoder, src: &Object, dst: &Object, size: &Object);
    #[wasm_bindgen(method)]
    fn finish(this: &GpuCommandEncoder) -> GpuCommandBuffer;

    type GpuRenderPassEncoder;
    #[wasm_bindgen(method, js_name = setPipeline)]
    fn set_pipeline(this: &GpuRenderPassEncoder, pipeline: &GpuRenderPipeline);
    #[wasm_bindgen(method, js_name = setBindGroup)]
    fn set_bind_group(this: &GpuRenderPassEncoder, index: u32, group: &GpuBindGroup);
    #[wasm_bindgen(method, js_name = setBindGroup)]
    fn set_bind_group_with_offsets(
        this: &GpuRenderPassEncoder,
        index: u32,
        group: &GpuBindGroup,
        offsets: &[u32],
        start: f64,
        len: u32,
    );
    #[wasm_bindgen(method, js_name = setVertexBuffer)]
    fn set_vertex_buffer(this: &GpuRenderPassEncoder, slot: u32, buffer: &GpuBuffer, offset: f64);
    #[wasm_bindgen(method, js_name = setIndexBuffer)]
    fn set_index_buffer(this: &GpuRenderPassEncoder, buffer: &GpuBuffer, format: &str, offset: f64);
    #[wasm_bindgen(method)]
    fn draw(
        this: &GpuRenderPassEncoder,
        vertices: u32,
        instances: u32,
        first: u32,
        first_inst: u32,
    );
    #[wasm_bindgen(method, js_name = drawIndexed)]
    fn draw_indexed(
        this: &GpuRenderPassEncoder,
        indices: u32,
        instances: u32,
        first: u32,
        base_vertex: i32,
        first_inst: u32,
    );
    #[wasm_bindgen(method, js_name = setScissorRect)]
    fn set_scissor_rect(this: &GpuRenderPassEncoder, x: u32, y: u32, w: u32, h: u32);
    #[wasm_bindgen(method)]
    fn end(this: &GpuRenderPassEncoder);

    type GpuCanvasContext;
    #[wasm_bindgen(method, catch)]
    fn configure(this: &GpuCanvasContext, desc: &Object) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch, js_name = getCurrentTexture)]
    fn get_current_texture(this: &GpuCanvasContext) -> Result<GpuTexture, JsValue>;
    #[wasm_bindgen(method)]
    fn unconfigure(this: &GpuCanvasContext);

    type Canvas;
    #[wasm_bindgen(method, js_name = getContext)]
    fn get_context(this: &Canvas, kind: &str) -> JsValue;
    #[wasm_bindgen(method, setter)]
    fn set_width(this: &Canvas, width: u32);
    #[wasm_bindgen(method, setter)]
    fn set_height(this: &Canvas, height: u32);

    type Document;
    #[wasm_bindgen(method, js_name = querySelector)]
    fn query_selector(this: &Document, selector: &str) -> JsValue;
}

/// Set `key` on a descriptor object. Descriptors are plain JS objects; a
/// failing `Reflect.set` on one would mean a frozen object, which these never are.
fn set(target: &Object, key: &str, value: impl Into<JsValue>) {
    let _ = Reflect::set(target, &JsValue::from_str(key), &value.into());
}

/// A plain JS object with the given properties.
fn obj(props: &[(&str, JsValue)]) -> Object {
    let o = Object::new();
    for (k, v) in props {
        set(&o, k, v.clone());
    }
    o
}

/// A JS array of the given values.
fn arr(items: &[JsValue]) -> Array {
    items.iter().collect()
}

/// Resolve a promise, mapping a rejection or an absent value to `None`.
async fn resolve(promise: Promise) -> Option<JsValue> {
    JsFuture::from(promise)
        .await
        .ok()
        .filter(|v| !v.is_null() && !v.is_undefined())
}

/// `navigator.gpu`, or `None` where WebGPU is not exposed.
fn navigator_gpu() -> Option<Gpu> {
    let navigator = Reflect::get(&js_sys::global(), &"navigator".into()).ok()?;
    let gpu = Reflect::get(&navigator, &"gpu".into()).ok()?;
    (!gpu.is_undefined() && !gpu.is_null()).then(|| gpu.unchecked_into())
}

/// An opened device and what the backend needs to know about its adapter.
struct Opened {
    device: GpuDevice,
    preferred: TextureFormat,
    float32_filterable: bool,
}

/// Open the best adapter's device.
async fn open() -> Option<Opened> {
    let gpu = navigator_gpu()?;
    let options = obj(&[("powerPreference", "high-performance".into())]);
    let adapter: GpuAdapter = resolve(gpu.request_adapter(&options))
        .await?
        .unchecked_into();
    // `R32Float` (the depth plane's storage) is sampled through the shared
    // filtering layout, which needs this feature; request it whenever offered.
    let float32_filterable = adapter.features().has("float32-filterable");
    let features = if float32_filterable {
        arr(&["float32-filterable".into()])
    } else {
        Array::new()
    };
    let limits = adapter.limits();
    let required = obj(&[
        (
            "maxTextureDimension2D",
            limits.max_texture_dimension_2d().into(),
        ),
        (
            "minUniformBufferOffsetAlignment",
            limits.min_uniform_buffer_offset_alignment().into(),
        ),
    ]);
    let desc = obj(&[
        ("requiredFeatures", features.into()),
        ("requiredLimits", required.into()),
    ]);
    let device: GpuDevice = resolve(adapter.request_device(&desc))
        .await?
        .unchecked_into();
    let preferred = match gpu.preferred_canvas_format().as_str() {
        "rgba8unorm" => TextureFormat::Rgba8Unorm,
        _ => TextureFormat::Bgra8Unorm,
    };
    Some(Opened {
        device,
        preferred,
        float32_filterable,
    })
}

thread_local! {
    /// The device [`prepare`] opened, taken by the next [`WebGpuBackend::new`].
    static PREPARED: RefCell<Option<Opened>> = const { RefCell::new(None) };
}

/// Open the WebGPU device so a later synchronous [`WebGpuBackend::new`] (and so
/// [`create_device`](crate::create_device)) can take it. Returns whether a
/// device is available.
pub async fn prepare() -> bool {
    let opened = open().await;
    let ok = opened.is_some();
    PREPARED.with(|p| *p.borrow_mut() = opened);
    ok
}

/// A buffer: the WebGPU buffer and its byte length.
struct WebGpuBuffer {
    buffer: GpuBuffer,
    len: usize,
}

/// A texture with its default view, cached for bind groups and passes.
struct WebGpuTexture {
    texture: GpuTexture,
    view: GpuTextureView,
    format: TextureFormat,
    width: u32,
    height: u32,
    render_target: bool,
}

/// A registered pipeline.
struct WebGpuPipeline {
    pipeline: GpuRenderPipeline,
    /// Whether the pipeline declares a vertex buffer at slot 0.
    has_vertex_buffer: bool,
}

/// A canvas surface.
struct WebGpuSurface {
    canvas: Canvas,
    context: GpuCanvasContext,
    format: TextureFormat,
    width: u32,
    height: u32,
}

/// The frame between `begin_frame` and `present`.
struct Acquired {
    surface: SurfaceId,
    view: GpuTextureView,
}

/// The uniform ring: the GPU buffer, its bind group, and the CPU scratch the
/// next submission's slots are written into.
struct UniformRing {
    buffer: GpuBuffer,
    group: GpuBindGroup,
    /// Capacity in slots.
    capacity: usize,
    /// Slot stride: the device's uniform offset alignment.
    stride: usize,
    scratch: Vec<u8>,
    /// Slots written since the last submission.
    used: usize,
    /// The last slot written and its payload, for consecutive-draw dedupe.
    last: Option<(u32, [u8; InlineUniforms::MAX])>,
}

/// Reusable render-pass descriptor objects: one pass, one color attachment,
/// rewritten per pass instead of rebuilt.
struct PassDescriptor {
    pass: Object,
    attachment: Object,
    clear: Object,
    key_view: JsValue,
    key_load_op: JsValue,
    load: JsValue,
    clear_op: JsValue,
    channel_keys: [JsValue; 4],
}

impl PassDescriptor {
    fn new() -> Self {
        let clear = obj(&[
            ("r", 0.0.into()),
            ("g", 0.0.into()),
            ("b", 0.0.into()),
            ("a", 0.0.into()),
        ]);
        let attachment = obj(&[
            ("loadOp", "clear".into()),
            ("storeOp", "store".into()),
            ("clearValue", clear.clone().into()),
        ]);
        let pass = obj(&[("colorAttachments", arr(&[attachment.clone().into()]).into())]);
        Self {
            pass,
            attachment,
            clear,
            key_view: "view".into(),
            key_load_op: "loadOp".into(),
            load: "load".into(),
            clear_op: "clear".into(),
            channel_keys: ["r".into(), "g".into(), "b".into(), "a".into()],
        }
    }

    /// Point the descriptor at `view` with `load`.
    fn target(&self, view: &GpuTextureView, load: LoadOp) {
        let _ = Reflect::set(&self.attachment, &self.key_view, view);
        match load {
            LoadOp::Clear(color) => {
                let _ = Reflect::set(&self.attachment, &self.key_load_op, &self.clear_op);
                for (key, value) in self.channel_keys.iter().zip(color) {
                    let _ = Reflect::set(&self.clear, key, &JsValue::from_f64(value as f64));
                }
            }
            LoadOp::Load => {
                let _ = Reflect::set(&self.attachment, &self.key_load_op, &self.load);
            }
        }
    }
}

/// The WebGPU backend.
pub struct WebGpuBackend {
    device: GpuDevice,
    queue: GpuQueue,
    preferred: TextureFormat,
    float32_filterable: bool,
    uniform_layout: GpuBindGroupLayout,
    texture_layout: GpuBindGroupLayout,
    pipeline_layout: GpuPipelineLayout,
    /// Bound as group 1 at every pass start, so a draw with no bind group of its
    /// own still satisfies the layout.
    default_group: GpuBindGroup,
    default_texture: GpuTexture,
    default_view: GpuTextureView,
    default_sampler: GpuSampler,
    uniforms: UniformRing,
    pass_desc: PassDescriptor,
    /// One-element array reused for every `queue.submit`.
    submit_list: Array,
    modules: HashMap<usize, GpuShaderModule>,
    acquired: Option<Acquired>,
    buffers: SlotMap<WebGpuBuffer>,
    textures: SlotMap<WebGpuTexture>,
    samplers: SlotMap<GpuSampler>,
    bind_groups: SlotMap<GpuBindGroup>,
    pipelines: SlotMap<WebGpuPipeline>,
    surfaces: SlotMap<WebGpuSurface>,
    caps: Caps,
    retire_queue: RetireQueue,
    current_epoch: Epoch,
    reclaim_scratch: Vec<Retired>,
    lost: Rc<Cell<bool>>,
    validation_errors: Rc<Cell<u32>>,
    /// The `uncapturederror` listener, removed in `Drop` before it is freed.
    on_error: Closure<dyn FnMut(JsValue)>,
}

impl Default for WebGpuBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl WebGpuBackend {
    /// Take the device [`prepare`] opened.
    ///
    /// # Panics
    /// Panics if [`prepare`] was not awaited first, or found no device.
    pub fn new() -> Self {
        let opened = PREPARED
            .with(|p| p.borrow_mut().take())
            .expect("await viso_gpu::webgpu::prepare() before creating the WebGPU device");
        Self::from_opened(opened)
    }

    /// Open a device and build the backend on it, or `None` without WebGPU.
    pub async fn request() -> Option<Self> {
        open().await.map(Self::from_opened)
    }

    fn from_opened(opened: Opened) -> Self {
        let Opened {
            device,
            preferred,
            float32_filterable,
        } = opened;
        let queue = device.queue();
        let limits = device.device_limits();

        let lost = Rc::new(Cell::new(false));
        // The `lost` promise settles exactly once — at the latest when `Drop`
        // destroys the device — so a one-shot closure frees itself when it runs.
        let on_lost = {
            let lost = lost.clone();
            Closure::once_into_js(move |info: JsValue| {
                lost.set(true);
                let reason = Reflect::get(&info, &"reason".into()).ok();
                if reason.and_then(|r| r.as_string()).as_deref() != Some("destroyed") {
                    console_error(&info);
                }
            })
        };
        let lost_promise = device.lost();
        if let Ok(then) = Reflect::get(&lost_promise, &"then".into()) {
            let _ = then
                .unchecked_into::<js_sys::Function>()
                .call1(&lost_promise, &on_lost);
        }
        let validation_errors = Rc::new(Cell::new(0u32));
        let on_error = {
            let count = validation_errors.clone();
            Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
                count.set(count.get() + 1);
                let error = Reflect::get(&event, &"error".into()).unwrap_or(event);
                let message = Reflect::get(&error, &"message".into()).unwrap_or(error);
                console_error(&message);
            })
        };
        device.add_event_listener("uncapturederror", on_error.as_ref().unchecked_ref());

        let visibility = STAGE_VERTEX | STAGE_FRAGMENT;
        let uniform_layout = device.create_bind_group_layout(&obj(&[(
            "entries",
            arr(&[obj(&[
                ("binding", 0.into()),
                ("visibility", visibility.into()),
                (
                    "buffer",
                    obj(&[
                        ("type", "uniform".into()),
                        ("hasDynamicOffset", true.into()),
                        ("minBindingSize", (InlineUniforms::MAX as u32).into()),
                    ])
                    .into(),
                ),
            ])
            .into()])
            .into(),
        )]));
        let texture_entry = |binding: u32| {
            obj(&[
                ("binding", binding.into()),
                ("visibility", visibility.into()),
                (
                    "texture",
                    obj(&[
                        ("sampleType", "float".into()),
                        ("viewDimension", "2d".into()),
                    ])
                    .into(),
                ),
            ])
            .into()
        };
        let texture_layout = device.create_bind_group_layout(&obj(&[(
            "entries",
            arr(&[
                texture_entry(0),
                texture_entry(1),
                obj(&[
                    ("binding", 2.into()),
                    ("visibility", visibility.into()),
                    ("sampler", obj(&[("type", "filtering".into())]).into()),
                ])
                .into(),
            ])
            .into(),
        )]));
        let pipeline_layout = device.create_pipeline_layout(&obj(&[(
            "bindGroupLayouts",
            arr(&[uniform_layout.clone(), texture_layout.clone()]).into(),
        )]));

        let default_texture = device.create_texture(&obj(&[
            (
                "size",
                obj(&[("width", 1.into()), ("height", 1.into())]).into(),
            ),
            ("format", "rgba8unorm".into()),
            ("usage", TEXTURE_BINDING.into()),
        ]));
        let default_view = default_texture.create_view();
        let default_sampler = device.create_sampler(&sampler_descriptor(&SamplerDesc::default()));
        let default_group = texture_group(
            &device,
            &texture_layout,
            &default_view,
            &default_view,
            &default_sampler,
        );

        let stride =
            (limits.min_uniform_buffer_offset_alignment() as usize).max(InlineUniforms::MAX);
        let uniforms = uniform_ring(&device, &uniform_layout, INITIAL_UNIFORM_SLOTS, stride);

        let caps = Caps {
            max_texture_size: limits.max_texture_dimension_2d(),
            presents_to_display: true,
            compute_dispatch: false,
            bindless_texture_slots: 0,
            indirect_draw: false,
        };
        Self {
            device,
            queue,
            preferred,
            float32_filterable,
            uniform_layout,
            texture_layout,
            pipeline_layout,
            default_group,
            default_texture,
            default_view,
            default_sampler,
            uniforms,
            pass_desc: PassDescriptor::new(),
            submit_list: Array::new_with_length(1),
            modules: HashMap::new(),
            acquired: None,
            buffers: SlotMap::new(),
            textures: SlotMap::new(),
            samplers: SlotMap::new(),
            bind_groups: SlotMap::new(),
            pipelines: SlotMap::new(),
            surfaces: SlotMap::new(),
            caps,
            retire_queue: RetireQueue::new(),
            current_epoch: Epoch::START,
            reclaim_scratch: Vec::new(),
            lost,
            validation_errors,
            on_error,
        }
    }

    /// Number of validation errors the device has reported so far. Errors are
    /// delivered asynchronously: await a read-back (or yield to the event loop)
    /// before reading this for work just submitted.
    pub fn validation_errors(&self) -> u32 {
        self.validation_errors.get()
    }

    /// Whether the device has been lost.
    pub fn is_lost(&self) -> bool {
        self.lost.get()
    }

    /// Whether `R32Float` textures are filterable on this device (the depth
    /// plane's storage is sampled through the shared filtering layout).
    pub fn float32_filterable(&self) -> bool {
        self.float32_filterable
    }

    /// Number of resources parked in the retire queue.
    pub fn retired_count(&self) -> usize {
        self.retire_queue.len()
    }

    /// Read back a texture's full contents (row-major, tightly packed at the
    /// format's native texel size) once the GPU has finished every earlier
    /// submission. A verification path, never a frame path.
    ///
    /// # Panics
    /// Panics if the device cannot map the read-back buffer (it was lost).
    pub async fn read_texture(&mut self, id: TextureId) -> Vec<u8> {
        let t = self.texture(id);
        let (width, height) = (t.width, t.height);
        let row = width as usize * t.format.bytes_per_texel();
        let pitch = row.next_multiple_of(COPY_ROW_ALIGN);
        let size = pitch * height as usize;
        let buffer = self.device.create_buffer(&obj(&[
            ("size", (size as f64).into()),
            ("usage", (BUFFER_MAP_READ | BUFFER_COPY_DST).into()),
        ]));
        let encoder = self.device.create_command_encoder();
        encoder.copy_texture_to_buffer(
            &obj(&[("texture", t.texture.clone())]),
            &obj(&[
                ("buffer", buffer.clone()),
                ("bytesPerRow", (pitch as u32).into()),
                ("rowsPerImage", height.into()),
            ]),
            &extent(width, height),
        );
        self.submit(&encoder);
        JsFuture::from(buffer.map_async(MAP_READ))
            .await
            .expect("mapping the read-back buffer failed");
        let mapped = Uint8Array::new(&buffer.get_mapped_range());
        let mut pitched = vec![0u8; size];
        mapped.copy_to(&mut pitched);
        buffer.unmap();
        buffer.destroy_buffer();
        let mut out = Vec::with_capacity(row * height as usize);
        for y in 0..height as usize {
            out.extend_from_slice(&pitched[y * pitch..y * pitch + row]);
        }
        self.reclaim_completed();
        out
    }

    fn buffer(&self, id: BufferId) -> &WebGpuBuffer {
        self.buffers
            .get(id.into())
            .expect("buffer handle does not resolve")
    }

    fn texture(&self, id: TextureId) -> &WebGpuTexture {
        self.textures
            .get(id.into())
            .expect("texture handle does not resolve")
    }

    fn surface(&self, id: SurfaceId) -> &WebGpuSurface {
        self.surfaces
            .get(id.into())
            .expect("surface handle does not resolve")
    }

    /// The highest epoch whose commands can no longer be recorded. Every encode
    /// submits before it returns, so only an open frame keeps its epoch live.
    fn completed_epoch(&self) -> Epoch {
        if self.acquired.is_some() {
            Epoch(self.current_epoch.0.saturating_sub(1))
        } else {
            self.current_epoch
        }
    }

    /// Release every retired resource no open frame can still reference.
    fn reclaim_completed(&mut self) {
        let fence = Fence::at(self.completed_epoch());
        self.reclaim_scratch.clear();
        self.retire_queue
            .drain_completed(fence, &mut self.reclaim_scratch);
        for i in 0..self.reclaim_scratch.len() {
            let entry = self.reclaim_scratch[i];
            match entry.kind {
                ResourceKind::Buffer => {
                    if let Some(b) = self.buffers.remove(entry.id) {
                        b.buffer.destroy_buffer();
                    }
                }
                ResourceKind::Texture => {
                    if let Some(t) = self.textures.remove(entry.id) {
                        t.texture.destroy_texture();
                    }
                }
                ResourceKind::Sampler => {
                    self.samplers.remove(entry.id);
                }
                ResourceKind::Pipeline => {
                    self.pipelines.remove(entry.id);
                }
                ResourceKind::BindGroup => {
                    self.bind_groups.remove(entry.id);
                }
            }
        }
    }

    /// Grow the uniform ring to hold at least `slots` slots. Called between
    /// submissions only, so the old ring has no unsubmitted reader.
    fn reserve_uniform_slots(&mut self, slots: usize) {
        if slots <= self.uniforms.capacity {
            return;
        }
        let capacity = slots.next_power_of_two();
        let stride = self.uniforms.stride;
        let old = std::mem::replace(
            &mut self.uniforms,
            uniform_ring(&self.device, &self.uniform_layout, capacity, stride),
        );
        old.buffer.destroy_buffer();
    }

    /// The ring offset holding `bytes` for the next submission.
    fn uniform_offset(&mut self, bytes: &[u8]) -> u32 {
        let mut payload = [0u8; InlineUniforms::MAX];
        payload[..bytes.len()].copy_from_slice(bytes);
        let ring = &mut self.uniforms;
        if let Some((offset, last)) = ring.last
            && last == payload
        {
            return offset;
        }
        debug_assert!(ring.used < ring.capacity, "uniform ring reserved too small");
        let start = ring.used * ring.stride;
        let end = start + InlineUniforms::MAX;
        if ring.scratch.len() < end {
            ring.scratch.resize(end, 0);
        }
        ring.scratch[start..end].copy_from_slice(&payload);
        ring.used += 1;
        let offset = start as u32;
        ring.last = Some((offset, payload));
        offset
    }

    /// Upload the uniform slots, submit `encoder`'s commands, and rewind the ring.
    fn submit(&mut self, encoder: &GpuCommandEncoder) {
        let ring = &mut self.uniforms;
        if ring.used > 0 {
            let len = (ring.used - 1) * ring.stride + InlineUniforms::MAX;
            self.queue
                .write_buffer(&ring.buffer, 0.0, &ring.scratch[..len]);
            ring.used = 0;
            ring.last = None;
        }
        self.submit_list.set(0, encoder.finish().into());
        self.queue.submit(&self.submit_list);
        self.submit_list.set(0, JsValue::UNDEFINED);
    }

    fn record_pass(
        &mut self,
        encoder: &GpuCommandEncoder,
        pass: &RenderPass,
        commands: &[DrawCommand],
    ) {
        let (width, height) = match pass.target {
            RenderTarget::Surface(frame) => {
                let Some(acq) = self
                    .acquired
                    .as_ref()
                    .filter(|a| a.surface == frame.surface)
                else {
                    return;
                };
                self.pass_desc.target(&acq.view, pass.load);
                let s = self.surface(frame.surface);
                (s.width, s.height)
            }
            RenderTarget::Texture(id) => {
                let t = self.texture(id);
                assert!(
                    t.render_target,
                    "texture drawn into was not created as a render target"
                );
                self.pass_desc.target(&t.view, pass.load);
                (t.width, t.height)
            }
        };
        let rp = encoder.begin_render_pass(&self.pass_desc.pass);
        rp.set_bind_group(1, &self.default_group);
        let mut bound_pipeline = None;
        let mut bound_group = None;
        let mut bound_offset = None;
        for c in commands {
            let (sx, sy, sw, sh) = match c.scissor {
                Some((x, y, w, h)) => {
                    let x = x.min(width);
                    let y = y.min(height);
                    (x, y, w.min(width - x), h.min(height - y))
                }
                None => (0, 0, width, height),
            };
            rp.set_scissor_rect(sx, sy, sw, sh);
            let has_vertex_buffer = {
                let p = self
                    .pipelines
                    .get(c.pipeline.into())
                    .expect("pipeline handle does not resolve");
                if bound_pipeline != Some(c.pipeline) {
                    rp.set_pipeline(&p.pipeline);
                    bound_pipeline = Some(c.pipeline);
                }
                p.has_vertex_buffer
            };
            if let Some(bg) = c.bind_group
                && bound_group != Some(bg)
            {
                let g = self
                    .bind_groups
                    .get(bg.into())
                    .expect("bind group handle does not resolve");
                rp.set_bind_group(1, g);
                bound_group = Some(bg);
            }
            let uniforms = c.uniforms.as_bytes();
            let offset = match bound_offset {
                Some(o) if uniforms.is_empty() => o,
                _ => self.uniform_offset(uniforms),
            };
            if bound_offset != Some(offset) {
                rp.set_bind_group_with_offsets(0, &self.uniforms.group, &[offset], 0.0, 1);
                bound_offset = Some(offset);
            }
            match c.geometry {
                Geometry::Generated { count } => {
                    if has_vertex_buffer {
                        let inst = self.buffer(c.instance_buffer);
                        rp.set_vertex_buffer(0, &inst.buffer, c.instance_offset as f64);
                    }
                    rp.draw(6, count, 0, 0);
                }
                Geometry::IndexedMesh {
                    vertex_buffer,
                    index_buffer,
                    index_format,
                    index_offset,
                    index_count,
                } => {
                    rp.set_vertex_buffer(0, &self.buffer(vertex_buffer).buffer, 0.0);
                    let format = match index_format {
                        IndexFormat::U16 => "uint16",
                        IndexFormat::U32 => "uint32",
                    };
                    rp.set_index_buffer(
                        &self.buffer(index_buffer).buffer,
                        format,
                        (index_offset as usize * index_format.size()) as f64,
                    );
                    rp.draw_indexed(index_count, 1, 0, 0, 0);
                }
            }
        }
        rp.end();
    }
}

/// A `{width, height}` extent.
fn extent(width: u32, height: u32) -> Object {
    obj(&[("width", width.into()), ("height", height.into())])
}

/// A uniform ring of `capacity` slots of `stride` bytes and its bind group.
fn uniform_ring(
    device: &GpuDevice,
    layout: &GpuBindGroupLayout,
    capacity: usize,
    stride: usize,
) -> UniformRing {
    let buffer = device.create_buffer(&obj(&[
        ("size", ((capacity * stride) as f64).into()),
        ("usage", (BUFFER_UNIFORM | BUFFER_COPY_DST).into()),
    ]));
    let group = device.create_bind_group(&obj(&[
        ("layout", layout.into()),
        (
            "entries",
            arr(&[obj(&[
                ("binding", 0.into()),
                (
                    "resource",
                    obj(&[
                        ("buffer", buffer.clone()),
                        ("offset", 0.into()),
                        ("size", (InlineUniforms::MAX as u32).into()),
                    ])
                    .into(),
                ),
            ])
            .into()])
            .into(),
        ),
    ]));
    UniformRing {
        buffer,
        group,
        capacity,
        stride,
        scratch: Vec::with_capacity(capacity * stride),
        used: 0,
        last: None,
    }
}

/// A group-1 bind group: `tex`, `dst_tex`, `samp`.
fn texture_group(
    device: &GpuDevice,
    layout: &GpuBindGroupLayout,
    tex: &GpuTextureView,
    dst: &GpuTextureView,
    sampler: &GpuSampler,
) -> GpuBindGroup {
    let entry = |binding: u32, resource: JsValue| {
        obj(&[("binding", binding.into()), ("resource", resource)]).into()
    };
    device.create_bind_group(&obj(&[
        ("layout", layout.into()),
        (
            "entries",
            arr(&[
                entry(0, tex.into()),
                entry(1, dst.into()),
                entry(2, sampler.into()),
            ])
            .into(),
        ),
    ]))
}

fn sampler_descriptor(desc: &SamplerDesc) -> Object {
    let (filter, mip) = match desc.filter {
        FilterMode::Nearest => ("nearest", "nearest"),
        FilterMode::Linear => ("linear", "nearest"),
        FilterMode::MipmapLinear => ("linear", "linear"),
    };
    let address = match desc.address {
        AddressMode::ClampToEdge => "clamp-to-edge",
        AddressMode::Repeat => "repeat",
        AddressMode::Mirror => "mirror-repeat",
    };
    obj(&[
        ("magFilter", filter.into()),
        ("minFilter", filter.into()),
        ("mipmapFilter", mip.into()),
        ("addressModeU", address.into()),
        ("addressModeV", address.into()),
        ("addressModeW", address.into()),
    ])
}

/// The WebGPU texture format for `format`.
fn gpu_format(format: TextureFormat) -> &'static str {
    match format {
        TextureFormat::Bgra8Unorm => "bgra8unorm",
        // Premultiplication is a content convention, not a storage property, so
        // a data plane and a color plane of the same width share one format.
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Data => "rgba8unorm",
        TextureFormat::R8Unorm => "r8unorm",
        TextureFormat::Rgba16Float => "rgba16float",
        // A depth texture cannot be sampled through a float layout; the plane is
        // stored as a colour float instead, same bytes per texel.
        TextureFormat::Depth32Float => "r32float",
    }
}

/// The WebGPU vertex format for an attribute.
fn attr_format(format: AttrFormat) -> &'static str {
    match format {
        AttrFormat::Float1 => "float32",
        AttrFormat::Float2 => "float32x2",
        AttrFormat::Float3 => "float32x3",
        AttrFormat::Float4 => "float32x4",
        AttrFormat::Uint1 => "uint32",
        AttrFormat::Uint2 => "uint32x2",
        AttrFormat::Uint4 => "uint32x4",
    }
}

impl GpuBackend for WebGpuBackend {
    const SHADER_LANG: ShaderLang = ShaderLang::Wgsl;

    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId {
        // `writeBuffer` moves whole 4-byte words.
        let len = desc.size.max(4).next_multiple_of(4);
        let buffer = self.device.create_buffer(&obj(&[
            ("size", (len as f64).into()),
            (
                "usage",
                (BUFFER_VERTEX | BUFFER_INDEX | BUFFER_COPY_DST).into(),
            ),
        ]));
        self.buffers.insert(WebGpuBuffer { buffer, len }).into()
    }

    fn create_texture(&mut self, desc: &TextureDesc) -> TextureId {
        let (width, height) = (desc.width.max(1), desc.height.max(1));
        let mut usage = TEXTURE_BINDING | TEXTURE_COPY_DST | TEXTURE_COPY_SRC;
        if desc.render_target {
            usage |= TEXTURE_RENDER_ATTACHMENT;
        }
        // WebGPU zero-initializes every texture, so an unwritten one reads
        // transparent black.
        let texture = self.device.create_texture(&obj(&[
            ("size", extent(width, height).into()),
            ("format", gpu_format(desc.format).into()),
            ("usage", usage.into()),
            ("label", desc.label.into()),
        ]));
        let view = texture.create_view();
        self.textures
            .insert(WebGpuTexture {
                texture,
                view,
                format: desc.format,
                width,
                height,
                render_target: desc.render_target,
            })
            .into()
    }

    fn create_sampler(&mut self, desc: &SamplerDesc) -> SamplerId {
        let sampler = self.device.create_sampler(&sampler_descriptor(desc));
        self.samplers.insert(sampler).into()
    }

    fn create_pipeline(
        &mut self,
        desc: &PipelineDesc,
        layout: &InstanceLayout,
    ) -> Result<PipelineId, crate::instance::LayoutError> {
        layout.validate_against(&desc.instance_schema)?;
        let ShaderCode::Wgsl(source) = desc.code else {
            panic!(
                "the WebGPU backend consumes WGSL, got {:?}",
                desc.code.lang()
            );
        };
        let module = self
            .modules
            .entry(source.as_ptr() as usize)
            .or_insert_with(|| {
                self.device
                    .create_shader_module(&obj(&[("code", source.into())]))
            })
            .clone();

        let step_mode = match desc.builtin {
            BuiltinShader::Path | BuiltinShader::Mesh => "vertex",
            _ => "instance",
        };
        let attributes = Array::new();
        let mut offset = 0u32;
        for (location, a) in desc.instance_schema.attributes.iter().enumerate() {
            attributes.push(&obj(&[
                ("format", attr_format(a.format).into()),
                ("offset", offset.into()),
                ("shaderLocation", (location as u32).into()),
            ]));
            offset += a.format.size() as u32;
        }
        let has_vertex_buffer = attributes.length() > 0;
        let buffers = if has_vertex_buffer {
            arr(&[obj(&[
                ("arrayStride", (layout.stride as u32).into()),
                ("stepMode", step_mode.into()),
                ("attributes", attributes.into()),
            ])
            .into()])
        } else {
            Array::new()
        };
        let target = obj(&[("format", gpu_format(desc.color_format).into())]);
        if desc.blend == BlendMode::PremultipliedOver {
            let over = || {
                obj(&[
                    ("operation", "add".into()),
                    ("srcFactor", "one".into()),
                    ("dstFactor", "one-minus-src-alpha".into()),
                ])
            };
            set(
                &target,
                "blend",
                obj(&[("color", over().into()), ("alpha", over().into())]),
            );
        }
        let pipeline = self.device.create_render_pipeline(&obj(&[
            ("label", desc.label.into()),
            ("layout", self.pipeline_layout.clone()),
            (
                "vertex",
                obj(&[
                    ("module", module.clone()),
                    ("entryPoint", desc.vertex_entry.into()),
                    ("buffers", buffers.into()),
                ])
                .into(),
            ),
            (
                "fragment",
                obj(&[
                    ("module", module),
                    ("entryPoint", desc.fragment_entry.into()),
                    ("targets", arr(&[target.into()]).into()),
                ])
                .into(),
            ),
            (
                "primitive",
                obj(&[
                    ("topology", "triangle-list".into()),
                    ("cullMode", "none".into()),
                ])
                .into(),
            ),
        ]));
        Ok(self
            .pipelines
            .insert(WebGpuPipeline {
                pipeline,
                has_vertex_buffer,
            })
            .into())
    }

    fn create_bind_group(&mut self, desc: &BindGroupDesc) -> BindGroupId {
        let mut views: [Option<&GpuTextureView>; 2] = [None; 2];
        let mut texture_count = 0;
        let mut sampler = None;
        for binding in &desc.bindings {
            match *binding {
                Binding::Texture(id) => {
                    if texture_count < views.len() {
                        views[texture_count] = Some(&self.texture(id).view);
                        texture_count += 1;
                    }
                }
                Binding::Sampler(id) => {
                    sampler = Some(
                        self.samplers
                            .get(id.into())
                            .expect("sampler handle does not resolve"),
                    );
                }
                // The built-ins' uniforms live in the shared uniform ring.
                Binding::Uniform(_) => {}
            }
        }
        // The layout is complete for every program: a single-texture program
        // leaves `dst_tex` unread, so it repeats `tex`.
        let tex = views[0].unwrap_or(&self.default_view);
        let dst = views[1].unwrap_or(tex);
        let group = texture_group(
            &self.device,
            &self.texture_layout,
            tex,
            dst,
            sampler.unwrap_or(&self.default_sampler),
        );
        self.bind_groups.insert(group).into()
    }

    fn destroy_buffer(&mut self, id: BufferId) {
        if self.buffers.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::Buffer, id.into(), self.current_epoch);
        }
    }

    fn destroy_texture(&mut self, id: TextureId) {
        if self.textures.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::Texture, id.into(), self.current_epoch);
        }
    }

    fn destroy_sampler(&mut self, id: SamplerId) {
        if self.samplers.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::Sampler, id.into(), self.current_epoch);
        }
    }

    fn destroy_pipeline(&mut self, id: PipelineId) {
        if self.pipelines.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::Pipeline, id.into(), self.current_epoch);
        }
    }

    fn destroy_bind_group(&mut self, id: BindGroupId) {
        if self.bind_groups.get(id.into()).is_some() {
            self.retire_queue
                .retire(ResourceKind::BindGroup, id.into(), self.current_epoch);
        }
    }

    fn write_buffer(&mut self, id: BufferId, offset: usize, bytes: &[u8]) {
        let buf = self.buffer(id);
        assert!(
            offset + bytes.len() <= buf.len,
            "write_buffer out of range: {} + {} > {}",
            offset,
            bytes.len(),
            buf.len
        );
        assert!(
            offset.is_multiple_of(4),
            "write_buffer offset {offset} is not 4-byte aligned"
        );
        if bytes.is_empty() {
            return;
        }
        let whole = bytes.len() & !3;
        if whole > 0 {
            self.queue
                .write_buffer(&buf.buffer, offset as f64, &bytes[..whole]);
        }
        if whole < bytes.len() {
            // The trailing partial word, zero-padded: the buffer is sized in whole
            // words, so the padding stays inside it.
            let mut word = [0u8; 4];
            word[..bytes.len() - whole].copy_from_slice(&bytes[whole..]);
            self.queue
                .write_buffer(&buf.buffer, (offset + whole) as f64, &word);
        }
    }

    fn write_texture(&mut self, id: TextureId, x: u32, y: u32, w: u32, h: u32, bytes: &[u8]) {
        let t = self.texture(id);
        let bytes_per_row = w as usize * t.format.bytes_per_texel();
        let len = bytes_per_row * h as usize;
        assert!(
            bytes.len() >= len,
            "write_texture: {} bytes < {}x{} region of {}-byte texels",
            bytes.len(),
            w,
            h,
            t.format.bytes_per_texel()
        );
        if len == 0 {
            return;
        }
        self.queue.write_texture(
            &obj(&[
                ("texture", t.texture.clone()),
                ("origin", obj(&[("x", x.into()), ("y", y.into())]).into()),
            ]),
            &bytes[..len],
            &obj(&[
                ("bytesPerRow", (bytes_per_row as u32).into()),
                ("rowsPerImage", h.into()),
            ]),
            &extent(w, h),
        );
    }

    fn create_surface(&mut self, raw: RawWindowHandle, width: u32, height: u32) -> SurfaceId {
        let RawWindowHandle::WebCanvas { canvas_id } = raw else {
            panic!("WebGpuBackend cannot present to {raw:?}");
        };
        let document: Document = Reflect::get(&js_sys::global(), &"document".into())
            .expect("a document to find the canvas in")
            .unchecked_into();
        let canvas = document.query_selector(&format!("canvas[data-viso-canvas=\"{canvas_id}\"]"));
        assert!(
            !canvas.is_null(),
            "no <canvas data-viso-canvas=\"{canvas_id}\"> in the document"
        );
        let canvas: Canvas = canvas.unchecked_into();
        canvas.set_width(width.max(1));
        canvas.set_height(height.max(1));
        let context = canvas.get_context("webgpu");
        assert!(!context.is_null(), "the canvas has no WebGPU context");
        let context: GpuCanvasContext = context.unchecked_into();
        context
            .configure(&obj(&[
                ("device", self.device.clone()),
                ("format", gpu_format(self.preferred).into()),
                ("usage", TEXTURE_RENDER_ATTACHMENT.into()),
                ("alphaMode", "opaque".into()),
            ]))
            .expect("failed to configure the WebGPU canvas context");
        self.surfaces
            .insert(WebGpuSurface {
                canvas,
                context,
                format: self.preferred,
                width: width.max(1),
                height: height.max(1),
            })
            .into()
    }

    fn resize_surface(&mut self, id: SurfaceId, width: u32, height: u32) {
        let s = self
            .surfaces
            .get_mut(id.into())
            .expect("surface handle does not resolve");
        let (width, height) = (width.max(1), height.max(1));
        if (s.width, s.height) != (width, height) {
            s.canvas.set_width(width);
            s.canvas.set_height(height);
            s.width = width;
            s.height = height;
        }
    }

    fn destroy_surface(&mut self, id: SurfaceId) {
        if self.acquired.as_ref().is_some_and(|a| a.surface == id) {
            self.acquired = None;
        }
        if let Some(s) = self.surfaces.remove(id.into()) {
            s.context.unconfigure();
        }
    }

    fn begin_frame(&mut self, surface: SurfaceId) -> Option<Frame> {
        if self.lost.get() {
            return None;
        }
        if let Some(open) = self.acquired.as_ref().map(|a| a.surface) {
            // A frame begun and never presented: drop it as a lost frame.
            self.device_lost(open);
        }
        self.reclaim_completed();
        let texture = self.surface(surface).context.get_current_texture().ok()?;
        self.acquired = Some(Acquired {
            surface,
            view: texture.create_view(),
        });
        self.current_epoch = self.current_epoch.next();
        Some(Frame {
            surface,
            drawable: 0,
        })
    }

    fn encode(&mut self, list: &DrawList<'_>) {
        if self.lost.get() {
            return;
        }
        // One slot per draw plus one per pass (a pass whose first draw carries no
        // uniforms still binds one) bounds this encode's ring use.
        self.reserve_uniform_slots(list.commands.len() + list.passes.len());
        let encoder = self.device.create_command_encoder();
        for pass in list.passes {
            self.record_pass(&encoder, pass, &list.commands[pass.command_range()]);
        }
        self.submit(&encoder);
        if self.acquired.is_none() {
            // Offscreen-only work never reaches `begin_frame`, so reclaim here too.
            self.reclaim_completed();
        }
    }

    fn present(&mut self, frame: Frame) {
        // The browser presents the canvas when this task returns.
        if self
            .acquired
            .as_ref()
            .is_some_and(|a| a.surface == frame.surface)
        {
            self.acquired = None;
        }
    }

    fn device_lost(&mut self, _surface: SurfaceId) {
        // Every encode has already been submitted; the abandoned canvas texture
        // is simply not drawn into again.
        self.acquired = None;
        self.reclaim_completed();
    }

    fn caps(&self) -> &Caps {
        &self.caps
    }

    fn surface_format(&self, surface: SurfaceId) -> TextureFormat {
        self.surface(surface).format
    }
}

impl Drop for WebGpuBackend {
    fn drop(&mut self) {
        for b in self.buffers.drain() {
            b.buffer.destroy_buffer();
        }
        for t in self.textures.drain() {
            t.texture.destroy_texture();
        }
        self.uniforms.buffer.destroy_buffer();
        self.default_texture.destroy_texture();
        self.device
            .remove_event_listener("uncapturederror", self.on_error.as_ref().unchecked_ref());
        self.device.destroy_device();
    }
}
