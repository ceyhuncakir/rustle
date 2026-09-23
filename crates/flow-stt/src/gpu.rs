//! Whether recognition can run on a graphics card here, found out before any
//! model loads, and said in words a user can act on when it cannot.
//!
//! A build has at most one GPU backend, because ort ships each as its own
//! prebuilt ONNX Runtime:
//!
//! - `webgpu`: any graphics card through Dawn, on Vulkan (Linux), Direct3D 12
//!   (Windows) or Metal (macOS). It needs nothing beyond the graphics driver.
//!   On an RTX 4090 it ran the 60 s fixture at 140x realtime against CUDA's
//!   170x, with the same text. Cards are listed through Vulkan, which Dawn
//!   uses on Linux and every current Windows driver installs. Integrated
//!   GPUs are left alone under `auto`: an Intel UHD 770 ran the fixture at
//!   6x where the CPU's int8 path managed 36x. Apple Silicon is the
//!   exception, and on macOS the GPU is Metal's.
//! - `cuda`: NVIDIA cards through ONNX Runtime's CUDA provider. [`detect`]
//!   checks what that needs in the order a user would fix it: NVIDIA's
//!   driver (asked through NVML), CUDA 12 support in it, a card the
//!   provider's kernels cover, enough memory, the provider's libraries where
//!   ONNX Runtime looks, and CUDA 12 and cuDNN 9. Those last are loaded from
//!   the usual install locations when the loader would not find them, which
//!   is what `flowd/stt.py` did for the pip wheels in its venv.
//! - neither: the CPU.
//!
//! On Linux the PCI bus is read as well, so a card without a working driver
//! is still named.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use flow_core::models::Precision;
use libloading::Library;
use log::info;
use serde::Serialize;

/// The GPU backend this build has: `cuda`, `webgpu` or none.
pub fn backend() -> Option<&'static str> {
    if cfg!(feature = "cuda") {
        Some("cuda")
    } else if cfg!(feature = "webgpu") {
        Some("webgpu")
    } else {
        None
    }
}

/// Whether a provider setting insists on the GPU. `cuda` is the name the
/// setting had before there was more than one backend.
pub fn insists_on_gpu(provider: &str) -> bool {
    matches!(provider, "gpu" | "cuda")
}

/// Peak use measured on an RTX 4090 was 3.4 GiB (fp32 encoder, CUDA
/// context and cuDNN workspace, 60 s of audio), so a 4 GB card is the
/// smallest that fits.
const MIN_VRAM_MB: u64 = 3500;

/// One graphics card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GpuDevice {
    pub name: String,
    /// `nvidia`, `amd`, `intel`, `apple` or `other`.
    pub vendor: String,
    /// `discrete` or `integrated`, when a driver says.
    pub kind: Option<String>,
    /// Video memory in MiB, when a driver says.
    pub memory_mb: Option<u64>,
    /// CUDA compute capability such as `8.9`, NVIDIA cards only.
    pub compute: Option<String>,
    /// The kernel driver bound to the card on Linux, e.g. `amdgpu` or `nouveau`.
    pub driver: Option<String>,
}

/// What [`detect`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GpuReport {
    /// The build's GPU backend: `cuda`, `webgpu`, or none for a CPU build.
    pub backend: Option<String>,
    /// Every graphics card found, the one recognition would use first.
    pub devices: Vec<GpuDevice>,
    /// The card recognition would run on, e.g. `NVIDIA GeForce RTX 4090 (24 GB)`.
    pub gpu: Option<String>,
    /// NVIDIA's driver version and the newest CUDA it supports.
    pub driver: Option<String>,
    /// Whether recognition runs on `gpu` when the provider is `auto`.
    pub usable: bool,
    /// Why not, when it does not, e.g. `cuDNN 9 is not installed`.
    pub problem: Option<String>,
    /// What would change that, when something can.
    pub fix: Option<String>,
    /// CUDA libraries loaded from outside the loader's search path.
    pub loaded_from: Vec<String>,
}

impl GpuReport {
    /// One line for logs, `flow doctor` and the compute report.
    pub fn describe(&self) -> String {
        match (&self.gpu, &self.problem) {
            (Some(gpu), None) => gpu.clone(),
            (Some(gpu), Some(problem)) => format!("{gpu} not used: {problem}"),
            (None, Some(problem)) => problem.clone(),
            (None, None) => "no graphics card found".into(),
        }
    }
}

/// The report, worked out on first use and then kept: the libraries it
/// loads have to stay loaded for the provider, and the hardware does not
/// change under a running app.
pub fn detect() -> &'static GpuReport {
    static REPORT: OnceLock<GpuReport> = OnceLock::new();
    REPORT.get_or_init(|| {
        let report = probe();
        info!("gpu: {}", report.describe());
        report
    })
}

/// Which precision the configured provider will load, and so which files to
/// fetch: fp32 when the encoder will run on the GPU, int8 on the CPU.
pub fn precision_for(provider: &str) -> Precision {
    let on_gpu = match provider {
        "cpu" => false,
        p if insists_on_gpu(p) => backend().is_some(),
        _ => detect().usable,
    };
    if on_gpu {
        Precision::Fp32
    } else {
        Precision::Int8
    }
}

/// Hand CUDA's context back once ONNX Runtime has been released; see
/// [`crate::release_runtime`]. A no-op for other backends.
pub(crate) fn reset_cuda() -> bool {
    backend() == Some("cuda") && cuda_libs::reset_device()
}

/// Why the GPU cannot be used, and what would change that.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Blocker {
    problem: String,
    fix: Option<String>,
}

fn blocked(problem: impl Into<String>, fix: Option<String>) -> Blocker {
    Blocker { problem: problem.into(), fix }
}

fn probe() -> GpuReport {
    let nvidia = if cfg!(target_os = "macos") { Err(None) } else { nvml::query() };
    let pci = pci::display_devices();
    let adapters = match backend() {
        Some("webgpu") if cfg!(target_os = "macos") => Ok(apple::gpu().into_iter().collect()),
        Some("webgpu") => vulkan::adapters(),
        _ => Ok(Vec::new()),
    };
    let devices = merge_devices(&nvidia, adapters.as_ref().map(Vec::as_slice).unwrap_or(&[]), pci);

    let mut report = GpuReport {
        backend: backend().map(Into::into),
        devices,
        gpu: None,
        driver: nvidia.as_ref().ok().map(nvml::Nvidia::label),
        usable: false,
        problem: None,
        fix: None,
        loaded_from: Vec::new(),
    };
    let (gpu, blocker) = match backend() {
        Some("cuda") => {
            let gpu = nvidia.as_ref().ok().and_then(nvml::Nvidia::best).map(nvml::Card::label);
            let blocker = cuda_blocker(&nvidia, &report.devices).or_else(|| {
                provider_blocker().or_else(|| {
                    let (missing, loaded) = cuda_libs::preload();
                    report.loaded_from = loaded;
                    cuda_libs::blocker(&missing)
                })
            });
            (gpu, blocker)
        }
        Some(_) => webgpu_choice(&adapters, &report.devices),
        None => (
            report.devices.first().map(|d| d.name.clone()),
            Some(blocked(
                "this build of Flow has no GPU support",
                Some("use a build made with `--features webgpu`, which scripts/install-app.sh makes".into()),
            )),
        ),
    };
    report.gpu = gpu.or_else(|| report.devices.first().map(|d| d.name.clone()));
    match blocker {
        None => report.usable = true,
        Some(b) => {
            report.problem = Some(b.problem);
            report.fix = b.fix;
        }
    }
    report
}

/// One list of cards from what each source knows: NVML has NVIDIA's own
/// view (memory, compute capability), Vulkan says discrete or integrated,
/// and the PCI bus has every card, driver or not. The card recognition
/// would pick comes first.
fn merge_devices(
    nvidia: &Result<nvml::Nvidia, Option<Blocker>>,
    adapters: &[Adapter],
    pci: Vec<(GpuDevice, (u32, u32))>,
) -> Vec<GpuDevice> {
    let nvml_cards = nvidia.as_ref().map(|nv| nv.cards.as_slice()).unwrap_or(&[]);
    let mut devices: Vec<GpuDevice> = nvml_cards
        .iter()
        .map(|card| {
            let mut d = card.device();
            d.kind = adapters.iter().find(|a| a.name == card.name).map(|a| a.kind.name().into());
            d
        })
        .collect();
    for a in adapters.iter().filter(|a| a.kind.can_compute()) {
        if a.vendor == "nvidia" && !nvml_cards.is_empty() {
            continue;
        }
        let driver = pci
            .iter()
            .find(|(_, ids)| *ids == (a.vendor_id, a.device_id))
            .and_then(|(d, _)| d.driver.clone());
        devices.push(a.device(driver));
    }
    for (card, ids) in pci {
        let in_vulkan = adapters.iter().any(|a| (a.vendor_id, a.device_id) == ids);
        let in_nvml =
            !nvml_cards.is_empty() && card.vendor == "nvidia" && card.driver.as_deref() == Some("nvidia");
        if !in_vulkan && !in_nvml {
            devices.push(card);
        }
    }
    let rank = |d: &GpuDevice| match d.kind.as_deref() {
        Some("discrete") => 0,
        _ if d.vendor == "nvidia" => 1,
        Some("integrated") => 2,
        _ => 3,
    };
    devices.sort_by_key(|d| (rank(d), std::cmp::Reverse(d.memory_mb.unwrap_or(0))));
    devices
}

/// The card WebGPU would pick (Dawn asks for the high-performance one), and
/// whether `auto` should use it.
fn webgpu_choice(
    adapters: &Result<Vec<Adapter>, String>,
    devices: &[GpuDevice],
) -> (Option<String>, Option<Blocker>) {
    if cfg!(target_os = "macos") {
        return match adapters.as_ref().ok().and_then(|a| a.first()) {
            Some(apple) => (Some(apple.label()), None),
            None => (None, Some(blocked("GPU recognition on a Mac needs Apple Silicon", None))),
        };
    }
    let no_driver_fix = || {
        Some(if cfg!(windows) {
            "update the graphics driver from the card maker's site".to_string()
        } else {
            "install the Vulkan driver for your card (mesa-vulkan-drivers on Fedora, mesa-vulkan-drivers or nvidia-driver on Debian and Ubuntu)".to_string()
        })
    };
    let adapters = match adapters {
        Ok(list) => list.iter().filter(|a| a.kind.can_compute()).collect::<Vec<_>>(),
        Err(why) => {
            let named = devices.first().map(|d| d.name.clone());
            return (named, Some(blocked(why.clone(), no_driver_fix())));
        }
    };
    let Some(best) = adapters.iter().max_by_key(|a| (a.kind, a.memory_mb)) else {
        return match devices.first() {
            Some(card) => {
                (Some(card.name.clone()), Some(blocked("it has no Vulkan driver", no_driver_fix())))
            }
            None => (None, Some(blocked("no graphics card found", None))),
        };
    };
    if best.kind == Kind::Integrated {
        return (
            Some(best.label()),
            Some(blocked("it is built into the processor, and this model runs faster on the CPU", None)),
        );
    }
    if best.memory_mb < MIN_VRAM_MB {
        return (
            Some(best.label()),
            Some(blocked(
                format!(
                    "it has {} of video memory and recognition needs about 3.5 GB",
                    gigabytes(best.memory_mb)
                ),
                None,
            )),
        );
    }
    (Some(best.label()), None)
}

/// The CUDA build's checks that depend only on the machine.
fn cuda_blocker(nvidia: &Result<nvml::Nvidia, Option<Blocker>>, devices: &[GpuDevice]) -> Option<Blocker> {
    if cfg!(target_os = "macos") {
        return Some(blocked("CUDA does not exist on macOS", Some(webgpu_fix())));
    }
    let nv = match nvidia {
        Ok(nv) if !nv.cards.is_empty() => nv,
        Ok(_) => return Some(blocked("NVIDIA's driver reports no graphics card", None)),
        // The driver is there but not working; NVML said how.
        Err(Some(broken)) => return Some(broken.clone()),
        Err(None) => {
            return Some(match devices.iter().find(|d| d.vendor == "nvidia") {
                Some(card) => blocked(
                    match card.driver.as_deref() {
                        Some("nvidia") => "NVIDIA's management library (libnvidia-ml) is missing".to_string(),
                        Some(other) => format!("NVIDIA's driver is not installed (the card uses {other})"),
                        None => "NVIDIA's driver is not installed".to_string(),
                    },
                    Some(driver_fix()),
                ),
                None => match devices.first() {
                    Some(other) => blocked(
                        format!("this CUDA build needs an NVIDIA card and this machine has {}", other.name),
                        Some(webgpu_fix()),
                    ),
                    None => blocked("no NVIDIA graphics card", None),
                },
            });
        }
    };
    let Some(card) = nv.best() else {
        return Some(blocked("NVIDIA's driver reports no graphics card", None));
    };
    if nv.cuda < 12_000 {
        return Some(blocked(
            format!(
                "the NVIDIA driver ({}) supports CUDA {} but Flow needs CUDA 12",
                nv.driver,
                cuda_version(nv.cuda)
            ),
            Some("update the NVIDIA driver to 525 or newer".into()),
        ));
    }
    let (major, minor) = card.compute;
    if !kernels_cover(major, minor) {
        let age = if (major, minor) < (7, 5) { "older" } else { "newer" };
        return Some(blocked(
            format!("its compute capability {major}.{minor} is {age} than the CUDA build's GPU code covers (7.5, 8.x, 9.0)"),
            Some(webgpu_fix()),
        ));
    }
    if card.memory_mb < MIN_VRAM_MB {
        return Some(blocked(
            format!(
                "it has {} of video memory and recognition needs about 3.5 GB",
                gigabytes(card.memory_mb)
            ),
            None,
        ));
    }
    None
}

fn webgpu_fix() -> String {
    "use the WebGPU build (the default of scripts/install-app.sh and the installers), which runs on any graphics card".into()
}

fn driver_fix() -> String {
    if cfg!(windows) {
        "install NVIDIA's driver from nvidia.com".into()
    } else {
        "install NVIDIA's driver (on Fedora: akmod-nvidia from RPM Fusion) and restart".into()
    }
}

/// Compute capabilities the kernels in ort's CUDA 12 provider (rc.12) were
/// built for: sm_75, sm_80 and sm_90a, with no PTX to compile for anything
/// else (`cuobjdump --list-elf libonnxruntime_providers_cuda.so`). sm_80
/// code also runs on 8.6 and 8.9. Check again when bumping `ort`.
fn kernels_cover(major: i32, minor: i32) -> bool {
    matches!((major, minor), (7, 5) | (8, _) | (9, 0))
}

fn cuda_version(v: i32) -> String {
    format!("{}.{}", v / 1000, (v % 1000) / 10)
}

fn gigabytes(mb: u64) -> String {
    let gb = mb as f64 / 1024.0;
    if gb < 10.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{gb:.0} GB")
    }
}

// -- graphics adapters as WebGPU sees them ------------------------------------------

/// Vulkan's device types, in the order `auto` prefers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// A software rasteriser such as lavapipe: the CPU with extra steps.
    Cpu,
    Other,
    Virtual,
    Integrated,
    Discrete,
}

impl Kind {
    fn can_compute(self) -> bool {
        !matches!(self, Kind::Cpu | Kind::Other)
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Discrete => "discrete",
            Kind::Integrated => "integrated",
            Kind::Virtual => "virtual",
            Kind::Cpu => "software",
            Kind::Other => "other",
        }
    }
}

/// One adapter WebGPU could run on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Adapter {
    name: String,
    vendor: &'static str,
    vendor_id: u32,
    device_id: u32,
    kind: Kind,
    /// The largest device-local memory heap, in MiB.
    memory_mb: u64,
}

impl Adapter {
    fn label(&self) -> String {
        if self.kind == Kind::Discrete {
            format!("{} ({})", self.name, gigabytes(self.memory_mb))
        } else {
            self.name.clone()
        }
    }

    fn device(&self, driver: Option<String>) -> GpuDevice {
        GpuDevice {
            name: self.name.clone(),
            vendor: self.vendor.into(),
            kind: Some(self.kind.name().into()),
            memory_mb: (self.kind == Kind::Discrete).then_some(self.memory_mb),
            compute: None,
            driver,
        }
    }
}

// -- loading libraries -------------------------------------------------------------

/// A library that comes with the OS or the graphics driver, by name. On
/// Windows only System32 is searched: a DLL of the same name in the current
/// folder or on `PATH` must not stand in for it.
fn open_system(name: &str) -> Result<Library, libloading::Error> {
    // SAFETY: the loaders and driver libraries opened this way (NVML, Vulkan)
    // only set up their own state when loaded.
    #[cfg(windows)]
    {
        use libloading::os::windows::{Library as Windows, LOAD_LIBRARY_SEARCH_SYSTEM32};
        unsafe { Windows::load_with_flags(name, LOAD_LIBRARY_SEARCH_SYSTEM32) }.map(Library::from)
    }
    #[cfg(not(windows))]
    unsafe {
        Library::new(name)
    }
}

/// A library by full path. On Windows its own folder is searched for the
/// DLLs it depends on, which plain `LoadLibrary` does not do, and then only
/// the program's folder and System32.
fn open_path(path: &Path) -> Result<Library, libloading::Error> {
    // SAFETY: as for `open_system`; also CUDA's libraries, whose
    // initialisers likewise only set up their own state.
    #[cfg(windows)]
    {
        use libloading::os::windows::{
            Library as Windows, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        };
        let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS;
        unsafe { Windows::load_with_flags(path, flags) }.map(Library::from)
    }
    #[cfg(not(windows))]
    unsafe {
        Library::new(path)
    }
}

// -- ONNX Runtime's provider libraries -------------------------------------------

#[cfg(windows)]
const PROVIDER_LIBS: [&str; 2] = ["onnxruntime_providers_shared.dll", "onnxruntime_providers_cuda.dll"];
#[cfg(not(windows))]
const PROVIDER_LIBS: [&str; 2] = ["libonnxruntime_providers_shared.so", "libonnxruntime_providers_cuda.so"];

/// ONNX Runtime loads its CUDA provider from beside the program; a build
/// with the `cuda` feature puts the files there (ort's `copy-dylibs`), and
/// an install has to carry them along.
fn provider_blocker() -> Option<Blocker> {
    let dir = provider_dir()?;
    if PROVIDER_LIBS.iter().all(|f| dir.join(f).is_file()) {
        return None;
    }
    let real = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let fix = match real {
        Some(real) if real != dir && PROVIDER_LIBS.iter().all(|f| real.join(f).is_file()) => format!(
            "start Flow as {} rather than through a link; scripts/install-app.sh sets this up",
            real.join(program_name()).display()
        ),
        _ => "reinstall Flow with GPU support (scripts/install-app.sh)".into(),
    };
    Some(blocked(
        format!("ONNX Runtime's CUDA libraries are not next to the program in {}", dir.display()),
        Some(fix),
    ))
}

fn program_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "flow".into())
}

/// Where ONNX Runtime will look. On Windows that is the program's own
/// folder. On Linux it is the folder in `argv[0]` with links not followed:
/// started through a symlink, it looks beside the link (checked with
/// `exec -a`), and a bare name was found on `PATH`.
fn provider_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::current_exe().ok()?.parent().map(Path::to_path_buf);
    }
    let argv0 = PathBuf::from(std::env::args_os().next()?);
    let cwd = std::env::current_dir().ok()?;
    dir_for_argv0(&argv0, &cwd, std::env::var_os("PATH").as_deref())
}

fn dir_for_argv0(argv0: &Path, cwd: &Path, path: Option<&OsStr>) -> Option<PathBuf> {
    if argv0.components().count() > 1 {
        return cwd.join(argv0).parent().map(Path::to_path_buf);
    }
    std::env::split_paths(path?).find(|dir| dir.join(argv0).is_file())
}

// -- NVML --------------------------------------------------------------------

mod nvml {
    use std::ffi::{c_char, c_int, c_uint, c_void, CStr};

    use libloading::Library;

    use super::{blocked, gigabytes, Blocker, GpuDevice};

    type Device = *mut c_void;
    type Ret = c_int;
    const SUCCESS: Ret = 0;
    const ERROR_DRIVER_NOT_LOADED: Ret = 9;
    const ERROR_LIB_RM_VERSION_MISMATCH: Ret = 18;

    /// `nvmlMemory_t`; only `total` is read.
    #[repr(C)]
    #[derive(Default)]
    #[allow(dead_code)]
    struct Memory {
        total: u64,
        free: u64,
        used: u64,
    }

    pub struct Card {
        pub name: String,
        pub memory_mb: u64,
        pub compute: (i32, i32),
    }

    impl Card {
        pub fn label(&self) -> String {
            format!("{} ({})", self.name, gigabytes(self.memory_mb))
        }

        pub fn device(&self) -> GpuDevice {
            GpuDevice {
                name: self.name.clone(),
                vendor: "nvidia".into(),
                kind: None,
                memory_mb: Some(self.memory_mb),
                compute: Some(format!("{}.{}", self.compute.0, self.compute.1)),
                driver: Some("nvidia".into()),
            }
        }
    }

    pub struct Nvidia {
        pub driver: String,
        /// The newest CUDA the driver supports, as 1000 * major + 10 * minor.
        pub cuda: i32,
        pub cards: Vec<Card>,
    }

    impl Nvidia {
        /// The card CUDA puts first: it orders fastest first, which the
        /// newest architecture and then the most memory approximates.
        pub fn best(&self) -> Option<&Card> {
            self.cards.iter().max_by_key(|c| (c.compute, c.memory_mb))
        }

        pub fn label(&self) -> String {
            format!("{}, CUDA {}", self.driver, super::cuda_version(self.cuda))
        }
    }

    fn library() -> Option<Library> {
        if cfg!(windows) {
            // Drivers before R418 kept it in NVSMI rather than in System32.
            let nvsmi = std::env::var_os("ProgramW6432")
                .map(|pf| std::path::Path::new(&pf).join(r"NVIDIA Corporation\NVSMI\nvml.dll"));
            super::open_system("nvml.dll").ok().or_else(|| super::open_path(&nvsmi?).ok())
        } else {
            ["libnvidia-ml.so.1", "libnvidia-ml.so"].into_iter().find_map(|n| super::open_system(n).ok())
        }
    }

    /// `Err(None)`: no NVIDIA driver at all. `Err(Some(_))`: installed but
    /// not working, and why.
    pub fn query() -> Result<Nvidia, Option<Blocker>> {
        let lib = library().ok_or(None)?;
        // SAFETY: the signatures are NVML's documented C API.
        unsafe { query_with(&lib) }
    }

    unsafe fn query_with(lib: &Library) -> Result<Nvidia, Option<Blocker>> {
        macro_rules! sym {
            ($name:literal, $ty:ty) => {
                *lib.get::<$ty>($name).map_err(|e| {
                    Some(blocked(format!("NVIDIA's management library is incomplete: {e}"), None))
                })?
            };
        }
        let init = sym!(b"nvmlInit_v2\0", unsafe extern "C" fn() -> Ret);
        let shutdown = sym!(b"nvmlShutdown\0", unsafe extern "C" fn() -> Ret);
        let driver_version =
            sym!(b"nvmlSystemGetDriverVersion\0", unsafe extern "C" fn(*mut c_char, c_uint) -> Ret);
        let cuda_version = match lib
            .get::<unsafe extern "C" fn(*mut c_int) -> Ret>(b"nvmlSystemGetCudaDriverVersion_v2\0")
        {
            Ok(f) => *f,
            Err(_) => sym!(b"nvmlSystemGetCudaDriverVersion\0", unsafe extern "C" fn(*mut c_int) -> Ret),
        };
        let count = sym!(b"nvmlDeviceGetCount_v2\0", unsafe extern "C" fn(*mut c_uint) -> Ret);
        let handle =
            sym!(b"nvmlDeviceGetHandleByIndex_v2\0", unsafe extern "C" fn(c_uint, *mut Device) -> Ret);
        let name = sym!(b"nvmlDeviceGetName\0", unsafe extern "C" fn(Device, *mut c_char, c_uint) -> Ret);
        let memory = sym!(b"nvmlDeviceGetMemoryInfo\0", unsafe extern "C" fn(Device, *mut Memory) -> Ret);
        let compute = sym!(
            b"nvmlDeviceGetCudaComputeCapability\0",
            unsafe extern "C" fn(Device, *mut c_int, *mut c_int) -> Ret
        );

        match init() {
            SUCCESS => {}
            ERROR_DRIVER_NOT_LOADED => {
                return Err(Some(blocked(
                    "NVIDIA's driver is installed but not loaded",
                    Some("restart the computer; a new or updated driver loads at boot".into()),
                )))
            }
            ERROR_LIB_RM_VERSION_MISMATCH => {
                return Err(Some(blocked(
                    "the NVIDIA driver was updated and the old one is still running",
                    Some("restart the computer".into()),
                )))
            }
            code => {
                return Err(Some(blocked(format!("NVIDIA's driver did not start (NVML error {code})"), None)))
            }
        }

        let text = |buf: &[c_char]| CStr::from_ptr(buf.as_ptr()).to_string_lossy().trim().to_string();
        let mut buf = [0 as c_char; 96];
        let driver = if driver_version(buf.as_mut_ptr(), buf.len() as c_uint) == SUCCESS {
            text(&buf)
        } else {
            "unknown".into()
        };
        let mut cuda = 0;
        if cuda_version(&mut cuda) != SUCCESS {
            cuda = 0;
        }
        let mut cards = Vec::new();
        let mut n: c_uint = 0;
        if count(&mut n) == SUCCESS {
            for i in 0..n {
                let mut dev: Device = std::ptr::null_mut();
                if handle(i, &mut dev) != SUCCESS {
                    continue;
                }
                let mut buf = [0 as c_char; 96];
                let card_name = if name(dev, buf.as_mut_ptr(), buf.len() as c_uint) == SUCCESS {
                    text(&buf)
                } else {
                    "NVIDIA graphics card".into()
                };
                let mut mem = Memory::default();
                let memory_mb = if memory(dev, &mut mem) == SUCCESS { mem.total / (1024 * 1024) } else { 0 };
                let (mut major, mut minor) = (0, 0);
                if compute(dev, &mut major, &mut minor) != SUCCESS {
                    (major, minor) = (0, 0);
                }
                cards.push(Card { name: card_name, memory_mb, compute: (major, minor) });
            }
        }
        shutdown();
        Ok(Nvidia { driver, cuda, cards })
    }
}

// -- the PCI bus (Linux) ------------------------------------------------------

mod pci {
    use super::GpuDevice;

    /// Display controllers on the PCI bus, named from the system's pci.ids,
    /// with their vendor and device ids.
    #[cfg(target_os = "linux")]
    pub fn display_devices() -> Vec<(GpuDevice, (u32, u32))> {
        use std::fs;

        let Ok(entries) = fs::read_dir("/sys/bus/pci/devices") else { return Vec::new() };
        let ids = ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids", "/usr/share/pci.ids"]
            .iter()
            .find_map(|p| fs::read_to_string(p).ok())
            .unwrap_or_default();
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let dir = entry.path();
            let hex = |f: &str| {
                let s = fs::read_to_string(dir.join(f)).ok()?;
                u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
            };
            let (Some(class), Some(vendor), Some(device)) = (hex("class"), hex("vendor"), hex("device"))
            else {
                continue;
            };
            // Base class 0x03: VGA, XGA and 3D controllers.
            if class >> 16 != 0x03 {
                continue;
            }
            let driver = fs::read_link(dir.join("driver"))
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            let card = GpuDevice {
                name: pretty_name(vendor, lookup(&ids, vendor, device)),
                vendor: vendor_key(vendor).into(),
                kind: None,
                memory_mb: None,
                compute: None,
                driver,
            };
            out.push((card, (vendor, device)));
        }
        out
    }

    #[cfg(not(target_os = "linux"))]
    pub fn display_devices() -> Vec<(GpuDevice, (u32, u32))> {
        Vec::new()
    }

    pub fn vendor_key(vendor: u32) -> &'static str {
        match vendor {
            0x10de => "nvidia",
            0x1002 | 0x1022 => "amd",
            0x8086 => "intel",
            0x106b => "apple",
            _ => "other",
        }
    }

    /// The device's entry in pci.ids: a vendor line (`10de  NVIDIA
    /// Corporation`) followed by its devices, one tab in (`\t2684  AD102
    /// [GeForce RTX 4090]`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn lookup(ids: &str, vendor: u32, device: u32) -> Option<&str> {
        let vendor_prefix = format!("{vendor:04x}  ");
        let device_prefix = format!("\t{device:04x}  ");
        ids.lines()
            .skip_while(|l| !l.starts_with(&vendor_prefix))
            .skip(1)
            .take_while(|l| l.starts_with('\t') || l.starts_with('#') || l.is_empty())
            .find_map(|l| l.strip_prefix(&device_prefix))
            .map(str::trim)
    }

    /// `NVIDIA GeForce RTX 4090` from `AD102 [GeForce RTX 4090]`: the
    /// bracketed marketing name when there is one, with the vendor in front.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn pretty_name(vendor: u32, device: Option<&str>) -> String {
        let brand = match vendor_key(vendor) {
            "nvidia" => "NVIDIA",
            "amd" => "AMD",
            "intel" => "Intel",
            _ => "",
        };
        let model = device.map(|d| match (d.find('['), d.rfind(']')) {
            (Some(open), Some(close)) if close > open => &d[open + 1..close],
            _ => d,
        });
        match (brand, model) {
            ("", Some(m)) => m.to_string(),
            ("", None) => "Graphics card".into(),
            (b, Some(m)) => format!("{b} {m}"),
            (b, None) => format!("{b} graphics card"),
        }
    }
}

// -- Vulkan (Linux, Windows) --------------------------------------------------

/// Graphics adapters as the Vulkan loader lists them. Only the loader's
/// exported Vulkan 1.0 entry points are used, so any driver will do.
mod vulkan {
    use std::ffi::{c_char, c_void, CStr};

    use libloading::Library;

    use super::{pci, Adapter, Kind};

    type Handle = *mut c_void;
    type VkResult = i32;
    const SUCCESS: VkResult = 0;
    const INCOMPLETE: VkResult = 5;
    const ERROR_INCOMPATIBLE_DRIVER: VkResult = -9;
    const STRUCTURE_TYPE_APPLICATION_INFO: u32 = 0;
    const STRUCTURE_TYPE_INSTANCE_CREATE_INFO: u32 = 1;
    const MEMORY_HEAP_DEVICE_LOCAL: u32 = 1;
    const API_VERSION_1_0: u32 = 1 << 22;

    #[repr(C)]
    struct ApplicationInfo {
        s_type: u32,
        next: *const c_void,
        application_name: *const c_char,
        application_version: u32,
        engine_name: *const c_char,
        engine_version: u32,
        api_version: u32,
    }

    #[repr(C)]
    struct InstanceCreateInfo {
        s_type: u32,
        next: *const c_void,
        flags: u32,
        application_info: *const ApplicationInfo,
        layer_count: u32,
        layers: *const *const c_char,
        extension_count: u32,
        extensions: *const *const c_char,
    }

    /// The head of `VkPhysicalDeviceProperties` (824 bytes on 64-bit), with
    /// room behind it for the limits and sparse properties nobody reads.
    #[repr(C)]
    struct Properties {
        api_version: u32,
        driver_version: u32,
        vendor_id: u32,
        device_id: u32,
        device_type: u32,
        name: [c_char; 256],
        pipeline_cache_uuid: [u8; 16],
        rest: [u64; 128],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MemoryType {
        flags: u32,
        heap: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MemoryHeap {
        size: u64,
        flags: u32,
    }

    /// `VkPhysicalDeviceMemoryProperties`.
    #[repr(C)]
    struct MemoryProperties {
        type_count: u32,
        types: [MemoryType; 32],
        heap_count: u32,
        heaps: [MemoryHeap; 16],
    }

    type CreateInstance =
        unsafe extern "system" fn(*const InstanceCreateInfo, *const c_void, *mut Handle) -> VkResult;
    type DestroyInstance = unsafe extern "system" fn(Handle, *const c_void);
    type EnumerateDevices = unsafe extern "system" fn(Handle, *mut u32, *mut Handle) -> VkResult;
    type GetProperties = unsafe extern "system" fn(Handle, *mut Properties);
    type GetMemoryProperties = unsafe extern "system" fn(Handle, *mut MemoryProperties);

    const LOADER: &str = if cfg!(windows) { "vulkan-1.dll" } else { "libvulkan.so.1" };

    /// Every adapter, software ones included; `Err` says why there are none.
    pub fn adapters() -> Result<Vec<Adapter>, String> {
        let lib = super::open_system(LOADER)
            .map_err(|_| format!("the Vulkan loader ({LOADER}) is not installed"))?;
        // SAFETY: the signatures are Vulkan 1.0's.
        let result = unsafe { enumerate(&lib) };
        // Drivers are loaded into the process now, and some do not survive
        // being unloaded; WebGPU opens the loader again anyway.
        std::mem::forget(lib);
        result
    }

    unsafe fn enumerate(lib: &Library) -> Result<Vec<Adapter>, String> {
        let missing = |e: libloading::Error| format!("the Vulkan loader is incomplete: {e}");
        let create = *lib.get::<CreateInstance>(b"vkCreateInstance\0").map_err(missing)?;
        let destroy = *lib.get::<DestroyInstance>(b"vkDestroyInstance\0").map_err(missing)?;
        let enumerate = *lib.get::<EnumerateDevices>(b"vkEnumeratePhysicalDevices\0").map_err(missing)?;
        let properties = *lib.get::<GetProperties>(b"vkGetPhysicalDeviceProperties\0").map_err(missing)?;
        let memory =
            *lib.get::<GetMemoryProperties>(b"vkGetPhysicalDeviceMemoryProperties\0").map_err(missing)?;

        let app = ApplicationInfo {
            s_type: STRUCTURE_TYPE_APPLICATION_INFO,
            next: std::ptr::null(),
            application_name: c"Flow".as_ptr(),
            application_version: 0,
            engine_name: std::ptr::null(),
            engine_version: 0,
            api_version: API_VERSION_1_0,
        };
        let info = InstanceCreateInfo {
            s_type: STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
            next: std::ptr::null(),
            flags: 0,
            application_info: &app,
            layer_count: 0,
            layers: std::ptr::null(),
            extension_count: 0,
            extensions: std::ptr::null(),
        };
        let mut instance: Handle = std::ptr::null_mut();
        match create(&info, std::ptr::null(), &mut instance) {
            SUCCESS => {}
            ERROR_INCOMPATIBLE_DRIVER => return Err("no Vulkan driver is installed".into()),
            code => return Err(format!("Vulkan did not start (error {code})")),
        }

        let mut count = 0u32;
        let mut handles: Vec<Handle> = Vec::new();
        let mut code = enumerate(instance, &mut count, std::ptr::null_mut());
        if code == SUCCESS && count > 0 {
            handles.resize(count as usize, std::ptr::null_mut());
            code = enumerate(instance, &mut count, handles.as_mut_ptr());
            handles.truncate(count as usize);
        }
        let mut out = Vec::new();
        if code == SUCCESS || code == INCOMPLETE {
            for &device in &handles {
                let mut props: Properties = std::mem::zeroed();
                properties(device, &mut props);
                let mut mem: MemoryProperties = std::mem::zeroed();
                memory(device, &mut mem);
                let heaps = &mem.heaps[..(mem.heap_count as usize).min(16)];
                let memory_mb = heaps
                    .iter()
                    .filter(|h| h.flags & MEMORY_HEAP_DEVICE_LOCAL != 0)
                    .map(|h| h.size / (1024 * 1024))
                    .max()
                    .unwrap_or(0);
                out.push(Adapter {
                    name: tidy_name(&CStr::from_ptr(props.name.as_ptr()).to_string_lossy()),
                    vendor: pci::vendor_key(props.vendor_id),
                    vendor_id: props.vendor_id,
                    device_id: props.device_id,
                    kind: match props.device_type {
                        1 => Kind::Integrated,
                        2 => Kind::Discrete,
                        3 => Kind::Virtual,
                        4 => Kind::Cpu,
                        _ => Kind::Other,
                    },
                    memory_mb,
                });
            }
        }
        destroy(instance, std::ptr::null());
        if code != SUCCESS && code != INCOMPLETE {
            return Err(format!("Vulkan could not list the graphics cards (error {code})"));
        }
        Ok(out)
    }

    /// Mesa appends its internal chip name: `AMD Radeon RX 7900 XTX (RADV
    /// NAVI31)`, `Intel(R) UHD Graphics 770 (ADL-S GT1)`.
    pub fn tidy_name(name: &str) -> String {
        let name = name.trim();
        match name.rfind(" (") {
            Some(at) if name.ends_with(')') => name[..at].to_string(),
            _ => name.to_string(),
        }
    }
}

// -- Apple Silicon -------------------------------------------------------------

mod apple {
    use super::Adapter;

    /// The Mac's own GPU on Apple Silicon, which WebGPU reaches through
    /// Metal. Its memory is the Mac's, shared with the CPU.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    pub fn gpu() -> Option<Adapter> {
        let chip = sysctl_string(c"machdep.cpu.brand_string").unwrap_or_else(|| "Apple Silicon".into());
        Some(Adapter {
            name: format!("{chip} GPU"),
            vendor: "apple",
            vendor_id: 0x106b,
            device_id: 0,
            // Integrated by construction, but fast enough to be worth it.
            kind: super::Kind::Discrete,
            memory_mb: sysctl_u64(c"hw.memsize").map(|b| b / (1024 * 1024)).unwrap_or(0),
        })
    }

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    pub fn gpu() -> Option<Adapter> {
        None
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    extern "C" {
        fn sysctlbyname(
            name: *const std::ffi::c_char,
            old: *mut std::ffi::c_void,
            old_len: *mut usize,
            new: *mut std::ffi::c_void,
            new_len: usize,
        ) -> std::ffi::c_int;
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn sysctl_string(name: &std::ffi::CStr) -> Option<String> {
        let mut buf = [0u8; 128];
        let mut len = buf.len();
        // SAFETY: sysctlbyname writes at most `len` bytes into `buf`.
        let rc = unsafe {
            sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0)
        };
        (rc == 0).then(|| String::from_utf8_lossy(&buf[..len]).trim_end_matches('\0').trim().to_string())
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn sysctl_u64(name: &std::ffi::CStr) -> Option<u64> {
        let mut value = 0u64;
        let mut len = std::mem::size_of::<u64>();
        // SAFETY: as above, with an 8-byte buffer for an 8-byte value.
        let rc = unsafe {
            sysctlbyname(name.as_ptr(), (&mut value as *mut u64).cast(), &mut len, std::ptr::null_mut(), 0)
        };
        (rc == 0).then_some(value)
    }
}

// -- CUDA and cuDNN ------------------------------------------------------------

mod cuda_libs {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use super::{blocked, Blocker, Library, Mutex};

    pub struct Needed {
        /// How a message names it.
        what: &'static str,
        /// The pip wheel that ships it, for the Linux fix.
        wheel: &'static str,
        linux: &'static str,
        windows: &'static str,
    }

    /// What ONNX Runtime's CUDA provider links against (`ldd` on
    /// `libonnxruntime_providers_cuda.so`), dependencies first.
    const NEEDED: &[Needed] = &[
        Needed {
            what: "the CUDA 12 runtime",
            wheel: "nvidia-cuda-runtime-cu12",
            linux: "libcudart.so.12",
            windows: "cudart64_12.dll",
        },
        Needed {
            what: "cuBLAS 12",
            wheel: "nvidia-cublas-cu12",
            linux: "libcublasLt.so.12",
            windows: "cublasLt64_12.dll",
        },
        Needed {
            what: "cuBLAS 12",
            wheel: "nvidia-cublas-cu12",
            linux: "libcublas.so.12",
            windows: "cublas64_12.dll",
        },
        Needed {
            what: "cuFFT",
            wheel: "nvidia-cufft-cu12",
            linux: "libcufft.so.11",
            windows: "cufft64_11.dll",
        },
        Needed {
            what: "cuRAND",
            wheel: "nvidia-curand-cu12",
            linux: "libcurand.so.10",
            windows: "curand64_10.dll",
        },
        Needed {
            what: "cuDNN 9",
            wheel: "nvidia-cudnn-cu12",
            linux: "libcudnn.so.9",
            windows: "cudnn64_9.dll",
        },
    ];

    /// Loaded libraries stay loaded: the provider needs them later.
    static KEPT: Mutex<Vec<Library>> = Mutex::new(Vec::new());

    /// Load a CUDA library by full path or bare name. Failures are logged:
    /// a library that is there but will not load (a missing dependency, the
    /// wrong architecture) otherwise just reads as "not installed".
    fn open(path: &Path) -> Option<Library> {
        // SAFETY: the CUDA libraries' initialisers only set up their own state.
        #[cfg(unix)]
        let opened = {
            use libloading::os::unix::{Library as Unix, RTLD_GLOBAL, RTLD_LAZY};
            unsafe { Unix::open(Some(path), RTLD_LAZY | RTLD_GLOBAL) }.map(Library::from)
        };
        // A bare name only from the program's folder and System32, never the
        // current folder or `PATH`, where any DLL of that name would do; the
        // CUDA and cuDNN folders are then tried by full path.
        #[cfg(windows)]
        let opened = if path.is_absolute() {
            super::open_path(path)
        } else {
            use libloading::os::windows::{Library as Windows, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS};
            unsafe { Windows::load_with_flags(path, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS) }.map(Library::from)
        };
        opened.inspect_err(|e| log::debug!("could not load {}: {e}", path.display())).ok()
    }

    /// Load everything the provider needs. Returns the names of what is
    /// missing and where anything off the loader's path came from.
    pub fn preload() -> (Vec<&'static Needed>, Vec<String>) {
        let dirs = search_dirs();
        let mut missing: Vec<&Needed> = Vec::new();
        let mut loaded = Vec::new();
        let mut kept = Vec::new();
        for need in NEEDED {
            let name = if cfg!(windows) { need.windows } else { need.linux };
            if let Some(lib) = open(Path::new(name)) {
                kept.push(lib);
                continue;
            }
            let found = dirs
                .iter()
                .map(|d| d.join(name))
                .filter(|p| p.is_file())
                .find_map(|p| open(&p).map(|l| (l, p)));
            match found {
                Some((lib, path)) => {
                    log::debug!("loaded {}", path.display());
                    loaded.push(path.display().to_string());
                    kept.push(lib);
                    // cuDNN 9 opens its sub-libraries by name. On Linux it
                    // finds them beside itself; on Windows, load them too.
                    if cfg!(windows) && need.what == "cuDNN 9" {
                        kept.extend(siblings(path.parent()));
                    }
                }
                None => missing.push(need),
            }
        }
        KEPT.lock().unwrap_or_else(|e| e.into_inner()).extend(kept);
        (missing, loaded)
    }

    /// cuDNN 9's sub-libraries beside `dir`'s `cudnn64_9.dll`, the ones the
    /// others depend on (graph, then ops) first.
    fn siblings(dir: Option<&Path>) -> Vec<Library> {
        let Some(Ok(entries)) = dir.map(std::fs::read_dir) else { return Vec::new() };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let name = p.file_name().and_then(OsStr::to_str).unwrap_or("");
                name.starts_with("cudnn_") && name.ends_with("64_9.dll")
            })
            .collect();
        paths.sort_by_key(|p| {
            let name = p.file_name().and_then(OsStr::to_str).unwrap_or("").to_string();
            let rank = if name.starts_with("cudnn_graph") {
                0
            } else if name.starts_with("cudnn_ops") {
                1
            } else {
                2
            };
            (rank, name)
        });
        paths.iter().filter_map(|p| open(p)).collect()
    }

    /// Destroy the CUDA context: the last ~400 MB of video memory, which
    /// ONNX Runtime keeps after its sessions and environment are gone. Only
    /// once nothing uses CUDA any more. Returns whether it ran.
    pub fn reset_device() -> bool {
        let kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: `cudaError_t cudaDeviceReset(void)` from the CUDA runtime
        // this module loaded and keeps loaded.
        let reset = kept
            .iter()
            .find_map(|lib| unsafe { lib.get::<unsafe extern "C" fn() -> i32>(b"cudaDeviceReset\0") }.ok());
        match reset {
            Some(reset) => {
                let code = unsafe { reset() };
                if code != 0 {
                    log::warn!("cudaDeviceReset failed ({code})");
                }
                code == 0
            }
            None => false,
        }
    }

    pub fn blocker(missing: &[&Needed]) -> Option<Blocker> {
        if missing.is_empty() {
            return None;
        }
        let mut names: Vec<&str> = missing.iter().map(|n| n.what).collect();
        names.dedup();
        let mut wheels: Vec<&str> = missing.iter().map(|n| n.wheel).collect();
        wheels.dedup();
        let problem =
            format!("{} {} not installed", and_list(&names), if names.len() == 1 { "is" } else { "are" });
        let fix = if cfg!(windows) {
            "install the CUDA 12 toolkit and cuDNN 9 from developer.nvidia.com, then restart Flow".to_string()
        } else if names == ["cuDNN 9"] {
            "install cuDNN 9 for CUDA 12 (NVIDIA's libcudnn9-cuda-12 package, or `pip install --user nvidia-cudnn-cu12`), then restart Flow".to_string()
        } else {
            format!(
                "install NVIDIA's CUDA 12 and cuDNN 9 packages, or `pip install --user {}`, then restart Flow",
                wheels.join(" ")
            )
        };
        Some(blocked(problem, Some(fix)))
    }

    pub fn and_list(items: &[&str]) -> String {
        match items {
            [] => String::new(),
            [one] => one.to_string(),
            [init @ .., last] => format!("{} and {last}", init.join(", ")),
        }
    }

    /// Where CUDA and cuDNN are usually installed, beyond the loader's own
    /// search path. `FLOW_CUDA_LIBS` (a path list) comes first.
    fn search_dirs() -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(extra) = std::env::var_os("FLOW_CUDA_LIBS") {
            dirs.extend(std::env::split_paths(&extra));
        }
        let lib = if cfg!(windows) { "bin" } else { "lib64" };
        for var in ["CUDA_PATH", "CUDA_HOME"] {
            if let Some(root) = std::env::var_os(var) {
                dirs.push(Path::new(&root).join(lib));
            }
        }
        let wheels = ["cudnn", "cublas", "cuda_runtime", "cufft", "curand"];
        if cfg!(windows) {
            for (_, root) in
                std::env::vars_os().filter(|(k, _)| k.to_string_lossy().starts_with("CUDA_PATH_V12"))
            {
                dirs.push(Path::new(&root).join("bin"));
            }
            if let Some(pf) = std::env::var_os("ProgramFiles") {
                let pf = PathBuf::from(pf);
                dirs.extend(
                    children(&pf.join(r"NVIDIA GPU Computing Toolkit\CUDA"), "v12").map(|d| d.join("bin")),
                );
                // cuDNN 9's installer: CUDNN\v9.x\bin\12.x, sometimes with an x64 below.
                for version in children(&pf.join(r"NVIDIA\CUDNN"), "v9") {
                    for cuda in children(&version.join("bin"), "12") {
                        dirs.push(cuda.join("x64"));
                        dirs.push(cuda);
                    }
                }
            }
            if let Some(appdata) = std::env::var_os("APPDATA") {
                for python in children(&Path::new(&appdata).join("Python"), "Python3") {
                    let site = python.join("site-packages").join("nvidia");
                    dirs.extend(wheels.iter().map(|w| site.join(w).join("bin")));
                }
            }
        } else {
            dirs.push("/usr/local/cuda/lib64".into());
            dirs.extend(children(Path::new("/usr/local"), "cuda-12").map(|d| d.join("lib64")));
            dirs.push("/opt/cuda/lib64".into());
            // pip wheels, installed with --user or system-wide.
            let mut sites: Vec<PathBuf> = Vec::new();
            if let Some(home) = std::env::var_os("HOME") {
                sites.extend(
                    children(&Path::new(&home).join(".local/lib"), "python3")
                        .map(|p| p.join("site-packages")),
                );
            }
            for root in ["/usr/local/lib", "/usr/lib", "/usr/lib64"] {
                for python in children(Path::new(root), "python3") {
                    sites.push(python.join("site-packages"));
                    sites.push(python.join("dist-packages"));
                }
            }
            for site in sites {
                dirs.extend(wheels.iter().map(|w| site.join("nvidia").join(w).join("lib")));
            }
        }
        // Full paths: Windows searches a DLL's own folder for its
        // dependencies only when it was loaded by one.
        let mut dirs: Vec<PathBuf> =
            dirs.into_iter().filter(|d| d.is_dir()).filter_map(|d| std::path::absolute(d).ok()).collect();
        dirs.dedup();
        dirs
    }

    /// Subdirectories of `parent` whose names start with `prefix`, newest
    /// version first.
    fn children(parent: &Path, prefix: &str) -> impl Iterator<Item = PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(parent)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir() && p.file_name().and_then(OsStr::to_str).is_some_and(|n| n.starts_with(prefix))
            })
            .collect();
        found.sort_by_key(|p| std::cmp::Reverse(version_key(p)));
        found.into_iter()
    }

    /// `cuda-12.9` sorts after `cuda-12.10` as text; compare the numbers.
    fn version_key(p: &Path) -> Vec<u32> {
        let name = p.file_name().and_then(OsStr::to_str).unwrap_or("");
        name.split(|c: char| !c.is_ascii_digit()).filter_map(|s| s.parse().ok()).collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn lists_read_naturally() {
            assert_eq!(and_list(&["cuDNN 9"]), "cuDNN 9");
            assert_eq!(and_list(&["a", "b"]), "a and b");
            assert_eq!(and_list(&["a", "b", "c"]), "a, b and c");
        }

        #[test]
        fn a_missing_cudnn_names_its_package() {
            let cudnn = NEEDED.iter().find(|n| n.what == "cuDNN 9").unwrap();
            let b = blocker(&[cudnn]).unwrap();
            assert_eq!(b.problem, "cuDNN 9 is not installed");
            assert!(b.fix.unwrap().to_lowercase().contains("cudnn"));
        }

        #[test]
        fn cublas_is_named_once() {
            let b = blocker(&NEEDED.iter().collect::<Vec<_>>()).unwrap();
            assert_eq!(
                b.problem,
                "the CUDA 12 runtime, cuBLAS 12, cuFFT, cuRAND and cuDNN 9 are not installed"
            );
        }

        #[test]
        fn versions_compare_as_numbers() {
            let mut v = [PathBuf::from("/usr/local/cuda-12.9"), PathBuf::from("/usr/local/cuda-12.10")];
            v.sort_by_key(|p| std::cmp::Reverse(version_key(p)));
            assert_eq!(v[0], PathBuf::from("/usr/local/cuda-12.10"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nvidia(cuda: i32, compute: (i32, i32), memory_mb: u64) -> Result<nvml::Nvidia, Option<Blocker>> {
        Ok(nvml::Nvidia {
            driver: "580.159.03".into(),
            cuda,
            cards: vec![nvml::Card { name: "NVIDIA GeForce RTX 4090".into(), memory_mb, compute }],
        })
    }

    fn card(name: &str, vendor: &str, driver: &str) -> GpuDevice {
        GpuDevice {
            name: name.into(),
            vendor: vendor.into(),
            kind: None,
            memory_mb: None,
            compute: None,
            driver: Some(driver.into()),
        }
    }

    fn adapter(name: &str, vendor: &'static str, ids: (u32, u32), kind: Kind, memory_mb: u64) -> Adapter {
        Adapter { name: name.into(), vendor, vendor_id: ids.0, device_id: ids.1, kind, memory_mb }
    }

    fn radeon() -> Adapter {
        adapter("AMD Radeon RX 7900 XTX", "amd", (0x1002, 0x744c), Kind::Discrete, 24_560)
    }

    fn uhd() -> Adapter {
        adapter("Intel(R) UHD Graphics 770", "intel", (0x8086, 0x4680), Kind::Integrated, 15_800)
    }

    fn lavapipe() -> Adapter {
        adapter("llvmpipe", "other", (0x10005, 0), Kind::Cpu, 64_000)
    }

    #[test]
    fn a_supported_nvidia_card_passes_the_cuda_checks() {
        if cfg!(target_os = "macos") {
            return;
        }
        assert_eq!(cuda_blocker(&nvidia(13_000, (8, 9), 24_564), &[]), None);
    }

    #[test]
    fn cuda_problems_are_named() {
        if cfg!(target_os = "macos") {
            return;
        }
        let problem = |n| cuda_blocker(&n, &[]).map(|b| b.problem).unwrap_or_default();
        assert!(problem(nvidia(11_040, (8, 9), 24_564)).contains("CUDA 11.4"));
        assert!(problem(nvidia(13_000, (6, 1), 8_192)).contains("6.1 is older"));
        assert!(problem(nvidia(13_000, (12, 0), 32_768)).contains("12.0 is newer"));
        assert!(problem(nvidia(13_000, (7, 5), 2_048)).contains("2.0 GB"));
    }

    #[test]
    fn without_nvidias_driver_the_card_is_still_named() {
        if cfg!(target_os = "macos") {
            return;
        }
        let nouveau = [card("NVIDIA GeForce RTX 3060", "nvidia", "nouveau")];
        let b = cuda_blocker(&Err(None), &nouveau).unwrap();
        assert_eq!(b.problem, "NVIDIA's driver is not installed (the card uses nouveau)");
        assert!(b.fix.is_some());

        let amd = [card("AMD Radeon RX 7900 XTX", "amd", "amdgpu")];
        let b = cuda_blocker(&Err(None), &amd).unwrap();
        assert_eq!(
            b.problem,
            "this CUDA build needs an NVIDIA card and this machine has AMD Radeon RX 7900 XTX"
        );
        assert!(b.fix.unwrap().contains("WebGPU"));
    }

    #[test]
    fn webgpu_takes_the_discrete_card() {
        if cfg!(target_os = "macos") {
            return;
        }
        let (gpu, blocker) = webgpu_choice(&Ok(vec![uhd(), radeon(), lavapipe()]), &[]);
        assert_eq!(gpu.as_deref(), Some("AMD Radeon RX 7900 XTX (24 GB)"));
        assert_eq!(blocker, None);
    }

    #[test]
    fn webgpu_leaves_an_integrated_gpu_to_the_cpu() {
        if cfg!(target_os = "macos") {
            return;
        }
        let (gpu, blocker) = webgpu_choice(&Ok(vec![uhd(), lavapipe()]), &[]);
        assert_eq!(gpu.as_deref(), Some("Intel(R) UHD Graphics 770"));
        let b = blocker.unwrap();
        assert!(b.problem.contains("built into the processor"));
        assert_eq!(b.fix, None, "nothing to fix: the CPU is the faster choice");
    }

    #[test]
    fn webgpu_problems_are_named() {
        if cfg!(target_os = "macos") {
            return;
        }
        // Only a software rasteriser: the card itself lacks a driver.
        let amd = [card("AMD Radeon RX 580", "amd", "amdgpu")];
        let (gpu, b) = webgpu_choice(&Ok(vec![lavapipe()]), &amd);
        assert_eq!(gpu.as_deref(), Some("AMD Radeon RX 580"));
        assert_eq!(b.as_ref().map(|b| b.problem.as_str()), Some("it has no Vulkan driver"));
        assert!(b.unwrap().fix.is_some());

        let (gpu, b) = webgpu_choice(&Err("no Vulkan driver is installed".into()), &amd);
        assert_eq!(gpu.as_deref(), Some("AMD Radeon RX 580"));
        assert_eq!(b.unwrap().problem, "no Vulkan driver is installed");

        let small = adapter("AMD Radeon RX 550", "amd", (0x1002, 0x699f), Kind::Discrete, 2_048);
        let (_, b) = webgpu_choice(&Ok(vec![small]), &[]);
        assert!(b.unwrap().problem.contains("2.0 GB"));

        let (gpu, b) = webgpu_choice(&Ok(vec![]), &[]);
        assert_eq!((gpu, b.map(|b| b.problem)), (None, Some("no graphics card found".into())));
    }

    #[test]
    fn devices_merge_without_duplicates() {
        let rtx = (card("NVIDIA GeForce RTX 4090", "nvidia", "nvidia"), (0x10de, 0x2684));
        let igpu = (card("Intel UHD Graphics 770", "intel", "i915"), (0x8086, 0x4680));
        let adapters = [
            adapter("NVIDIA GeForce RTX 4090", "nvidia", (0x10de, 0x2684), Kind::Discrete, 24_564),
            uhd(),
            lavapipe(),
        ];
        let devices = merge_devices(&nvidia(13_000, (8, 9), 24_564), &adapters, vec![igpu, rtx]);
        let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["NVIDIA GeForce RTX 4090", "Intel(R) UHD Graphics 770"]);
        assert_eq!(devices[0].kind.as_deref(), Some("discrete"));
        assert_eq!(devices[0].compute.as_deref(), Some("8.9"), "NVML's details are kept");
        assert_eq!(devices[1].driver.as_deref(), Some("i915"), "the kernel driver comes from the PCI bus");

        // A card no driver claims still shows up, from the PCI bus.
        let bare = (card("AMD Radeon RX 580", "amd", "vfio-pci"), (0x1002, 0x67df));
        let devices = merge_devices(&Err(None), &[uhd()], vec![bare]);
        assert_eq!(devices.len(), 2);
    }

    #[test]
    fn kernels_cover_turing_to_hopper_only() {
        assert!(kernels_cover(7, 5));
        assert!(kernels_cover(8, 6));
        assert!(kernels_cover(8, 9));
        assert!(kernels_cover(9, 0));
        assert!(!kernels_cover(6, 1));
        assert!(!kernels_cover(7, 0));
        assert!(!kernels_cover(12, 0));
    }

    #[test]
    fn describe_says_what_happened() {
        let mut r = GpuReport {
            backend: Some("webgpu".into()),
            devices: vec![],
            gpu: Some("NVIDIA GeForce RTX 4090 (24 GB)".into()),
            driver: None,
            usable: true,
            problem: None,
            fix: None,
            loaded_from: vec![],
        };
        assert_eq!(r.describe(), "NVIDIA GeForce RTX 4090 (24 GB)");
        r.problem = Some("cuDNN 9 is not installed".into());
        assert_eq!(r.describe(), "NVIDIA GeForce RTX 4090 (24 GB) not used: cuDNN 9 is not installed");
        r.gpu = None;
        r.problem = Some("no NVIDIA graphics card".into());
        assert_eq!(r.describe(), "no NVIDIA graphics card");
    }

    #[test]
    fn provider_names() {
        assert!(insists_on_gpu("gpu"));
        assert!(insists_on_gpu("cuda"), "the old name still works");
        assert!(!insists_on_gpu("auto"));
        assert_eq!(precision_for("cpu"), Precision::Int8);
        let gpu = if backend().is_some() { Precision::Fp32 } else { Precision::Int8 };
        assert_eq!(precision_for("gpu"), gpu);
    }

    #[test]
    fn memory_reads_as_gigabytes() {
        assert_eq!(gigabytes(24_564), "24 GB");
        assert_eq!(gigabytes(3_911), "3.8 GB");
    }

    #[test]
    fn vulkan_names_lose_mesas_chip_suffix() {
        assert_eq!(vulkan::tidy_name("AMD Radeon RX 7900 XTX (RADV NAVI31)"), "AMD Radeon RX 7900 XTX");
        assert_eq!(vulkan::tidy_name("Intel(R) UHD Graphics 770 (ADL-S GT1)"), "Intel(R) UHD Graphics 770");
        assert_eq!(vulkan::tidy_name("NVIDIA GeForce RTX 4090"), "NVIDIA GeForce RTX 4090");
    }

    #[test]
    fn pci_ids_lookup_and_names() {
        let ids = "# comment\n10de  NVIDIA Corporation\n\t2684  AD102 [GeForce RTX 4090]\n\t\t1043 889a  ROG\n\t2704  AD103\n1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\t744c  Navi 31 [Radeon RX 7900 XT/7900 XTX/7900M]\n";
        assert_eq!(pci::lookup(ids, 0x10de, 0x2684), Some("AD102 [GeForce RTX 4090]"));
        assert_eq!(pci::lookup(ids, 0x10de, 0x744c), None, "a device id under another vendor");
        assert_eq!(pci::pretty_name(0x10de, pci::lookup(ids, 0x10de, 0x2684)), "NVIDIA GeForce RTX 4090");
        assert_eq!(pci::pretty_name(0x10de, pci::lookup(ids, 0x10de, 0x2704)), "NVIDIA AD103");
        assert_eq!(
            pci::pretty_name(0x1002, pci::lookup(ids, 0x1002, 0x744c)),
            "AMD Radeon RX 7900 XT/7900 XTX/7900M"
        );
        assert_eq!(pci::pretty_name(0x8086, None), "Intel graphics card");
        assert_eq!(pci::pretty_name(0x1234, None), "Graphics card");
    }

    #[test]
    fn provider_dir_follows_argv0() {
        let cwd = Path::new("/home/me");
        assert_eq!(dir_for_argv0(Path::new("/opt/flow/flow"), cwd, None), Some("/opt/flow".into()));
        assert_eq!(
            dir_for_argv0(Path::new("target/debug/flow"), cwd, None),
            Some("/home/me/target/debug".into())
        );
        assert_eq!(dir_for_argv0(Path::new("flow-no-such-program"), cwd, Some(OsStr::new("/nowhere"))), None);
    }
}
