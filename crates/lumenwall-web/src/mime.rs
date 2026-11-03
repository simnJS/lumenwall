//! MIME types for files served from a wallpaper folder.

/// Type sent for unknown extensions.
pub const DEFAULT_MIME: &str = "application/octet-stream";

/// Sorted by extension (lower case) for binary search.
const TABLE: &[(&str, &str)] = &[
    ("aac", "audio/aac"),
    ("apng", "image/apng"),
    ("atlas", "text/plain"),
    ("avif", "image/avif"),
    ("bin", "application/octet-stream"),
    ("bmp", "image/bmp"),
    ("cjs", "text/javascript"),
    ("css", "text/css"),
    ("csv", "text/csv"),
    ("cur", "image/x-icon"),
    ("eot", "application/vnd.ms-fontobject"),
    ("flac", "audio/flac"),
    ("frag", "text/plain"),
    ("gif", "image/gif"),
    ("glb", "model/gltf-binary"),
    ("glsl", "text/plain"),
    ("gltf", "model/gltf+json"),
    ("htm", "text/html"),
    ("html", "text/html"),
    ("ico", "image/x-icon"),
    ("jfif", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("jpg", "image/jpeg"),
    ("js", "text/javascript"),
    ("json", "application/json"),
    ("jsonld", "application/ld+json"),
    ("ktx2", "image/ktx2"),
    ("m4a", "audio/mp4"),
    ("m4v", "video/mp4"),
    ("map", "application/json"),
    ("md", "text/markdown"),
    ("mid", "audio/midi"),
    ("midi", "audio/midi"),
    ("mjs", "text/javascript"),
    ("mov", "video/quicktime"),
    ("mp3", "audio/mpeg"),
    ("mp4", "video/mp4"),
    ("mpeg", "video/mpeg"),
    ("oga", "audio/ogg"),
    ("ogg", "audio/ogg"),
    ("ogv", "video/ogg"),
    ("opus", "audio/ogg"),
    ("otf", "font/otf"),
    ("pdf", "application/pdf"),
    ("png", "image/png"),
    ("pnga", "image/png"),
    ("svg", "image/svg+xml"),
    ("svgz", "image/svg+xml"),
    ("tga", "image/x-tga"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("ts", "video/mp2t"),
    ("ttf", "font/ttf"),
    ("txt", "text/plain"),
    ("vert", "text/plain"),
    ("vtt", "text/vtt"),
    ("wasm", "application/wasm"),
    ("wav", "audio/wav"),
    ("weba", "audio/webm"),
    ("webm", "video/webm"),
    ("webmanifest", "application/manifest+json"),
    ("webp", "image/webp"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
    ("xhtml", "application/xhtml+xml"),
    ("xml", "application/xml"),
    ("zip", "application/zip"),
];

/// MIME type from a file name's extension (ASCII case-insensitive), or [`DEFAULT_MIME`].
pub fn mime_for_path(path: &str) -> &'static str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let Some((_, ext)) = name.rsplit_once('.') else {
        return DEFAULT_MIME;
    };
    if ext.len() > 16 {
        return DEFAULT_MIME;
    }
    let mut buf = [0u8; 16];
    let lower = &mut buf[..ext.len()];
    lower.copy_from_slice(ext.as_bytes());
    lower.make_ascii_lowercase();
    let Ok(lower) = std::str::from_utf8(lower) else {
        return DEFAULT_MIME;
    };
    TABLE
        .binary_search_by(|(e, _)| (*e).cmp(lower))
        .map_or(DEFAULT_MIME, |i| TABLE[i].1)
}

/// Whether text responses should declare UTF-8.
pub fn is_text(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json" | "application/xml" | "image/svg+xml"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_lower_case() {
        for pair in TABLE.windows(2) {
            assert!(pair[0].0 < pair[1].0, "{} !< {}", pair[0].0, pair[1].0);
        }
        assert!(
            TABLE
                .iter()
                .all(|(e, _)| e.bytes().all(|b| !b.is_ascii_uppercase()))
        );
    }

    #[test]
    fn lookups() {
        assert_eq!(mime_for_path("index.html"), "text/html");
        assert_eq!(mime_for_path("js/app.min.JS"), "text/javascript");
        assert_eq!(mime_for_path("a/b/style.css"), "text/css");
        assert_eq!(mime_for_path("img/x.SVG"), "image/svg+xml");
        assert_eq!(mime_for_path("m.wasm"), "application/wasm");
        assert_eq!(mime_for_path("v.webm"), "video/webm");
        assert_eq!(mime_for_path("font.woff2"), "font/woff2");
        assert_eq!(mime_for_path("noext"), DEFAULT_MIME);
        assert_eq!(mime_for_path("dir.d/noext"), DEFAULT_MIME);
        assert_eq!(mime_for_path("x.unknownextension"), DEFAULT_MIME);
        assert_eq!(mime_for_path("x.é"), DEFAULT_MIME);
        assert!(is_text("text/css"));
        assert!(!is_text("image/png"));
    }
}
