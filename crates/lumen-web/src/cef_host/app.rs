//! CEF application: process-wide switches, scheme registration, and the renderer-side bridge
//! injection.

use std::sync::Arc;

use cef::*;

use super::scheme::{SchemeHandlerFactoryBuilder, scheme_options};
use super::{PAGE_MESSAGE, RELEASED_MESSAGE, SCHEME_SWITCH, Shared};
use crate::bridge::{BRIDGE_JS, BRIDGE_SCRIPT_URL, HOST_POST_FUNCTION};

/// Per-process configuration of the application object.
#[derive(Clone)]
pub struct AppConfig {
    /// `wp<id>`; registered in every process.
    pub scheme: String,
    /// Present in the browser process only.
    pub shared: Option<Arc<Shared>>,
}

fn s(text: &str) -> CefString {
    CefString::from(text)
}

wrap_app! {
    pub struct LumenApp {
        config: AppConfig,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let browser_process = process_type.is_none_or(|t| t.to_string().is_empty());
            let Some(command_line) = command_line else { return };
            if !browser_process {
                return;
            }
            // Spec 07 §15.3, by functional equivalence. Web security stays disabled because the
            // WE contract makes pages load user files through file:/// URLs; the request handler
            // restricts which files (see crate::access).
            command_line.append_switch_with_value(Some(&s("autoplay-policy")), Some(&s("no-user-gesture-required")));
            command_line.append_switch(Some(&s("disable-web-security")));
            command_line.append_switch(Some(&s("disable-site-isolation-trials")));
            command_line.append_switch_with_value(
                Some(&s("disable-features")),
                Some(&s("HardwareMediaKeyHandling,MediaSessionService,IsolateOrigins,site-per-process")),
            );
            command_line.append_switch(Some(&s("disable-gpu-shader-disk-cache")));
            command_line.append_switch(Some(&s("disable-breakpad")));
            command_line.append_switch(Some(&s("disable-component-update")));
            command_line.append_switch(Some(&s("no-first-run")));
            command_line.append_switch(Some(&s("noerrdialogs")));
        }

        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            if let Some(registrar) = registrar
                && !self.config.scheme.is_empty()
            {
                registrar.add_custom_scheme(Some(&s(&self.config.scheme)), scheme_options());
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            let shared = self.config.shared.clone()?;
            Some(LumenBrowserProcessHandler::new(shared, self.config.scheme.clone()))
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(LumenRenderProcessHandler::new(0))
        }
    }
}

wrap_browser_process_handler! {
    pub struct LumenBrowserProcessHandler {
        shared: Arc<Shared>,
        scheme: String,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            let mut factory = SchemeHandlerFactoryBuilder::build(self.shared.files.clone());
            register_scheme_handler_factory(Some(&s(&self.scheme)), None, Some(&mut factory));
            super::create_browser(&self.shared);
        }

        fn on_before_child_process_launch(&self, command_line: Option<&mut CommandLine>) {
            if let Some(command_line) = command_line {
                command_line.append_switch_with_value(Some(&s(SCHEME_SWITCH)), Some(&s(&self.scheme)));
            }
        }
    }
}

wrap_render_process_handler! {
    pub struct LumenRenderProcessHandler {
        _unused: u8,
    }

    impl RenderProcessHandler {
        fn on_context_created(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            let (Some(frame), Some(context)) = (frame, context) else { return };
            if frame.is_main() == 0 {
                return;
            }
            install_bridge(context);
        }

        fn on_context_released(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _context: Option<&mut V8Context>,
        ) {
            let Some(frame) = frame else { return };
            if frame.is_main() == 0 {
                return;
            }
            if let Some(mut message) = process_message_create(Some(&s(RELEASED_MESSAGE))) {
                frame.send_process_message(ProcessId::from(sys::cef_process_id_t::PID_BROWSER), Some(&mut message));
            }
        }
    }
}

/// Defines the native page → host function, then runs the bridge before any page script.
fn install_bridge(context: &mut V8Context) {
    let Some(global) = context.global() else {
        return;
    };
    let mut handler = HostPostHandler::new(0);
    let name = s(HOST_POST_FUNCTION);
    let mut function = v8_value_create_function(Some(&name), Some(&mut handler));
    global.set_value_bykey(
        Some(&name),
        function.as_mut(),
        V8Propertyattribute::from(sys::cef_v8_propertyattribute_t::V8_PROPERTY_ATTRIBUTE_NONE),
    );
    let mut retval = None;
    let mut exception = None;
    if context.eval(
        Some(&s(BRIDGE_JS)),
        Some(&s(BRIDGE_SCRIPT_URL)),
        1,
        Some(&mut retval),
        Some(&mut exception),
    ) == 0
    {
        eprintln!("lumen-web: failed to install the Wallpaper Engine bridge");
    }
}

wrap_v8_handler! {
    pub struct HostPostHandler {
        _unused: u8,
    }

    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> ::std::os::raw::c_int {
            let Some(Some(argument)) = arguments.and_then(|a| a.first()) else { return 0 };
            if argument.is_string() == 0 {
                return 0;
            }
            let json = CefString::from(&argument.string_value());
            let Some(frame) = v8_context_get_current_context().and_then(|c| c.frame()) else { return 0 };
            let Some(mut message) = process_message_create(Some(&s(PAGE_MESSAGE))) else { return 0 };
            if let Some(list) = message.argument_list() {
                list.set_string(0, Some(&json));
            }
            frame.send_process_message(ProcessId::from(sys::cef_process_id_t::PID_BROWSER), Some(&mut message));
            1
        }
    }
}
