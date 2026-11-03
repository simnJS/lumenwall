//! Pure fullscreen / occlusion decisions shared by the backends (spec 07 §9).
//!
//! Backends gather OS state from events and call these functions; nothing here talks to a display server,
//! so the rules are unit-tested on any host.

use crate::geometry::Rect;
use crate::types::{FullscreenPolicy, PauseReasons, PauseScope};

/// ASCII case-insensitive substring test.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.len() > h.len() {
        return false;
    }
    h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

/// True when one of the non-empty `patterns` is a substring of `app_id` (ASCII case-insensitive).
/// A window without app_id is never ignored (§9.3).
pub fn app_id_ignored(app_id: Option<&str>, patterns: &[String]) -> bool {
    let Some(app_id) = app_id else { return false };
    patterns
        .iter()
        .filter(|p| !p.is_empty())
        .any(|p| contains_ignore_ascii_case(app_id, p))
}

/// Whether a toplevel counts for the fullscreen pause (§9.3).
pub fn toplevel_relevant(
    fullscreen: bool,
    activated: bool,
    app_id: Option<&str>,
    policy: &FullscreenPolicy,
) -> bool {
    policy.enabled
        && fullscreen
        && (!policy.only_active || activated)
        && !app_id_ignored(app_id, &policy.ignore_app_ids)
}

/// Double-buffered toplevel flags of wlr-foreign-toplevel: `state` events are pending until `done`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToplevelFlags {
    pending_fullscreen: bool,
    pending_activated: bool,
    /// Relevance computed at the last `done`.
    pub relevant: bool,
}

impl ToplevelFlags {
    /// `state` event (values of `zwlr_foreign_toplevel_handle_v1.state`: 2 = activated, 3 = fullscreen).
    pub fn set_pending_state(&mut self, states: impl IntoIterator<Item = u32>) {
        self.pending_fullscreen = false;
        self.pending_activated = false;
        for s in states {
            match s {
                2 => self.pending_activated = true,
                3 => self.pending_fullscreen = true,
                _ => {}
            }
        }
    }

    /// `done` event: commits the pending flags. Returns `true` when the relevance changed.
    pub fn commit(&mut self, app_id: Option<&str>, policy: &FullscreenPolicy) -> bool {
        let relevant = toplevel_relevant(
            self.pending_fullscreen,
            self.pending_activated,
            app_id,
            policy,
        );
        let changed = relevant != self.relevant;
        self.relevant = relevant;
        changed
    }
}

/// Parses the `wl_array` of a wlr-foreign-toplevel `state` event (native-endian `u32`s).
pub fn parse_state_array(bytes: &[u8]) -> impl Iterator<Item = u32> + '_ {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_ne_bytes(*c))
}

/// Pause reason that a window with geometry `window` imposes on an output with geometry `output`.
///
/// * exact match (the reference rule, §9.2) → `FULLSCREEN`;
/// * strictly larger and covering → `OCCLUDED` when `covered_pause`.
pub fn x11_window_reason(window: &Rect, output: &Rect, covered_pause: bool) -> PauseReasons {
    if window.is_empty() || output.is_empty() {
        PauseReasons::NONE
    } else if window == output {
        PauseReasons::FULLSCREEN
    } else if covered_pause && window.contains_rect(output) {
        PauseReasons::OCCLUDED
    } else {
        PauseReasons::NONE
    }
}

/// Whether a root child may count as covering an output, decided once per window with a few requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Not checked yet (checked lazily, only when its geometry matches an output).
    Unknown,
    /// A normal client or frame window.
    Counts,
    /// Never counts: input-only, desktop-type, or not listed by `QueryTree` (e.g. the composite overlay).
    Ignored,
}

/// One direct child of the X11 root window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RootChild {
    /// Window id.
    pub id: u32,
    /// Geometry relative to the root (border excluded, as `GetGeometry` reports it).
    pub rect: Rect,
    /// Map state ("viewable" for a root child).
    pub mapped: bool,
    /// `_NET_WM_STATE_HIDDEN` (minimised or on another workspace). Some window managers, e.g. Mutter for
    /// XWayland clients, keep such windows mapped; they hide nothing.
    pub hidden: bool,
    /// `_NET_WM_STATE_MAXIMIZED_VERT` + `_HORZ`: hides the output but is not a fullscreen application.
    pub maximized: bool,
    /// Lazy classification.
    pub verdict: Verdict,
}

/// Cache of the root's children, maintained from `SubstructureNotify` events instead of the reference's
/// per-frame `QueryTree` + geometry requests (§9.2).
#[derive(Clone, Debug, Default)]
pub struct RootChildren {
    children: Vec<RootChild>,
}

impl RootChildren {
    /// Replaces the cache (startup snapshot).
    pub fn reset(&mut self, children: impl IntoIterator<Item = RootChild>) {
        self.children.clear();
        self.children.extend(children);
    }

    fn find(&mut self, id: u32) -> Option<&mut RootChild> {
        self.children.iter_mut().find(|c| c.id == id)
    }

    /// `CreateNotify` or `ReparentNotify` to the root. Returns `true` when the cache changed.
    pub fn created(&mut self, id: u32, rect: Rect) -> bool {
        match self.find(id) {
            Some(c) => c.rect = rect,
            None => self.children.push(RootChild {
                id,
                rect,
                mapped: false,
                hidden: false,
                maximized: false,
                verdict: Verdict::Unknown,
            }),
        }
        true
    }

    /// `DestroyNotify` or reparenting away from the root. Returns `true` when the window was mapped.
    pub fn destroyed(&mut self, id: u32) -> bool {
        match self.children.iter().position(|c| c.id == id) {
            Some(i) => self.children.swap_remove(i).mapped,
            None => false,
        }
    }

    /// `MapNotify` / `UnmapNotify`. Returns `true` when the state changed.
    pub fn set_mapped(&mut self, id: u32, mapped: bool) -> bool {
        match self.find(id) {
            Some(c) if c.mapped != mapped => {
                c.mapped = mapped;
                true
            }
            _ => false,
        }
    }

    /// `ConfigureNotify`. Returns `true` when a mapped window changed geometry.
    pub fn configured(&mut self, id: u32, rect: Rect) -> bool {
        match self.find(id) {
            Some(c) if c.rect != rect => {
                c.rect = rect;
                c.mapped
            }
            _ => false,
        }
    }

    /// Stores the classification of a window.
    pub fn set_verdict(&mut self, id: u32, verdict: Verdict) {
        if let Some(c) = self.find(id) {
            c.verdict = verdict;
        }
    }

    /// Classification of a window, if it is a cached root child.
    pub fn verdict(&self, id: u32) -> Option<Verdict> {
        self.children.iter().find(|c| c.id == id).map(|c| c.verdict)
    }

    /// Latest `_NET_WM_STATE` of a window. Returns `true` when it changed for a mapped window.
    pub fn set_wm_state(&mut self, id: u32, hidden: bool, maximized: bool) -> bool {
        match self.find(id) {
            Some(c) if (c.hidden, c.maximized) != (hidden, maximized) => {
                c.hidden = hidden;
                c.maximized = maximized;
                c.mapped
            }
            _ => false,
        }
    }

    /// Mapped, counted windows whose geometry would pause one of `outputs` (their `_NET_WM_STATE` matters).
    pub fn counted_candidates<'a>(
        &'a self,
        outputs: &'a [Rect],
        exclude: &'a [u32],
        covered_pause: bool,
    ) -> impl Iterator<Item = u32> + 'a {
        self.children
            .iter()
            .filter(move |c| c.mapped && c.verdict == Verdict::Counts && !exclude.contains(&c.id))
            .filter(move |c| {
                outputs
                    .iter()
                    .any(|o| !x11_window_reason(&c.rect, o, covered_pause).is_empty())
            })
            .map(|c| c.id)
    }

    /// Mapped, unclassified windows whose geometry would pause one of `outputs`: classify these first.
    pub fn unclassified_candidates<'a>(
        &'a self,
        outputs: &'a [Rect],
        exclude: &'a [u32],
        covered_pause: bool,
    ) -> impl Iterator<Item = u32> + 'a {
        self.children
            .iter()
            .filter(move |c| c.mapped && c.verdict == Verdict::Unknown && !exclude.contains(&c.id))
            .filter(move |c| {
                outputs
                    .iter()
                    .any(|o| !x11_window_reason(&c.rect, o, covered_pause).is_empty())
            })
            .map(|c| c.id)
    }

    /// Adds to `out[i]` the reason imposed on `outputs[i]` by classified, mapped windows not in `exclude`.
    pub fn reasons(
        &self,
        outputs: &[Rect],
        exclude: &[u32],
        covered_pause: bool,
        fullscreen: bool,
        out: &mut [PauseReasons],
    ) {
        for c in self.children.iter().filter(|c| {
            c.mapped && !c.hidden && c.verdict == Verdict::Counts && !exclude.contains(&c.id)
        }) {
            for (o, r) in outputs.iter().zip(out.iter_mut()) {
                let reason = x11_window_reason(&c.rect, o, covered_pause);
                // Deviation: a maximised window that happens to match the output exactly is not a fullscreen
                // application (the reference pauses every output for it); it only hides its own output.
                if reason.contains(PauseReasons::FULLSCREEN) && (!fullscreen || c.maximized) {
                    // Detection disabled: a fullscreen window still hides the wallpaper.
                    if covered_pause {
                        *r |= PauseReasons::OCCLUDED;
                    }
                } else {
                    *r |= reason;
                }
            }
        }
    }

    /// Number of cached windows.
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// True when the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }
}

/// Applies the [`PauseScope`]: with `AllOutputs`, a fullscreen window on any output pauses every output.
pub fn apply_scope(reasons: &mut [PauseReasons], scope: PauseScope) {
    if scope == PauseScope::AllOutputs
        && reasons.iter().any(|r| r.contains(PauseReasons::FULLSCREEN))
    {
        for r in reasons.iter_mut() {
            *r |= PauseReasons::FULLSCREEN;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(only_active: bool, ignore: &[&str]) -> FullscreenPolicy {
        FullscreenPolicy {
            enabled: true,
            only_active,
            ignore_app_ids: ignore.iter().map(|s| s.to_string()).collect(),
            scope: PauseScope::AllOutputs,
        }
    }

    #[test]
    fn app_id_patterns() {
        let pats = vec!["".to_string(), "FireFox".to_string()];
        assert!(app_id_ignored(Some("org.mozilla.firefox"), &pats));
        assert!(!app_id_ignored(Some("mpv"), &pats));
        assert!(!app_id_ignored(None, &pats), "no app_id is never ignored");
        assert!(
            !app_id_ignored(Some("x"), &["".to_string()]),
            "empty patterns are ignored"
        );
        assert!(!app_id_ignored(Some("ab"), &["abc".to_string()]));
    }

    #[test]
    fn relevance_matrix() {
        let p = policy(false, &[]);
        assert!(toplevel_relevant(true, false, Some("mpv"), &p));
        assert!(!toplevel_relevant(false, true, Some("mpv"), &p));
        let active = policy(true, &[]);
        assert!(!toplevel_relevant(true, false, Some("mpv"), &active));
        assert!(toplevel_relevant(true, true, Some("mpv"), &active));
        let ign = policy(false, &["MPV"]);
        assert!(!toplevel_relevant(true, true, Some("io.mpv.Mpv"), &ign));
        assert!(toplevel_relevant(true, true, None, &ign));
        let off = FullscreenPolicy {
            enabled: false,
            ..p
        };
        assert!(!toplevel_relevant(true, true, None, &off));
    }

    #[test]
    fn toplevel_flags_are_double_buffered() {
        let p = policy(false, &[]);
        let mut f = ToplevelFlags::default();
        f.set_pending_state([3]);
        assert!(!f.relevant, "not applied before done");
        assert!(f.commit(Some("game"), &p));
        assert!(f.relevant);
        assert!(!f.commit(Some("game"), &p), "unchanged");
        f.set_pending_state([2]);
        assert!(f.commit(Some("game"), &p));
        assert!(!f.relevant);
    }

    #[test]
    fn state_array_parsing() {
        let mut bytes = Vec::new();
        for v in [2u32, 3] {
            bytes.extend_from_slice(&v.to_ne_bytes());
        }
        bytes.push(0xff); // trailing garbage ignored
        assert_eq!(parse_state_array(&bytes).collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn x11_rules() {
        let out = Rect::new(1920, 0, 2560, 1440);
        assert_eq!(
            x11_window_reason(&out, &out, true),
            PauseReasons::FULLSCREEN
        );
        assert_eq!(
            x11_window_reason(&Rect::new(0, 0, 4480, 1440), &out, true),
            PauseReasons::OCCLUDED
        );
        assert_eq!(
            x11_window_reason(&Rect::new(0, 0, 4480, 1440), &out, false),
            PauseReasons::NONE
        );
        assert_eq!(
            x11_window_reason(&Rect::new(1920, 30, 2560, 1410), &out, true),
            PauseReasons::NONE
        );
    }

    #[test]
    fn root_children_cache() {
        let outputs = [Rect::new(0, 0, 1920, 1080), Rect::new(1920, 0, 2560, 1440)];
        let ours = 100;
        let mut rc = RootChildren::default();
        rc.reset([
            RootChild {
                id: ours,
                rect: outputs[0],
                mapped: true,
                hidden: false,
                maximized: false,
                verdict: Verdict::Unknown,
            },
            RootChild {
                id: 1,
                rect: Rect::new(10, 10, 800, 600),
                mapped: true,
                hidden: false,
                maximized: false,
                verdict: Verdict::Counts,
            },
        ]);
        let mut out = [PauseReasons::NONE; 2];
        assert_eq!(
            rc.unclassified_candidates(&outputs, &[ours], true).count(),
            0,
            "our own window is excluded"
        );

        // A game maps a window exactly on the second output.
        assert!(rc.created(7, outputs[1]));
        assert_eq!(
            rc.unclassified_candidates(&outputs, &[ours], true).count(),
            0,
            "not mapped yet"
        );
        assert!(rc.set_mapped(7, true));
        assert!(!rc.set_mapped(7, true));
        assert_eq!(
            rc.unclassified_candidates(&outputs, &[ours], true)
                .collect::<Vec<_>>(),
            vec![7]
        );
        rc.reasons(&outputs, &[ours], true, true, &mut out);
        assert_eq!(
            out,
            [PauseReasons::NONE; 2],
            "unclassified windows do not count yet"
        );
        rc.set_verdict(7, Verdict::Counts);
        rc.reasons(&outputs, &[ours], true, true, &mut out);
        assert_eq!(out, [PauseReasons::NONE, PauseReasons::FULLSCREEN]);

        // Fullscreen detection disabled: reported as covered instead.
        let mut out = [PauseReasons::NONE; 2];
        rc.reasons(&outputs, &[ours], true, false, &mut out);
        assert_eq!(out, [PauseReasons::NONE, PauseReasons::OCCLUDED]);

        // Leaves fullscreen.
        assert!(rc.configured(7, Rect::new(2000, 50, 800, 600)));
        let mut out = [PauseReasons::NONE; 2];
        rc.reasons(&outputs, &[ours], true, true, &mut out);
        assert_eq!(out, [PauseReasons::NONE; 2]);
        assert!(rc.destroyed(7));
        assert!(!rc.destroyed(7));
        assert_eq!(rc.len(), 2);
    }

    #[test]
    fn hidden_windows_never_count() {
        // Mutter keeps minimised XWayland windows mapped, flagged _NET_WM_STATE_HIDDEN.
        let outputs = [Rect::new(0, 0, 2560, 1440)];
        let mut rc = RootChildren::default();
        rc.created(9, outputs[0]);
        rc.set_mapped(9, true);
        rc.set_verdict(9, Verdict::Counts);
        assert_eq!(rc.verdict(9), Some(Verdict::Counts));
        let mut out = [PauseReasons::NONE];
        rc.reasons(&outputs, &[], true, true, &mut out);
        assert_eq!(out, [PauseReasons::FULLSCREEN]);
        assert_eq!(
            rc.counted_candidates(&outputs, &[], true)
                .collect::<Vec<_>>(),
            vec![9]
        );
        assert!(rc.set_wm_state(9, true, false));
        assert!(!rc.set_wm_state(9, true, false));
        let mut out = [PauseReasons::NONE];
        rc.reasons(&outputs, &[], true, true, &mut out);
        assert_eq!(out, [PauseReasons::NONE]);
        assert_eq!(rc.verdict(1234), None);

        // Maximised to exactly the output: covered, not fullscreen.
        assert!(rc.set_wm_state(9, false, true));
        let mut out = [PauseReasons::NONE];
        rc.reasons(&outputs, &[], true, true, &mut out);
        assert_eq!(out, [PauseReasons::OCCLUDED]);
        let mut out = [PauseReasons::NONE];
        rc.reasons(&outputs, &[], false, true, &mut out);
        assert_eq!(out, [PauseReasons::NONE]);
    }

    #[test]
    fn ignored_windows_never_count() {
        let outputs = [Rect::new(0, 0, 1920, 1080)];
        let mut rc = RootChildren::default();
        rc.created(5, outputs[0]);
        rc.set_mapped(5, true);
        rc.set_verdict(5, Verdict::Ignored);
        let mut out = [PauseReasons::NONE];
        rc.reasons(&outputs, &[], true, true, &mut out);
        assert_eq!(out, [PauseReasons::NONE]);
        assert_eq!(rc.unclassified_candidates(&outputs, &[], true).count(), 0);
    }

    #[test]
    fn scope() {
        let mut r = [
            PauseReasons::NONE,
            PauseReasons::FULLSCREEN,
            PauseReasons::LOCKED,
        ];
        apply_scope(&mut r, PauseScope::PerOutput);
        assert_eq!(r[0], PauseReasons::NONE);
        apply_scope(&mut r, PauseScope::AllOutputs);
        assert!(r.iter().all(|r| r.contains(PauseReasons::FULLSCREEN)));
        assert!(r[2].contains(PauseReasons::LOCKED));
    }
}
