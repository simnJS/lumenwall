//! Restricts the Vulkan loader to the driver of the GPU used last time (Linux and Windows).
//!
//! Without a restriction the loader opens every installed driver to enumerate the GPUs.
//! Linux: the Mesa ones stay mapped (measured with 9 ICDs installed and an NVIDIA GPU:
//! `libLLVM`, RADV and lavapipe, ≈ 50 MiB RSS, 25–40 ms more per Vulkan instance, created at
//! start and at every GPU restore). Windows (RTX 4080 SUPER and an AMD iGPU without monitor):
//! the AMD driver is loaded and enumerated, creates `%LOCALAPPDATA%\AMD\VkCache`, and costs
//! ≈ 30 MiB working set and 12–23 ms per instance.
//!
//! The engine remembers the driver of the adapter it used (`<cache>/vulkan-driver`: manifest
//! glob, PCI vendor and device ids) and [`restrict_vulkan_loader`], called first thing in
//! `main`, sets `VK_LOADER_DRIVERS_SELECT` to that glob for the whole process. On Windows the
//! manifest name comes from the adapter's registry key (`nv-vk64.json`, `amd-vulkan64.json`…,
//! [`lumenwall_platform::vulkan_registry`]), and the implicit layers registered by the other
//! adapters' drivers are turned off with `VK_LOADER_LAYERS_DISABLE`: AMD's "switchable
//! graphics" layer lives in `amdvlk64.dll`, which the loader maps into every Vulkan process
//! otherwise, whatever driver is selected (measured). When the restricted loader finds no
//! adapter, or not the remembered one (GPU removed, driver changed), the variables are removed
//! and the instance created again with every driver ([`lift`]); the new choice is remembered.
//!
//! Nothing is done when the user sets one of the loader's driver variables; a user's
//! `VK_LOADER_LAYERS_DISABLE` is kept as it is.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lumenwall_platform::vulkan_registry::AdapterEntry;

const SELECT: &str = "VK_LOADER_DRIVERS_SELECT";
const LAYERS_DISABLE: &str = "VK_LOADER_LAYERS_DISABLE";
/// Loader variables that choose drivers: set by the user, they win.
const USER_VARS: [&str; 5] = [
    SELECT,
    "VK_LOADER_DRIVERS_DISABLE",
    "VK_DRIVER_FILES",
    "VK_ICD_FILENAMES",
    "VK_ADD_DRIVER_FILES",
];

/// Whether this OS gets a restriction.
const SUPPORTED: bool = cfg!(any(target_os = "linux", windows));

/// The adapter of the last run and its driver's manifest glob.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Remembered {
    glob: String,
    vendor: u32,
    device: u32,
}

impl Remembered {
    fn parse(text: &str) -> Option<Self> {
        let mut it = text.split_whitespace();
        let glob = it.next()?;
        if !valid_glob(glob) {
            return None;
        }
        let hex = |s: Option<&str>| u32::from_str_radix(s?, 16).ok();
        Some(Self {
            glob: glob.to_owned(),
            vendor: hex(it.next())?,
            device: hex(it.next())?,
        })
    }

    fn format(&self) -> String {
        format!("{} {:04x} {:04x}\n", self.glob, self.vendor, self.device)
    }
}

/// A glob of the manifest name only: no separator the loader would read as a list, no space
/// (the memory file is split on spaces).
fn valid_glob(glob: &str) -> bool {
    !glob.is_empty()
        && glob.len() <= 64
        && !glob.contains(',')
        && !glob.chars().any(char::is_whitespace)
}

/// What this process did with the loader.
#[derive(Debug, Default)]
enum State {
    /// Nothing decided yet: no memory, or another OS.
    #[default]
    Free,
    /// The user chose drivers through the environment: leave everything alone.
    User,
    /// `VK_LOADER_DRIVERS_SELECT` (and `VK_LOADER_LAYERS_DISABLE` when `layers`) set by
    /// [`restrict_vulkan_loader`].
    Restricted {
        remembered: Remembered,
        layers: bool,
    },
    /// The restriction was lifted (or there was none): every driver is visible.
    Lifted,
}

static STATE: Mutex<State> = Mutex::new(State::Free);

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn memory_file() -> Option<PathBuf> {
    crate::cache_dir().map(|d| d.join("vulkan-driver"))
}

/// What to set for a remembered adapter.
#[derive(Debug, PartialEq, Eq)]
struct Restriction {
    glob: String,
    /// Implicit layers of the other adapters' drivers.
    layers_off: Vec<String>,
}

/// Restriction for `remembered`, given the Windows adapter registrations (`entries`, empty on
/// Linux) and a reader of layer manifests. The registry's current manifest name wins over the
/// remembered one (a driver update may rename it). Pure, for testing.
fn restriction(
    remembered: &Remembered,
    entries: &[AdapterEntry],
    read: &dyn Fn(&Path) -> Option<String>,
) -> Restriction {
    let glob = manifest_glob(entries, remembered.vendor, remembered.device)
        .unwrap_or_else(|| remembered.glob.clone());
    Restriction {
        glob,
        layers_off: foreign_layers(entries, remembered.vendor, remembered.device, read),
    }
}

/// Glob of the driver manifest registered for the adapter `vendor:device` (Windows).
fn manifest_glob(entries: &[AdapterEntry], vendor: u32, device: u32) -> Option<String> {
    let entry = entries
        .iter()
        .find(|e| (e.vendor, e.device) == (vendor, device))?;
    let name = entry.drivers.first()?.file_name()?.to_str()?;
    let glob = format!("*{name}");
    valid_glob(&glob).then_some(glob)
}

/// Names of the implicit layers that the drivers of the other adapters register, minus those
/// the adapter `vendor:device` registers itself.
fn foreign_layers(
    entries: &[AdapterEntry],
    vendor: u32,
    device: u32,
    read: &dyn Fn(&Path) -> Option<String>,
) -> Vec<String> {
    let (own, others): (Vec<&AdapterEntry>, Vec<&AdapterEntry>) = entries
        .iter()
        .partition(|e| (e.vendor, e.device) == (vendor, device));
    if own.is_empty() {
        // The remembered adapter is not registered (removed?): turn nothing off.
        return Vec::new();
    }
    let names_of = |paths: &mut dyn Iterator<Item = &PathBuf>| -> Vec<String> {
        paths
            .filter_map(|p| read(p))
            .flat_map(|json| layer_names(&json))
            .collect()
    };
    let kept = names_of(&mut own.iter().flat_map(|e| e.implicit_layers.iter()));
    let mut off: Vec<String> = names_of(&mut others.iter().flat_map(|e| e.implicit_layers.iter()))
        .into_iter()
        .filter(|n| !kept.contains(n))
        .collect();
    off.sort();
    off.dedup();
    off
}

/// Layer names of a layer manifest (`"layer": {…}` or `"layers": [{…}]`). Names that the
/// loader's list syntax could misread are left out.
fn layer_names(json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let one = v.get("layer").into_iter();
    let many = v
        .get("layers")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten();
    one.chain(many)
        .filter_map(|l| l.get("name")?.as_str())
        .filter(|n| {
            !n.is_empty()
                && n.len() <= 128
                && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .map(str::to_owned)
        .collect()
}

/// Sets `VK_LOADER_DRIVERS_SELECT` to the driver of the GPU used last time (Windows: also
/// `VK_LOADER_LAYERS_DISABLE` to the other drivers' implicit layers), unless the user chose
/// drivers through the environment. Linux and Windows (no effect elsewhere).
///
/// Call it first thing in `main`, before any thread exists: it changes the process
/// environment.
pub fn restrict_vulkan_loader() {
    if !SUPPORTED {
        return;
    }
    if USER_VARS.iter().any(|v| std::env::var_os(v).is_some()) {
        *state() = State::User;
        return;
    }
    let Some(remembered) = memory_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| Remembered::parse(&t))
    else {
        return;
    };
    let entries = lumenwall_platform::vulkan_registry::adapter_entries();
    let plan = restriction(&remembered, &entries, &|p| std::fs::read_to_string(p).ok());
    let layers = !plan.layers_off.is_empty() && std::env::var_os(LAYERS_DISABLE).is_none();
    // SAFETY: called at the start of `main`, before any other thread exists (documented
    // contract), so nothing reads the environment concurrently.
    unsafe {
        std::env::set_var(SELECT, &plan.glob);
        if layers {
            std::env::set_var(LAYERS_DISABLE, plan.layers_off.join(","));
        }
    }
    *state() = State::Restricted {
        remembered: Remembered {
            glob: plan.glob,
            ..remembered
        },
        layers,
    };
}

/// Whether `ctx` was opened under the restriction on another adapter than the remembered one
/// (the remembered GPU is gone: [`lift`] and open again).
pub(crate) fn wrong_adapter(ctx: &lumenwall_render::GpuContext) -> bool {
    let info = ctx.adapter().get_info();
    match &*state() {
        State::Restricted { remembered: r, .. } => {
            (info.vendor, info.device) != (r.vendor, r.device)
        }
        _ => false,
    }
}

/// Removes the restriction for the rest of the process. Returns false when there was none
/// (nothing else to try).
pub(crate) fn lift() -> bool {
    let mut s = state();
    let State::Restricted { layers, .. } = *s else {
        return false;
    };
    // SAFETY: Linux: the variables exist (set by `restrict_vulkan_loader` before any thread),
    // so glibc removes their entries by moving the following pointers down in place: the
    // environment array is neither reallocated nor freed, and Rust readers hold std's
    // environment lock. Windows: the environment block is changed under the process lock
    // that every reader takes. The loader reads them on this thread only, while creating the
    // instance.
    unsafe {
        std::env::remove_var(SELECT);
        if layers {
            std::env::remove_var(LAYERS_DISABLE);
        }
    }
    *s = State::Lifted;
    true
}

/// Remembers the driver of `ctx`'s adapter for the next start (when the loader was not
/// restricted already, and the driver's manifest name is known).
pub(crate) fn remember(ctx: &lumenwall_render::GpuContext) {
    if !SUPPORTED || !matches!(*state(), State::Free | State::Lifted) {
        return;
    }
    let info = ctx.adapter().get_info();
    if info.backend != wgpu::Backend::Vulkan {
        return;
    }
    let glob = if cfg!(windows) {
        let entries = lumenwall_platform::vulkan_registry::adapter_entries();
        manifest_glob(&entries, info.vendor, info.device)
    } else {
        lumenwall_render::loader::adapter_manifest_glob(ctx.adapter()).map(str::to_owned)
    };
    let Some(glob) = glob else {
        return;
    };
    let new = Remembered {
        glob,
        vendor: info.vendor,
        device: info.device,
    };
    let Some(path) = memory_file() else {
        return;
    };
    let old = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| Remembered::parse(&t));
    if old.as_ref() == Some(&new) {
        return;
    }
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, new.format())?;
        std::fs::rename(&tmp, &path)
    };
    match write() {
        Ok(()) => tracing::info!(
            "Vulkan driver remembered for the next start: {} ({:04x}:{:04x})",
            new.glob,
            new.vendor,
            new.device
        ),
        Err(e) => tracing::debug!("{}: {e}", path.display()),
    }
}

/// The drivers the loader sees, for the logs.
pub(crate) fn describe() -> String {
    match &*state() {
        State::Restricted {
            remembered,
            layers: true,
        } => format!("{}, other drivers' implicit layers off", remembered.glob),
        State::Restricted { remembered, .. } => remembered.glob.clone(),
        State::User => "chosen by the environment".into(),
        State::Free | State::Lifted => "all".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_round_trip() {
        let r = Remembered {
            glob: "*nvidia*".into(),
            vendor: 0x10de,
            device: 0x2702,
        };
        assert_eq!(r.format(), "*nvidia* 10de 2702\n");
        assert_eq!(Remembered::parse(&r.format()), Some(r));
        assert_eq!(Remembered::parse(""), None);
        assert_eq!(Remembered::parse("*nvidia*"), None);
        assert_eq!(Remembered::parse("*a*,*b* 10de 2702"), None);
        assert_eq!(Remembered::parse("*nvidia* zz 2702"), None);
        let w = Remembered {
            glob: "*nv-vk64.json".into(),
            vendor: 0x10de,
            device: 0x2702,
        };
        assert_eq!(Remembered::parse(&w.format()), Some(w));
    }

    const NV_JSON: &str = r#"{ "file_format_version": "1.0.1",
        "ICD": { "library_path": ".\\nvoglv64.dll", "api_version": "1.4.357" },
        "layers": [ { "name": "VK_LAYER_NV_optimus" }, { "name": "VK_LAYER_NV_present" } ] }"#;
    const AMD_JSON: &str = r#"{ "file_format_version": "1.0.0",
        "ICD": { "library_path": ".\\amdvlk64.dll" },
        "layer": { "name": "VK_LAYER_AMD_switchable_graphics", "type": "GLOBAL" } }"#;

    /// The test PC: an RTX 4080 SUPER and an AMD iGPU, each driver registering its manifest as
    /// its own implicit layer file.
    fn entries() -> Vec<AdapterEntry> {
        vec![
            AdapterEntry {
                vendor: 0x10de,
                device: 0x2702,
                drivers: vec![PathBuf::from("C:\\ds\\nv_dispi.inf\\nv-vk64.json")],
                implicit_layers: vec![PathBuf::from("C:\\ds\\nv_dispi.inf\\nv-vk64.json")],
            },
            AdapterEntry {
                vendor: 0x1002,
                device: 0x164e,
                drivers: vec![PathBuf::from("C:\\ds\\u0401611.inf\\amd-vulkan64.json")],
                implicit_layers: vec![PathBuf::from("C:\\ds\\u0401611.inf\\amd-vulkan64.json")],
            },
        ]
    }

    fn read(p: &Path) -> Option<String> {
        let name = p.file_name()?.to_str()?;
        Some(match name {
            "nv-vk64.json" => NV_JSON.to_owned(),
            "amd-vulkan64.json" => AMD_JSON.to_owned(),
            _ => return None,
        })
    }

    #[test]
    fn windows_restriction_selects_the_adapter_and_turns_off_the_other_layers() {
        let nv = Remembered {
            glob: "*old-name.json".into(),
            vendor: 0x10de,
            device: 0x2702,
        };
        assert_eq!(
            restriction(&nv, &entries(), &read),
            Restriction {
                glob: "*nv-vk64.json".into(),
                layers_off: vec!["VK_LAYER_AMD_switchable_graphics".into()],
            }
        );
        let amd = Remembered {
            glob: "*amd-vulkan64.json".into(),
            vendor: 0x1002,
            device: 0x164e,
        };
        assert_eq!(
            restriction(&amd, &entries(), &read).layers_off,
            vec![
                "VK_LAYER_NV_optimus".to_owned(),
                "VK_LAYER_NV_present".to_owned()
            ]
        );
    }

    #[test]
    fn unknown_adapter_or_linux_keeps_the_remembered_glob_and_every_layer() {
        let gone = Remembered {
            glob: "*nv-vk64.json".into(),
            vendor: 0x10de,
            device: 0x1111,
        };
        assert_eq!(
            restriction(&gone, &entries(), &read),
            Restriction {
                glob: "*nv-vk64.json".into(),
                layers_off: Vec::new(),
            }
        );
        let linux = Remembered {
            glob: "*nvidia*".into(),
            vendor: 0x10de,
            device: 0x2702,
        };
        assert_eq!(restriction(&linux, &[], &read).glob, "*nvidia*");
        assert!(restriction(&linux, &[], &read).layers_off.is_empty());
    }

    #[test]
    fn layer_manifest_names() {
        assert_eq!(
            layer_names(AMD_JSON),
            vec!["VK_LAYER_AMD_switchable_graphics".to_owned()]
        );
        assert_eq!(layer_names(NV_JSON).len(), 2);
        assert!(layer_names("not json").is_empty());
        assert!(layer_names(r#"{"layer": {"name": "A,B"}}"#).is_empty());
    }
}
