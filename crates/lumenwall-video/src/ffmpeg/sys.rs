//! FFmpeg API used by this crate: the types and constants of `ffmpeg-sys-next`, and its
//! functions loaded at the first video (Linux, Windows).
//!
//! FFmpeg's shared libraries pull ≈ 135 other libraries (codecs, VA-API, VDPAU, OpenCL…):
//! linked to the executable, the dynamic loader maps them at every start, scenes included
//! (measured: 24.9 MiB RSS and 12 ms of start-up). The four libraries are opened by [`load`],
//! called before the first use (opening a video), for the major versions of the headers the
//! bindings were generated from, and stay loaded for the rest of the process:
//!
//! - Linux: `dlopen` of the sonames (`libavcodec.so.62`…).
//! - Windows: `LoadLibraryExW` of `avutil-60.dll`, `swscale-9.dll`, `avcodec-62.dll`,
//!   `avformat-62.dll` (the names of FFmpeg's shared builds) by full path, from the first
//!   folder holding all four: the executable's (where the installer puts them), then the
//!   absolute folders of `PATH` (development builds, an FFmpeg installed elsewhere); never
//!   the current directory. Their own dependencies (`swresample-6.dll`…) are resolved in the
//!   same folder, then in System32.
//!
//! The executable keeps no dependency on them (no `DT_NEEDED`, no import entry): no function
//! of `ffmpeg-sys-next` is referenced, every call goes through the table below
//! (`ffmpeg::tests::every_ffmpeg_call_is_loaded_at_run_time` checks the sources), so it starts
//! and plays scenes where FFmpeg is missing; video wallpapers then fail with the loader's
//! message. FFmpeg stays a separate, replaceable shared library (deny.toml, LGPL rule).
//!
//! Elsewhere the functions of `ffmpeg-sys-next` are linked as before, and [`load`] does
//! nothing.

pub(crate) use ffmpeg_sys_next::*;

/// Declares the functions loaded from each library: a wrapper per function with the
/// signature of `ffmpeg-sys-next` (checked by a test), which shadows the linked one.
#[cfg(any(target_os = "linux", windows))]
macro_rules! ffmpeg_api {
    ($( $lib:ident ($major:expr) { $( fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?; )* } )*) => {
        #[allow(non_snake_case)]
        struct Api {
            $($( $name: unsafe extern "C" fn($($ty),*) $(-> $ret)?, )*)*
        }

        impl Api {
            fn open() -> Result<Self, String> {
                let set = dl::LibrarySet::locate(&[$( (stringify!($lib), $major), )*])?;
                $( let $lib = set.open(stringify!($lib), $major)?; )*
                // SAFETY: each symbol is the FFmpeg function of that name, from the library
                // of the major version the bindings (and so the signatures) come from.
                unsafe {
                    Ok(Self {
                        $($( $name: std::mem::transmute::<
                            *mut std::ffi::c_void,
                            unsafe extern "C" fn($($ty),*) $(-> $ret)?,
                        >($lib.symbol(concat!(stringify!($name), "\0"))?), )*)*
                    })
                }
            }
        }

        $($(
            #[allow(non_snake_case, clippy::too_many_arguments)]
            #[doc = concat!("`", stringify!($name), "`, loaded at run time.")]
            ///
            /// # Safety
            ///
            /// The contract of the FFmpeg function.
            pub(crate) unsafe fn $name($($arg: $ty),*) $(-> $ret)? {
                // SAFETY: forwarded to the caller.
                unsafe { (api().$name)($($arg),*) }
            }
        )*)*

        /// Names of the functions loaded at run time.
        #[cfg(test)]
        pub(crate) const LOADED: &[&str] = &[$($( stringify!($name), )*)*];

        /// The wrappers have the signatures of `ffmpeg-sys-next` (compile-time check; the test
        /// binary links FFmpeg).
        #[cfg(test)]
        pub(crate) fn same_signatures() {
            $($( let _: unsafe extern "C" fn($($ty),*) $(-> $ret)? = ffmpeg_sys_next::$name; )*)*
        }
    };
}

#[cfg(any(target_os = "linux", windows))]
ffmpeg_api! {
    avutil(LIBAVUTIL_VERSION_MAJOR) {
        fn av_buffer_ref(buf: *const AVBufferRef) -> *mut AVBufferRef;
        fn av_buffer_unref(buf: *mut *mut AVBufferRef);
        fn av_frame_alloc() -> *mut AVFrame;
        fn av_frame_free(frame: *mut *mut AVFrame);
        fn av_frame_get_buffer(frame: *mut AVFrame, align: libc::c_int) -> libc::c_int;
        fn av_frame_move_ref(dst: *mut AVFrame, src: *mut AVFrame);
        fn av_freep(ptr: *mut libc::c_void);
        fn av_hwdevice_ctx_alloc(type_: AVHWDeviceType) -> *mut AVBufferRef;
        fn av_hwdevice_ctx_create(
            device_ctx: *mut *mut AVBufferRef,
            type_: AVHWDeviceType,
            device: *const libc::c_char,
            opts: *mut AVDictionary,
            flags: libc::c_int,
        ) -> libc::c_int;
        fn av_hwdevice_ctx_init(ref_: *mut AVBufferRef) -> libc::c_int;
        fn av_hwframe_transfer_data(
            dst: *mut AVFrame,
            src: *const AVFrame,
            flags: libc::c_int,
        ) -> libc::c_int;
        fn av_log_set_level(level: libc::c_int);
        fn av_malloc(size: usize) -> *mut libc::c_void;
        fn av_pix_fmt_desc_get(pix_fmt: AVPixelFormat) -> *const AVPixFmtDescriptor;
        fn av_strerror(errnum: libc::c_int, errbuf: *mut libc::c_char, errbuf_size: usize) -> libc::c_int;
    }
    swscale(LIBSWSCALE_VERSION_MAJOR) {
        fn sws_freeContext(swsContext: *mut SwsContext);
        fn sws_getCachedContext(
            context: *mut SwsContext,
            srcW: libc::c_int,
            srcH: libc::c_int,
            srcFormat: AVPixelFormat,
            dstW: libc::c_int,
            dstH: libc::c_int,
            dstFormat: AVPixelFormat,
            flags: libc::c_int,
            srcFilter: *mut SwsFilter,
            dstFilter: *mut SwsFilter,
            param: *const f64,
        ) -> *mut SwsContext;
        fn sws_scale_frame(c: *mut SwsContext, dst: *mut AVFrame, src: *const AVFrame) -> libc::c_int;
    }
    avcodec(LIBAVCODEC_VERSION_MAJOR) {
        fn av_packet_alloc() -> *mut AVPacket;
        fn av_packet_free(pkt: *mut *mut AVPacket);
        fn av_packet_unref(pkt: *mut AVPacket);
        fn avcodec_alloc_context3(codec: *const AVCodec) -> *mut AVCodecContext;
        fn avcodec_default_get_format(s: *mut AVCodecContext, fmt: *const AVPixelFormat) -> AVPixelFormat;
        fn avcodec_flush_buffers(avctx: *mut AVCodecContext);
        fn avcodec_free_context(avctx: *mut *mut AVCodecContext);
        fn avcodec_get_hw_config(codec: *const AVCodec, index: libc::c_int) -> *const AVCodecHWConfig;
        fn avcodec_open2(
            avctx: *mut AVCodecContext,
            codec: *const AVCodec,
            options: *mut *mut AVDictionary,
        ) -> libc::c_int;
        fn avcodec_parameters_to_context(
            codec: *mut AVCodecContext,
            par: *const AVCodecParameters,
        ) -> libc::c_int;
        fn avcodec_receive_frame(avctx: *mut AVCodecContext, frame: *mut AVFrame) -> libc::c_int;
        fn avcodec_send_packet(avctx: *mut AVCodecContext, avpkt: *const AVPacket) -> libc::c_int;
    }
    avformat(LIBAVFORMAT_VERSION_MAJOR) {
        fn av_find_best_stream(
            ic: *mut AVFormatContext,
            type_: AVMediaType,
            wanted_stream_nb: libc::c_int,
            related_stream: libc::c_int,
            decoder_ret: *mut *const AVCodec,
            flags: libc::c_int,
        ) -> libc::c_int;
        fn av_read_frame(s: *mut AVFormatContext, pkt: *mut AVPacket) -> libc::c_int;
        fn av_seek_frame(
            s: *mut AVFormatContext,
            stream_index: libc::c_int,
            timestamp: i64,
            flags: libc::c_int,
        ) -> libc::c_int;
        fn avformat_alloc_context() -> *mut AVFormatContext;
        fn avformat_close_input(s: *mut *mut AVFormatContext);
        fn avformat_find_stream_info(
            ic: *mut AVFormatContext,
            options: *mut *mut AVDictionary,
        ) -> libc::c_int;
        fn avformat_open_input(
            ps: *mut *mut AVFormatContext,
            url: *const libc::c_char,
            fmt: *const AVInputFormat,
            options: *mut *mut AVDictionary,
        ) -> libc::c_int;
        fn avformat_seek_file(
            s: *mut AVFormatContext,
            stream_index: libc::c_int,
            min_ts: i64,
            ts: i64,
            max_ts: i64,
            flags: libc::c_int,
        ) -> libc::c_int;
        fn avio_alloc_context(
            buffer: *mut libc::c_uchar,
            buffer_size: libc::c_int,
            write_flag: libc::c_int,
            opaque: *mut libc::c_void,
            read_packet: Option<
                unsafe extern "C" fn(opaque: *mut libc::c_void, buf: *mut u8, buf_size: libc::c_int) -> libc::c_int,
            >,
            write_packet: Option<
                unsafe extern "C" fn(opaque: *mut libc::c_void, buf: *const u8, buf_size: libc::c_int) -> libc::c_int,
            >,
            seek: Option<unsafe extern "C" fn(opaque: *mut libc::c_void, offset: i64, whence: libc::c_int) -> i64>,
        ) -> *mut AVIOContext;
        fn avio_context_free(s: *mut *mut AVIOContext);
    }
}

#[cfg(any(target_os = "linux", windows))]
static API: std::sync::OnceLock<Result<Api, String>> = std::sync::OnceLock::new();

/// Opens the FFmpeg libraries (once; later calls return the first outcome).
#[cfg(any(target_os = "linux", windows))]
pub(crate) fn load() -> Result<(), String> {
    API.get_or_init(|| {
        let t0 = std::time::Instant::now();
        let api = Api::open();
        if api.is_ok() {
            tracing::debug!(
                "FFmpeg libraries loaded in {:.1} ms",
                t0.elapsed().as_secs_f64() * 1000.0
            );
        }
        api
    })
    .as_ref()
    .map(|_| ())
    .map_err(Clone::clone)
}

#[cfg(any(target_os = "linux", windows))]
fn api() -> &'static Api {
    match API.get_or_init(Api::open) {
        Ok(api) => api,
        // `load` runs before any FFmpeg object exists (`VideoPlayer::open_input`).
        Err(e) => panic!("FFmpeg used without its libraries: {e}"),
    }
}

/// Linked at build time: nothing to load.
#[cfg(not(any(target_os = "linux", windows)))]
pub(crate) fn load() -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "linux")]
mod dl {
    use std::ffi::{CStr, CString, c_void};

    /// Libraries are found by soname through the dynamic loader's search path.
    pub(super) struct LibrarySet;

    impl LibrarySet {
        pub(super) fn locate(_libraries: &[(&str, libc::c_int)]) -> Result<Self, String> {
            Ok(Self)
        }

        pub(super) fn open(&self, name: &str, major: libc::c_int) -> Result<Library, String> {
            Library::open(name, major)
        }
    }

    /// A library opened for the rest of the process (never closed: FFmpeg objects and
    /// threads may outlive any owner here).
    pub(super) struct Library {
        handle: *mut c_void,
        name: String,
    }

    fn last_error() -> String {
        // SAFETY: `dlerror` returns null or a NUL-terminated message of this thread.
        let e = unsafe { libc::dlerror() };
        if e.is_null() {
            "unknown error".into()
        } else {
            // SAFETY: as above.
            unsafe { CStr::from_ptr(e) }.to_string_lossy().into_owned()
        }
    }

    impl Library {
        /// `lib<name>.so.<major>`.
        pub(super) fn open(name: &str, major: libc::c_int) -> Result<Self, String> {
            let soname = format!("lib{name}.so.{major}");
            let c = CString::new(soname.clone()).map_err(|e| e.to_string())?;
            // SAFETY: NUL-terminated name; the library's initialisers have no precondition.
            let handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                return Err(format!("FFmpeg 8 shared library {}", last_error()));
            }
            Ok(Self {
                handle,
                name: soname,
            })
        }

        /// Address of `symbol` (NUL-terminated).
        pub(super) fn symbol(&self, symbol: &str) -> Result<*mut c_void, String> {
            let c = CStr::from_bytes_with_nul(symbol.as_bytes()).map_err(|e| e.to_string())?;
            // SAFETY: valid handle and NUL-terminated name.
            let p = unsafe { libc::dlsym(self.handle, c.as_ptr()) };
            if p.is_null() {
                Err(format!("{}: {}", self.name, last_error()))
            } else {
                Ok(p)
            }
        }
    }
}

#[cfg(windows)]
mod dl {
    use std::ffi::{CStr, c_char, c_void};
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    /// `LoadLibraryExW` flags: the DLL's own dependencies are looked for in its folder, then
    /// in the application folder and System32 (never the current directory or `PATH`).
    const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
    const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x0000_1000;
    /// `ERROR_MOD_NOT_FOUND`.
    const MOD_NOT_FOUND: i32 = 126;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryExW(file: *const u16, reserved: *mut c_void, flags: u32) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    /// File name of FFmpeg's shared builds: `avcodec-62.dll`.
    fn file_name(name: &str, major: libc::c_int) -> String {
        format!("{name}-{major}.dll")
    }

    /// The folder all the DLLs are loaded from.
    pub(super) struct LibrarySet {
        dir: PathBuf,
    }

    impl LibrarySet {
        /// The executable's folder, then the absolute folders of `PATH`: the first one holding
        /// every library.
        pub(super) fn locate(libraries: &[(&str, libc::c_int)]) -> Result<Self, String> {
            let files: Vec<String> = libraries.iter().map(|&(n, m)| file_name(n, m)).collect();
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf));
            let path_dirs = std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
                .unwrap_or_default();
            let complete = |d: &Path| files.iter().all(|f| d.join(f).is_file());
            let found = exe_dir
                .iter()
                .chain(path_dirs.iter().filter(|d| d.is_absolute()))
                .find(|d| complete(d));
            if let Some(dir) = found {
                tracing::debug!("FFmpeg DLLs found in {}", dir.display());
                return Ok(Self { dir: dir.clone() });
            }
            let mut message = format!(
                "the FFmpeg 8 DLLs ({}) are neither next to the executable",
                files.join(", ")
            );
            if let Some(dir) = &exe_dir {
                message += &format!(" ({})", dir.display());
                let missing: Vec<&str> = files
                    .iter()
                    .filter(|f| !dir.join(f).is_file())
                    .map(String::as_str)
                    .collect();
                if missing.len() < files.len() {
                    message += &format!(", where {} is missing,", missing.join(", "));
                }
            }
            Err(message + " nor in a folder of PATH")
        }

        pub(super) fn open(&self, name: &str, major: libc::c_int) -> Result<Library, String> {
            let path = self.dir.join(file_name(name, major));
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
            // SAFETY: NUL-terminated absolute path; FFmpeg's DLL initialisers have no
            // precondition.
            let handle = unsafe {
                LoadLibraryExW(
                    wide.as_ptr(),
                    std::ptr::null_mut(),
                    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
                )
            };
            if handle.is_null() {
                let e = std::io::Error::last_os_error();
                let hint = if e.raw_os_error() == Some(MOD_NOT_FOUND) {
                    " (a DLL it depends on, such as swresample, is missing from that folder)"
                } else {
                    ""
                };
                return Err(format!("FFmpeg 8 DLL {}: {e}{hint}", path.display()));
            }
            Ok(Library {
                handle,
                name: file_name(name, major),
            })
        }
    }

    /// A DLL loaded for the rest of the process (never freed: FFmpeg objects and threads may
    /// outlive any owner here).
    pub(super) struct Library {
        handle: *mut c_void,
        name: String,
    }

    impl Library {
        /// Address of `symbol` (NUL-terminated).
        pub(super) fn symbol(&self, symbol: &str) -> Result<*mut c_void, String> {
            let c = CStr::from_bytes_with_nul(symbol.as_bytes()).map_err(|e| e.to_string())?;
            // SAFETY: valid module handle and NUL-terminated name.
            let p = unsafe { GetProcAddress(self.handle, c.as_ptr()) };
            if p.is_null() {
                let e = std::io::Error::last_os_error();
                Err(format!(
                    "{}: {}: {e}",
                    self.name,
                    &symbol[..symbol.len() - 1]
                ))
            } else {
                Ok(p)
            }
        }
    }
}
