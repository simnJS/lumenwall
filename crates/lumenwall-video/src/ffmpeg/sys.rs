//! FFmpeg API used by this crate: the types and constants of `ffmpeg-sys-next`, and its
//! functions loaded at the first video (Linux).
//!
//! FFmpeg's shared libraries pull ≈ 135 other libraries (codecs, VA-API, VDPAU, OpenCL…):
//! linked to the executable, the dynamic loader maps them at every start, scenes included
//! (measured: 24.9 MiB RSS and 12 ms of start-up). On Linux the four libraries are opened
//! with `dlopen` by [`load`], called before the first use (opening a video), by their
//! sonames for the major versions of the headers the bindings were generated from
//! (`libavcodec.so.62`…), and stay loaded for the rest of the process. The executable keeps
//! no `DT_NEEDED` on them: no function of `ffmpeg-sys-next` is referenced, every call goes
//! through the table below (`ffmpeg::tests::every_ffmpeg_call_is_loaded_at_run_time` checks
//! the sources). FFmpeg stays a separate, replaceable shared library (deny.toml, LGPL rule).
//!
//! Elsewhere (Windows) the functions of `ffmpeg-sys-next` are linked as before (import
//! libraries of the DLLs), and [`load`] does nothing.

pub(crate) use ffmpeg_sys_next::*;

/// Declares the functions loaded from each library: a wrapper per function with the
/// signature of `ffmpeg-sys-next` (checked by a test), which shadows the linked one.
#[cfg(target_os = "linux")]
macro_rules! ffmpeg_api {
    ($( $lib:ident ($major:expr) { $( fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?; )* } )*) => {
        #[allow(non_snake_case)]
        struct Api {
            $($( $name: unsafe extern "C" fn($($ty),*) $(-> $ret)?, )*)*
        }

        impl Api {
            fn open() -> Result<Self, String> {
                $( let $lib = dl::Library::open(stringify!($lib), $major)?; )*
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
static API: std::sync::OnceLock<Result<Api, String>> = std::sync::OnceLock::new();

/// Opens the FFmpeg libraries (once; later calls return the first outcome).
#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn api() -> &'static Api {
    match API.get_or_init(Api::open) {
        Ok(api) => api,
        // `load` runs before any FFmpeg object exists (`VideoPlayer::open_input`).
        Err(e) => panic!("FFmpeg used without its libraries: {e}"),
    }
}

/// Linked at build time: nothing to load.
#[cfg(not(target_os = "linux"))]
pub(crate) fn load() -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "linux")]
mod dl {
    use std::ffi::{CStr, CString, c_void};

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
