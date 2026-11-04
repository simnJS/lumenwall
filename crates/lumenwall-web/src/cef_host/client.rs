//! Browser client: offscreen painting, lifetime, loading, `file://` policy, audio and the page
//! → host channel.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use cef::*;

use super::{PAGE_MESSAGE, RELEASED_MESSAGE, Shared, UiAction};
use crate::ipc::{FrameInfo, HostEvent, PixelFormat};

/// Longest time the browser's paint is held waiting for the main process to copy a frame.
pub const FRAME_RELEASE_TIMEOUT: Duration = Duration::from_millis(100);

/// Set to print the page's console messages on standard error (debugging wallpapers).
pub const CONSOLE_ENV: &str = "LUMENWALL_WEB_CONSOLE";

pub fn build_client(shared: Arc<Shared>) -> Client {
    let console = std::env::var_os(CONSOLE_ENV).is_some_and(|v| v != "0");
    LumenClient::new(
        LumenRenderHandler::new(shared.clone()),
        LumenLifeSpanHandler::new(shared.clone()),
        LumenLoadHandler::new(shared.clone()),
        LumenRequestHandler::new(shared.clone()),
        LumenAudioHandler::new(shared.clone()),
        console.then(|| LumenDisplayHandler::new(0)),
        shared,
    )
}

wrap_display_handler! {
    pub struct LumenDisplayHandler {
        _unused: u8,
    }

    impl DisplayHandler {
        fn on_console_message(
            &self,
            _browser: Option<&mut Browser>,
            _level: LogSeverity,
            message: Option<&CefString>,
            source: Option<&CefString>,
            line: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let message = message.map(|m| m.to_string()).unwrap_or_default();
            let source = source.map(|s| s.to_string()).unwrap_or_default();
            eprintln!("web console: {message} ({source}:{line})");
            1
        }
    }
}

wrap_client! {
    pub struct LumenClient {
        render: RenderHandler,
        life_span: LifeSpanHandler,
        load: LoadHandler,
        request: RequestHandler,
        audio: AudioHandler,
        display: Option<DisplayHandler>,
        shared: Arc<Shared>,
    }

    impl Client {
        fn display_handler(&self) -> Option<DisplayHandler> {
            self.display.clone()
        }

        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.request.clone())
        }

        fn audio_handler(&self) -> Option<AudioHandler> {
            Some(self.audio.clone())
        }

        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            let Some(message) = message else { return 0 };
            let name = CefString::from(&message.name()).to_string();
            if name == PAGE_MESSAGE {
                let json = message
                    .argument_list()
                    .map(|list| CefString::from(&list.string(0)).to_string())
                    .unwrap_or_default();
                super::on_ui(UiAction::PageMessage(json));
                1
            } else if name == RELEASED_MESSAGE {
                super::on_ui(UiAction::ContextReleased);
                1
            } else {
                0
            }
        }
    }
}

wrap_render_handler! {
    pub struct LumenRenderHandler {
        shared: Arc<Shared>,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                let (w, h) = self.shared.size();
                rect.x = 0;
                rect.y = 0;
                rect.width = w.max(1) as i32;
                rect.height = h.max(1) as i32;
            }
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> ::std::os::raw::c_int {
            let Some(info) = screen_info else { return 0 };
            let (w, h) = self.shared.size();
            // Physical pixels, device scale factor 1 (spec 07 §15.4).
            info.device_scale_factor = 1.0;
            info.rect = Rect { x: 0, y: 0, width: w.max(1) as i32, height: h.max(1) as i32 };
            info.available_rect = info.rect.clone();
            1
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            if type_ != PaintElementType::from(sys::cef_paint_element_type_t::PET_VIEW) {
                return;
            }
            #[cfg(any(target_os = "linux", windows))]
            if self.shared.params.transport == crate::ipc::FrameTransport::Memory {
                if buffer.is_null() || width <= 0 || height <= 0 {
                    return;
                }
                let len = width as usize * height as usize * 4;
                // SAFETY: CEF passes a BGRA buffer of width × height pixels (stride width × 4),
                // valid for the duration of the callback.
                let pixels = unsafe { std::slice::from_raw_parts(buffer, len) };
                forward_memory_frame(&self.shared, pixels, width as u32, height as u32);
                return;
            }
            let _ = (buffer, width, height);
            // Shared textures requested but GPU compositing is unavailable.
            if !self.shared.software_warned.swap(true, Ordering::Relaxed) {
                self.shared.send_event(&HostEvent::Error {
                    message: "browser fell back to software painting; shared textures unavailable".into(),
                });
            }
        }

        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            let Some(info) = info else { return };
            if type_ != PaintElementType::from(sys::cef_paint_element_type_t::PET_VIEW) {
                return; // <select> popups are not shown on a wallpaper.
            }
            forward_frame(&self.shared, info);
        }
    }
}

/// Sends the frame to the main process and holds the browser's texture until it is copied.
fn forward_frame(shared: &Shared, info: &AcceleratedPaintInfo) {
    let id = shared.next_frame.fetch_add(1, Ordering::Relaxed) + 1;
    let format = if *info.format.as_ref() == sys::cef_color_type_t::CEF_COLOR_TYPE_RGBA_8888 {
        PixelFormat::Rgba8
    } else {
        PixelFormat::Bgra8
    };
    let coded = &info.extra.coded_size;
    let visible = &info.extra.visible_rect;
    let mut frame = FrameInfo {
        id,
        width: coded.width.max(0) as u32,
        height: coded.height.max(0) as u32,
        visible: [visible.x, visible.y, visible.width, visible.height],
        format,
        modifier: 0,
        planes: Vec::new(),
        handle: 0,
        memory: false,
    };

    #[cfg(unix)]
    let sent = {
        use std::os::fd::BorrowedFd;
        let count = usize::try_from(info.plane_count)
            .unwrap_or(0)
            .min(info.planes.len());
        let planes = &info.planes[..count];
        if planes.is_empty() || planes.iter().any(|p| p.fd < 0) {
            return;
        }
        frame.modifier = info.modifier;
        frame.planes = planes
            .iter()
            .map(|p| crate::ipc::PlaneInfo {
                offset: p.offset,
                stride: p.stride,
                size: p.size,
            })
            .collect();
        // SAFETY: the descriptors belong to CEF and stay open for the whole callback, which
        // outlives this borrow; sendmsg duplicates them into the main process.
        let fds: Vec<BorrowedFd<'_>> = planes
            .iter()
            .map(|p| unsafe { BorrowedFd::borrow_raw(p.fd) })
            .collect();
        shared.send_frame(&HostEvent::Frame(frame), &fds)
    };

    #[cfg(windows)]
    let sent = {
        if info.shared_texture_handle.is_null() {
            return;
        }
        // The main process duplicates this handle out of our process while we wait below.
        frame.handle = info.shared_texture_handle as usize as u64;
        shared.send_frame(&HostEvent::Frame(frame), &[])
    };

    if sent {
        wait_release(shared, id);
    }
}

/// Software painting: copies the pixels into the shared memory buffer, sends it and waits for
/// the main process to upload it (the next paint overwrites the same buffer).
#[cfg(any(target_os = "linux", windows))]
fn forward_memory_frame(shared: &Shared, pixels: &[u8], width: u32, height: u32) {
    let Ok(mut memory) = shared.memory.lock() else {
        return;
    };
    if memory.as_ref().is_none_or(|m| m.len() != pixels.len()) {
        *memory = None;
        match super::memory::SharedPixels::new(pixels.len()) {
            Ok(m) => *memory = Some(m),
            Err(e) => {
                if !shared.software_warned.swap(true, Ordering::Relaxed) {
                    shared.send_event(&HostEvent::Error {
                        message: format!("shared memory for frames: {e}"),
                    });
                }
                return;
            }
        }
    }
    let Some(buffer) = memory.as_mut() else {
        return;
    };
    buffer.as_mut_slice().copy_from_slice(pixels);
    let id = shared.next_frame.fetch_add(1, Ordering::Relaxed) + 1;
    let frame = FrameInfo {
        id,
        width,
        height,
        visible: [0, 0, width as i32, height as i32],
        format: PixelFormat::Bgra8,
        modifier: 0,
        planes: vec![crate::ipc::PlaneInfo {
            offset: 0,
            stride: width * 4,
            size: pixels.len() as u64,
        }],
        // Windows: the section handle, duplicated by the main process while we wait below.
        #[cfg(windows)]
        handle: buffer.handle_value(),
        #[cfg(not(windows))]
        handle: 0,
        memory: true,
    };
    #[cfg(target_os = "linux")]
    let sent = {
        use std::os::fd::AsFd;
        shared.send_frame(&HostEvent::Frame(frame), &[buffer.fd().as_fd()])
    };
    #[cfg(windows)]
    let sent = shared.send_frame(&HostEvent::Frame(frame), &[]);
    if sent {
        wait_release(shared, id);
    }
}

/// Holds the browser (UI thread) until the main process released frame `id`, at most
/// [`FRAME_RELEASE_TIMEOUT`].
fn wait_release(shared: &Shared, id: u64) {
    let Ok(releases) = shared.releases.lock() else {
        return;
    };
    let deadline = Instant::now() + FRAME_RELEASE_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match releases.recv_timeout(remaining) {
            Ok(released) if released == id => return,
            Ok(_) => continue, // Late release of an older, revoked frame.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                shared.send_event(&HostEvent::FrameRevoked { id });
                return;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

wrap_life_span_handler! {
    pub struct LumenLifeSpanHandler {
        shared: Arc<Shared>,
    }

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            _target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            1 // No popup windows from a wallpaper.
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                super::on_ui(UiAction::BrowserCreated(browser.clone()));
            }
            self.shared.send_event(&HostEvent::Started { pid: std::process::id() });
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            super::on_ui(UiAction::BrowserClosed);
        }
    }
}

wrap_load_handler! {
    pub struct LumenLoadHandler {
        shared: Arc<Shared>,
    }

    impl LoadHandler {
        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            http_status_code: ::std::os::raw::c_int,
        ) {
            let Some(frame) = frame else { return };
            if frame.is_main() != 0 {
                let url = CefString::from(&frame.url()).to_string();
                self.shared.send_event(&HostEvent::Loaded { url, status: http_status_code });
            }
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            let Some(frame) = frame else { return };
            if frame.is_main() != 0 {
                let url = failed_url.map(|u| u.to_string()).unwrap_or_default();
                let text = error_text.map(|t| t.to_string()).unwrap_or_default();
                let error = format!("{text} ({:?})", error_code.as_ref());
                self.shared.send_event(&HostEvent::LoadFailed { url, error });
            }
        }
    }
}

wrap_request_handler! {
    pub struct LumenRequestHandler {
        shared: Arc<Shared>,
    }

    impl RequestHandler {
        fn resource_request_handler(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _is_navigation: ::std::os::raw::c_int,
            _is_download: ::std::os::raw::c_int,
            _request_initiator: Option<&CefString>,
            _disable_default_handling: Option<&mut ::std::os::raw::c_int>,
        ) -> Option<ResourceRequestHandler> {
            let url = CefString::from(&request?.url()).to_string();
            // Only file:// needs a decision; everything else uses the default handling.
            url.get(..7)?.eq_ignore_ascii_case("file://").then(|| LumenResourceRequestHandler::new(self.shared.clone()))
        }

        fn on_render_process_terminated(
            &self,
            _browser: Option<&mut Browser>,
            _status: TerminationStatus,
            error_code: ::std::os::raw::c_int,
            error_string: Option<&CefString>,
        ) {
            let detail = error_string.map(|e| e.to_string()).unwrap_or_default();
            self.shared.send_event(&HostEvent::Error {
                message: format!("web page renderer terminated ({error_code}): {detail}"),
            });
            super::on_ui(UiAction::ContextReleased);
        }
    }
}

wrap_resource_request_handler! {
    pub struct LumenResourceRequestHandler {
        shared: Arc<Shared>,
    }

    impl ResourceRequestHandler {
        fn on_before_resource_load(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _callback: Option<&mut Callback>,
        ) -> ReturnValue {
            let allowed = request
                .map(|r| CefString::from(&r.url()).to_string())
                .is_some_and(|url| self.shared.access.read().is_ok_and(|a| a.allows_url(&url)));
            ReturnValue::from(if allowed {
                sys::cef_return_value_t::RV_CONTINUE
            } else {
                sys::cef_return_value_t::RV_CANCEL
            })
        }
    }
}

wrap_audio_handler! {
    pub struct LumenAudioHandler {
        shared: Arc<Shared>,
    }

    impl AudioHandler {
        fn audio_parameters(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&mut AudioParameters>,
        ) -> ::std::os::raw::c_int {
            let Some(params) = params else { return 0 };
            // No usable device: let the browser play the audio itself (mute still applies).
            let Some(rate) = self.shared.audio.device_rate() else { return 0 };
            params.sample_rate = rate as i32;
            params.channel_layout = ChannelLayout::from(sys::cef_channel_layout_t::CEF_CHANNEL_LAYOUT_STEREO);
            params.frames_per_buffer = (rate / 100).max(64) as i32;
            1
        }

        fn on_audio_stream_started(
            &self,
            _browser: Option<&mut Browser>,
            _params: Option<&AudioParameters>,
            channels: ::std::os::raw::c_int,
        ) {
            self.shared.audio_channels.store(channels.max(0) as u32, Ordering::Relaxed);
            self.shared.audio.stream_started();
        }

        fn on_audio_stream_packet(
            &self,
            _browser: Option<&mut Browser>,
            data: *mut *const f32,
            frames: ::std::os::raw::c_int,
            _pts: i64,
        ) {
            let channels = self.shared.audio_channels.load(Ordering::Relaxed) as usize;
            if data.is_null() || frames <= 0 || channels == 0 {
                return;
            }
            let frames = frames as usize;
            let mut planes: [&[f32]; 2] = [&[], &[]];
            for (c, plane) in planes.iter_mut().enumerate().take(channels.min(2)) {
                // SAFETY: CEF passes `channels` plane pointers of `frames` samples each, valid
                // for the duration of the callback.
                let ptr = unsafe { *data.add(c) };
                if ptr.is_null() {
                    return;
                }
                *plane = unsafe { std::slice::from_raw_parts(ptr, frames) };
            }
            self.shared.audio.push_planar(&planes[..channels.min(2)]);
        }

        fn on_audio_stream_stopped(&self, _browser: Option<&mut Browser>) {
            self.shared.audio.stream_stopped();
        }

        fn on_audio_stream_error(&self, _browser: Option<&mut Browser>, message: Option<&CefString>) {
            let detail = message.map(|m| m.to_string()).unwrap_or_default();
            tracing::warn!("web audio stream error: {detail}");
            self.shared.audio.stream_stopped();
        }
    }
}
