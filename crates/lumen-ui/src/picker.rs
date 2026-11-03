//! Native file and folder pickers, run off the UI thread so the window keeps
//! repainting while the dialog is open. Linux: the XDG desktop portal over D-Bus
//! (works on GNOME, KDE, Flatpak). Windows: the system dialog through `rfd`.

use std::path::PathBuf;

/// Opens a picker on its own thread; resolves to `None` when cancelled or unavailable.
pub async fn pick_async(directory: bool) -> Option<PathBuf> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("lumen-ui-picker".into())
        .spawn(move || {
            let _ = tx.send(pick(directory));
        });
    if spawned.is_err() {
        return None;
    }
    rx.await.ok().flatten()
}

#[cfg(windows)]
pub fn pick(directory: bool) -> Option<PathBuf> {
    let dialog = rfd::FileDialog::new();
    if directory {
        dialog.pick_folder()
    } else {
        dialog.pick_file()
    }
}

#[cfg(target_os = "linux")]
pub fn pick(directory: bool) -> Option<PathBuf> {
    match portal::pick(directory) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("file chooser portal unavailable: {e}");
            None
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn pick(_directory: bool) -> Option<PathBuf> {
    None
}

#[cfg(target_os = "linux")]
mod portal {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    pub fn pick(directory: bool) -> zbus::Result<Option<PathBuf>> {
        let conn = Connection::session()?;
        let token = format!("lumen{}", std::process::id());
        let sender = conn
            .unique_name()
            .map(|n| n.trim_start_matches(':').replace('.', "_"))
            .unwrap_or_default();
        let request = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
        // Listen before calling, so the response cannot be missed.
        let req = Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            request.as_str(),
            "org.freedesktop.portal.Request",
        )?;
        let mut responses = req.receive_signal("Response")?;
        let chooser = Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.FileChooser",
        )?;
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token.as_str()));
        options.insert("modal", Value::from(true));
        options.insert("directory", Value::from(directory));
        let _handle: OwnedObjectPath = chooser.call("OpenFile", &("", "Lumenwall", options))?;
        let Some(msg) = responses.next() else {
            return Ok(None);
        };
        let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize()?;
        if code != 0 {
            return Ok(None);
        }
        let Some(uris) = results.get("uris") else {
            return Ok(None);
        };
        let first = match &**uris {
            Value::Array(a) => a.iter().find_map(|v| match v {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            }),
            _ => None,
        };
        Ok(first.and_then(|uri| file_uri_to_path(&uri)))
    }

    pub fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
        let rest = uri.strip_prefix("file://")?;
        let bytes = rest.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%'
                && i + 2 < bytes.len()
                && let Ok(b) =
                    u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16)
            {
                out.push(b);
                i += 3;
                continue;
            }
            out.push(bytes[i]);
            i += 1;
        }
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn decodes_file_uris() {
            assert_eq!(
                super::file_uri_to_path("file:///home/me/Mes%20fonds/%C3%A9t%C3%A9").unwrap(),
                std::path::PathBuf::from("/home/me/Mes fonds/été")
            );
            assert!(super::file_uri_to_path("https://x").is_none());
        }
    }
}
