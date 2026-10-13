//! A view's assets in its release package: the package carries the bytes of
//! every asset the view names, a node showing one offers them to its store the
//! first time it shows it, and a state the source reads switches the resource
//! the node shows. A shader view's brush samples the images its value names
//! the same way.

mod support;

use std::path::{Path, PathBuf};
use std::rc::Rc;

use support::Rt;
use viso_dsl::assets::AssetCatalog;
use viso_dsl::hir::TargetProfile;
use viso_ui::resource::{ResourceId, ResourceRevision};
use viso_ui::{AssetDraw, AssetFit, Content, NodeId};

const SOURCE: &str = r#"
component Gallery {
    state dark = false;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Text { width: 20dp; height: 20dp; on click { dark = !dark; } }
            Image {
                source: if dark { asset("images/dark.png") } else { asset("images/light.png") };
                fit: ImageFit::cover;
                width: 64dp;
                height: 64dp;
            }
            Svg { source: asset("icons/close.svg"); tint: #ff0000; width: 16dp; height: 16dp; }
        }
    }
}
"#;

const LIGHT: &[u8] = b"\x89PNG\r\n\x1a\nlight";
const DARK: &[u8] = b"\x89PNG\r\n\x1a\ndark";
const CLOSE: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\"/>";

/// A package directory holding the view's assets, removed on drop.
struct Assets(PathBuf);

impl Assets {
    fn new(name: &str) -> Assets {
        let dir =
            std::env::temp_dir().join(format!("viso-view-assets-{name}-{}", std::process::id()));
        for (path, bytes) in [
            ("images/light.png", LIGHT),
            ("images/dark.png", DARK),
            ("icons/close.svg", CLOSE),
        ] {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        Assets(dir)
    }

    fn profile(&self) -> TargetProfile {
        TargetProfile {
            assets: Some(Rc::new(AssetCatalog::scan(&self.0).unwrap())),
            ..TargetProfile::default()
        }
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Assets {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What node `node` shows: its resource, fit and tint.
fn shown(rt: &Rt, node: NodeId) -> (ResourceId, AssetFit, bool) {
    match rt.store.content_payload(node) {
        Some(Content::Asset {
            resource,
            draw,
            fit,
            tint,
            ..
        }) => {
            assert_eq!(*draw, AssetDraw::Nothing, "nothing is decoded here");
            (*resource, *fit, tint.is_some())
        }
        other => panic!("{other:?}"),
    }
}

/// The bytes the store holds to decode, by resource.
fn offered(rt: &Rt) -> Vec<(ResourceId, Vec<u8>)> {
    let mut offered: Vec<_> = rt
        .store
        .assets()
        .undecoded()
        .map(|(id, revision, bytes)| {
            assert_eq!(revision, ResourceRevision::default());
            (id, bytes.to_vec())
        })
        .collect();
    offered.sort();
    offered
}

#[test]
fn a_packaged_view_offers_the_assets_it_shows_and_switches_them() {
    let assets = Assets::new("switch");
    let mut rt = Rt::packaged_for(SOURCE, assets.profile());
    let nodes = rt.descendants();
    let (image, svg) = (nodes[2], nodes[3]);
    let (light, dark, close) = (
        ResourceId::of("images/light.png"),
        ResourceId::of("images/dark.png"),
        ResourceId::of("icons/close.svg"),
    );
    assert_eq!(shown(&rt, image), (light, AssetFit::Cover, false));
    assert_eq!(shown(&rt, svg), (close, AssetFit::Contain, true));
    let mut expected = vec![(light, LIGHT.to_vec()), (close, CLOSE.to_vec())];
    expected.sort();
    assert_eq!(offered(&rt), expected, "only what shows is offered");

    rt.click(0);
    assert_eq!(shown(&rt, image), (dark, AssetFit::Cover, false));
    expected.push((dark, DARK.to_vec()));
    expected.sort();
    assert_eq!(offered(&rt), expected);
    drop(assets);
}

const LENS: &str = r#"
shader Lens {
    uniform viewport_size: Vec2F32;
    instance rect_pos: Vec2F32;
    instance rect_size: Vec2F32;
    texture photo: Texture2D<Vec4F32>;
    sampler smooth: Sampler;
    varying uv: Vec2F32;
    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        uv = unit;
        return VertexOutput {
            clip_position: to_clip(rect_pos + unit * rect_size, viewport_size),
        };
    }
    fragment() -> Vec4F32 {
        return sample(photo, smooth, uv);
    }
}

component Viewer {
    state dark = false;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Text { width: 20dp; height: 20dp; on click { dark = !dark; } }
            ShaderView {
                shader: Lens { photo: if dark { asset("images/dark.png") } else { asset("images/light.png") } };
                width: 64dp;
                height: 64dp;
            }
        }
    }
}
"#;

#[test]
fn a_packaged_shader_view_offers_the_images_its_brush_samples() {
    let assets = Assets::new("shader");
    let mut rt = Rt::packaged_for(LENS, assets.profile());
    let lens = rt.descendants()[2];
    let images = |rt: &Rt| match rt.store.content_payload(lens) {
        Some(Content::Shader { images, .. }) => images
            .iter()
            .map(|image| {
                assert_eq!(image.texture, None, "nothing is resident here");
                image.resource
            })
            .collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let (light, dark) = (
        ResourceId::of("images/light.png"),
        ResourceId::of("images/dark.png"),
    );
    assert_eq!(images(&rt), [light]);
    assert_eq!(offered(&rt), [(light, LIGHT.to_vec())]);

    rt.click(0);
    assert_eq!(images(&rt), [dark]);
    let mut expected = vec![(light, LIGHT.to_vec()), (dark, DARK.to_vec())];
    expected.sort();
    assert_eq!(offered(&rt), expected);
    drop(assets);
}

#[test]
fn an_asset_that_cannot_be_read_fails_the_package() {
    let assets = Assets::new("unreadable");
    let profile = assets.profile();
    std::fs::remove_file(assets.path().join("icons/close.svg")).unwrap();
    let errors = viso_dsl::aot::build_view_package_for(SOURCE, &support::origin(), profile)
        .expect_err("a missing asset");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code, "E3713");
    assert!(
        errors[0].message.contains("assets/icons/close.svg"),
        "{errors:?}"
    );
}
