//! `wp<id>://` requests served from the wallpaper folder ([`crate::files::FileService`]).

use std::sync::{Arc, Mutex};

use cef::*;

use crate::files::{FileError, FileResponse, FileService};
use crate::mime::is_text;

/// Scheme options: standard, secure, CORS- and fetch-enabled (spec 07 §15.2).
pub fn scheme_options() -> i32 {
    use cef::sys::cef_scheme_options_t as O;
    (O::CEF_SCHEME_OPTION_STANDARD as i32)
        | (O::CEF_SCHEME_OPTION_SECURE as i32)
        | (O::CEF_SCHEME_OPTION_CORS_ENABLED as i32)
        | (O::CEF_SCHEME_OPTION_FETCH_ENABLED as i32)
}

#[derive(Clone)]
pub struct SchemeFactory {
    pub files: Arc<FileService>,
}

wrap_scheme_handler_factory! {
    pub struct SchemeHandlerFactoryBuilder {
        factory: SchemeFactory,
    }

    impl SchemeHandlerFactory {
        fn create(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            _request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            Some(FileResourceHandler::new(self.factory.files.clone(), Arc::new(Mutex::new(None))))
        }
    }
}

impl SchemeHandlerFactoryBuilder {
    pub fn build(files: Arc<FileService>) -> SchemeHandlerFactory {
        Self::new(SchemeFactory { files })
    }
}

type Slot = Arc<Mutex<Option<Result<FileResponse, FileError>>>>;

wrap_resource_handler! {
    pub struct FileResourceHandler {
        files: Arc<FileService>,
        response: Slot,
    }

    impl ResourceHandler {
        fn open(
            &self,
            request: Option<&mut Request>,
            handle_request: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut Callback>,
        ) -> ::std::os::raw::c_int {
            let Some(request) = request else { return 0 };
            let url = CefString::from(&request.url()).to_string();
            let range = CefString::from(&request.header_by_name(Some(&CefString::from("Range")))).to_string();
            let range = (!range.is_empty()).then_some(range);
            let result = self.files.open(&url, range.as_deref());
            if let Err(e) = &result {
                tracing::debug!("{url}: {e}");
            }
            if let Ok(mut slot) = self.response.lock() {
                *slot = Some(result);
            }
            if let Some(handle) = handle_request {
                *handle = 1;
            }
            1
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            let Some(response) = response else { return };
            let Ok(slot) = self.response.lock() else { return };
            let set = |name: &str, value: &str| {
                response.set_header_by_name(Some(&CefString::from(name)), Some(&CefString::from(value)), 1);
            };
            set("Access-Control-Allow-Origin", "*");
            let length = match slot.as_ref() {
                Some(Ok(file)) => {
                    response.set_status(i32::from(file.status));
                    response.set_status_text(Some(&CefString::from(if file.status == 206 { "Partial Content" } else { "OK" })));
                    response.set_mime_type(Some(&CefString::from(file.mime)));
                    if is_text(file.mime) {
                        response.set_charset(Some(&CefString::from("utf-8")));
                    }
                    for (name, value) in file.headers() {
                        set(name, &value);
                    }
                    file.length as i64
                }
                Some(Err(e)) => {
                    response.set_status(i32::from(e.status()));
                    response.set_status_text(Some(&CefString::from(match e {
                        FileError::RangeNotSatisfiable { .. } => "Range Not Satisfiable",
                        _ => "File not found",
                    })));
                    response.set_mime_type(Some(&CefString::from("text/plain")));
                    if let FileError::RangeNotSatisfiable { total } = e {
                        set("Content-Range", &format!("bytes */{total}"));
                    }
                    0
                }
                None => {
                    response.set_status(404);
                    0
                }
            };
            if let Some(out) = response_length {
                *out = length;
            }
        }

        fn skip(
            &self,
            bytes_to_skip: i64,
            bytes_skipped: Option<&mut i64>,
            _callback: Option<&mut ResourceSkipCallback>,
        ) -> ::std::os::raw::c_int {
            let skipped = match self.response.lock().ok().as_deref_mut() {
                Some(Some(Ok(file))) => file.skip(bytes_to_skip.max(0) as u64).map_or(-2, |n| n as i64),
                _ => -2,
            };
            if let Some(out) = bytes_skipped {
                *out = skipped;
            }
            i32::from(skipped >= 0)
        }

        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: ::std::os::raw::c_int,
            bytes_read: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> ::std::os::raw::c_int {
            let Some(bytes_read) = bytes_read else { return 0 };
            *bytes_read = 0;
            if data_out.is_null() || bytes_to_read <= 0 {
                return 0;
            }
            // SAFETY: CEF provides a writable buffer of `bytes_to_read` bytes for this call.
            let buf = unsafe { std::slice::from_raw_parts_mut(data_out, bytes_to_read as usize) };
            let Ok(mut slot) = self.response.lock() else { return 0 };
            match slot.as_mut() {
                Some(Ok(file)) => match file.read(buf) {
                    Ok(n) if n > 0 => {
                        *bytes_read = n as i32;
                        1
                    }
                    Ok(_) => 0, // Complete.
                    Err(_) => {
                        *bytes_read = -2; // ERR_FAILED
                        0
                    }
                },
                _ => 0,
            }
        }

        fn cancel(&self) {
            if let Ok(mut slot) = self.response.lock() {
                *slot = None;
            }
        }
    }
}
