//! The Metal backend presents through a `CAMetalLayer` hosted in a `UIView`:
//! surface creation, frame acquisition, a cleared surface pass, present, and a
//! resize that rebuilds the drawable pool. Runs on the iOS simulator
//! (`xcrun simctl spawn booted` as the cargo runner).

#![cfg(target_os = "ios")]

use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use viso_gpu::backend::{DrawList, LoadOp, RenderPass, RenderTarget};
use viso_gpu::{GpuBackend, MetalBackend, RawWindowHandle, TextureFormat};

#[link(name = "UIKit", kind = "framework")]
unsafe extern "C" {}

/// A detached `UIView` of `w`×`h` points, leaked for the test's lifetime.
fn ui_view(w: f64, h: f64) -> *mut AnyObject {
    let class = AnyClass::get(c"UIView").expect("UIKit is linked");
    let frame = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: w,
            height: h,
        },
    };
    // SAFETY: `UIView` responds to `alloc` and `initWithFrame:`; the +1 reference
    // the pair returns is never released, so the view outlives the surface.
    unsafe {
        let view: *mut AnyObject = msg_send![class, alloc];
        let view: *mut AnyObject = msg_send![view, initWithFrame: frame];
        assert!(!view.is_null());
        view
    }
}

fn clear_frame(gpu: &mut MetalBackend, surface: viso_gpu::SurfaceId) {
    let frame = gpu
        .begin_frame(surface)
        .expect("a UIView-hosted layer vends drawables");
    let passes = [RenderPass {
        target: RenderTarget::Surface(frame),
        load: LoadOp::Clear([1.0, 0.0, 0.0, 1.0]),
        first_command: 0,
        command_count: 0,
    }];
    gpu.encode(&DrawList {
        commands: &[],
        passes: &passes,
    });
    gpu.present(frame);
}

#[test]
fn uikit_view_surface_acquires_clears_presents_and_resizes() {
    let mut gpu = MetalBackend::new();
    let view = ui_view(32.0, 16.0);
    let surface = gpu.create_surface(
        RawWindowHandle::UiKit {
            ui_view: view.cast(),
        },
        32,
        16,
    );
    assert_eq!(gpu.surface_format(surface), TextureFormat::Bgra8Unorm);
    for _ in 0..4 {
        clear_frame(&mut gpu, surface);
    }
    gpu.resize_surface(surface, 64, 48);
    for _ in 0..4 {
        clear_frame(&mut gpu, surface);
    }
}
