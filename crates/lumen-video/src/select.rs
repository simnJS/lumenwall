//! Choice of the hardware decoders to try, from `--hwdec` and the GPU wgpu renders on
//! (pure, unit tested).
//!
//! The decoder must run on the rendering GPU: decoding on another GPU (the AMD iGPU of a
//! machine that renders on NVIDIA) would read its frames back through system memory.

use crate::{HwDecode, HwKind};

/// PCI vendor ids.
pub const VENDOR_NVIDIA: u32 = 0x10de;
pub const VENDOR_AMD: u32 = 0x1002;
pub const VENDOR_INTEL: u32 = 0x8086;

/// The rendering adapter, as far as decoder selection is concerned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdapterHint {
    /// PCI vendor id (0 = unknown).
    pub vendor: u32,
    /// PCI device id.
    pub device: u32,
    /// PCI bus id (`0000:01:00.0`), empty when unknown.
    pub pci_bus_id: String,
    /// Adapter name.
    pub name: String,
}

impl AdapterHint {
    #[must_use]
    pub fn from_wgpu(info: &wgpu::AdapterInfo) -> Self {
        Self {
            vendor: info.vendor,
            device: info.device,
            pci_bus_id: info.device_pci_bus_id.clone(),
            name: info.name.clone(),
        }
    }
}

/// `LUMEN_VIDEO_DECODERS=zerocopy,cuda,vulkan,vaapi,d3d11va` (debugging, measurements):
/// decoder order used by `--hwdec auto` instead of the built-in one; unknown names are
/// ignored.
#[must_use]
pub fn parse_decoder_list(list: &str) -> Vec<HwKind> {
    list.split(',')
        .filter_map(|n| match n.trim().to_ascii_lowercase().as_str() {
            "cuda" | "nvdec" => Some(HwKind::Cuda),
            "vaapi" => Some(HwKind::Vaapi),
            "vulkan" => Some(HwKind::Vulkan),
            "d3d11va" => Some(HwKind::D3d11va),
            _ => None,
        })
        .collect()
}

/// Whether `--hwdec auto` may try FFmpeg's Vulkan decoder on the render device (zero copy):
/// always, unless `LUMEN_VIDEO_DECODERS` is set without the `zerocopy` entry.
#[must_use]
pub fn zero_copy_allowed() -> bool {
    std::env::var("LUMEN_VIDEO_DECODERS").map_or(true, |list| {
        list.split(',')
            .any(|n| n.trim().eq_ignore_ascii_case("zerocopy"))
    })
}

/// Hardware decoders to try after the zero-copy one, best first; software decoding always
/// follows.
#[must_use]
pub fn candidates(policy: HwDecode, adapter: &AdapterHint, windows: bool) -> Vec<HwKind> {
    if policy == HwDecode::Auto
        && let Ok(list) = std::env::var("LUMEN_VIDEO_DECODERS")
    {
        return parse_decoder_list(&list);
    }
    match policy {
        HwDecode::No => Vec::new(),
        HwDecode::NvdecCopy => vec![HwKind::Cuda],
        HwDecode::Auto if windows => vec![HwKind::D3d11va],
        HwDecode::Auto => match adapter.vendor {
            VENDOR_NVIDIA => vec![HwKind::Cuda, HwKind::Vulkan],
            VENDOR_AMD | VENDOR_INTEL => vec![HwKind::Vaapi, HwKind::Vulkan],
            // Software adapters (lavapipe, WARP) and unknown GPUs: whatever exists.
            _ => vec![HwKind::Vaapi, HwKind::Cuda, HwKind::Vulkan],
        },
    }
}

/// One DRM render node as seen in sysfs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderNode {
    /// `/dev/dri/renderD128`.
    pub path: String,
    /// PCI vendor id.
    pub vendor: u32,
    /// Last component of the canonical sysfs device path (PCI bus id for PCI GPUs).
    pub bus_id: String,
}

/// Render node of the rendering adapter: same PCI bus id, else the first one of the same
/// vendor.
#[must_use]
pub fn pick_render_node<'a>(
    nodes: &'a [RenderNode],
    adapter: &AdapterHint,
) -> Option<&'a RenderNode> {
    let bus = adapter.pci_bus_id.to_ascii_lowercase();
    if !bus.is_empty()
        && let Some(n) = nodes.iter().find(|n| n.bus_id.to_ascii_lowercase() == bus)
    {
        return Some(n);
    }
    nodes
        .iter()
        .find(|n| adapter.vendor != 0 && n.vendor == adapter.vendor)
}

/// Render nodes of this machine (Linux sysfs; empty elsewhere).
#[must_use]
pub fn render_nodes() -> Vec<RenderNode> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/sys/class/drm") else {
        return out;
    };
    let mut names: Vec<String> = dir
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("renderD"))
        .collect();
    names.sort();
    for name in names {
        let dev = std::path::Path::new("/sys/class/drm")
            .join(&name)
            .join("device");
        let vendor = std::fs::read_to_string(dev.join("vendor"))
            .ok()
            .and_then(|v| u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);
        let bus_id = std::fs::canonicalize(&dev)
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
            .unwrap_or_default();
        out.push(RenderNode {
            path: format!("/dev/dri/{name}"),
            vendor,
            bus_id,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(vendor: u32, bus: &str) -> AdapterHint {
        AdapterHint {
            vendor,
            pci_bus_id: bus.into(),
            ..AdapterHint::default()
        }
    }

    #[test]
    fn candidates_follow_the_rendering_gpu() {
        if std::env::var_os("LUMEN_VIDEO_DECODERS").is_some() {
            return;
        }
        let nv = hint(VENDOR_NVIDIA, "");
        let amd = hint(VENDOR_AMD, "");
        assert_eq!(
            candidates(HwDecode::Auto, &nv, false),
            vec![HwKind::Cuda, HwKind::Vulkan]
        );
        assert_eq!(
            candidates(HwDecode::Auto, &amd, false),
            vec![HwKind::Vaapi, HwKind::Vulkan]
        );
        assert_eq!(
            candidates(HwDecode::Auto, &amd, true),
            vec![HwKind::D3d11va]
        );
        assert_eq!(
            candidates(HwDecode::NvdecCopy, &amd, false),
            vec![HwKind::Cuda]
        );
        assert!(candidates(HwDecode::No, &nv, false).is_empty());
    }

    #[test]
    fn decoder_list() {
        assert_eq!(
            parse_decoder_list("zerocopy, Vulkan,nvdec,bogus"),
            vec![HwKind::Vulkan, HwKind::Cuda]
        );
    }

    #[test]
    fn render_node_by_bus_then_vendor() {
        let nodes = vec![
            RenderNode {
                path: "/dev/dri/renderD128".into(),
                vendor: VENDOR_NVIDIA,
                bus_id: "0000:01:00.0".into(),
            },
            RenderNode {
                path: "/dev/dri/renderD129".into(),
                vendor: VENDOR_AMD,
                bus_id: "0000:7c:00.0".into(),
            },
        ];
        let amd = hint(VENDOR_AMD, "0000:7C:00.0");
        assert_eq!(
            pick_render_node(&nodes, &amd).map(|n| n.path.as_str()),
            Some("/dev/dri/renderD129")
        );
        let amd_no_bus = hint(VENDOR_AMD, "");
        assert_eq!(
            pick_render_node(&nodes, &amd_no_bus).map(|n| n.path.as_str()),
            Some("/dev/dri/renderD129")
        );
        assert_eq!(pick_render_node(&nodes, &hint(VENDOR_INTEL, "")), None);
    }
}
