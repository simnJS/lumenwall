//! The playlists of the current plan: one [`PlaylistRunner`] per target (window, screen or
//! span) that names a playlist (spec 07 §17.4). The daemon owns them under its change lock,
//! keeps the plan showing their current items, and sleeps until the earliest deadline on a
//! thread of its own (`lumenwall-playlists`): no polling, no wake-up while every playing
//! playlist waits for a pause to end (docs/PERFORMANCE.md).

use std::collections::BTreeMap;
use std::time::Instant;

use lumenwall_ipc::PlaylistPosition;

use crate::plan::{ExecutionPlan, TargetKey};
use crate::playlist::{PlaylistRunner, SplitMix64};
use crate::services::PlaylistCatalog;

pub(super) struct Running {
    pub name: String,
    pub runner: PlaylistRunner<SplitMix64>,
}

/// Runners by target.
#[derive(Default)]
pub(super) struct Playlists {
    running: BTreeMap<TargetKey, Running>,
}

impl Playlists {
    /// The runner whose current item `target` shows: its own, or, when a layout change split
    /// the window target into screens, the window's (same playlist).
    fn runner_for(&self, key: &TargetKey, name: &str) -> Option<&Running> {
        self.running
            .get(key)
            .or_else(|| self.running.get(&TargetKey::Window))
            .filter(|r| r.name == name)
    }

    /// Puts each playing playlist's current item into `plan` (a plan built from the
    /// configuration names the first item), so that rebuilding the plan for another reason
    /// does not go back to the start.
    pub fn overlay(&self, plan: &mut ExecutionPlan) {
        for target in &mut plan.targets {
            if let Some(name) = &target.playlist
                && let Some(r) = self.runner_for(&target.key, name)
            {
                target.wallpaper.path = r.runner.current().to_path_buf();
            }
        }
    }

    /// Follows `plan`: keeps the runners of targets that play the same playlist, starts one
    /// at the wallpaper shown for each new playlist target, drops the others.
    pub fn sync(&mut self, plan: &ExecutionPlan, catalog: &dyn PlaylistCatalog, now: Instant) {
        self.running.retain(|key, r| {
            let kept = plan
                .target(key)
                .is_some_and(|t| t.playlist.as_deref() == Some(r.name.as_str()));
            if !kept {
                tracing::info!("playlist '{}' stopped on {key}", r.name);
            }
            kept
        });
        for target in &plan.targets {
            let Some(name) = &target.playlist else {
                continue;
            };
            if self.running.contains_key(&target.key) {
                continue;
            }
            match catalog.playlist(name) {
                Ok(playlist) => {
                    let runner = PlaylistRunner::new(
                        playlist,
                        Some(&target.wallpaper.path),
                        now,
                        SplitMix64::from_entropy(),
                    );
                    let s = &runner.playlist().settings;
                    tracing::info!(
                        "playlist '{name}' on {}: {} items, {}, {}{}",
                        target.key,
                        runner.playlist().items.len(),
                        if s.timer {
                            format!("every {} min", s.delay.as_secs() / 60)
                        } else {
                            "changes only on request".to_owned()
                        },
                        if s.random { "random" } else { "in order" },
                        if s.transition {
                            " (shown without transition)"
                        } else {
                            ""
                        },
                    );
                    self.running.insert(
                        target.key.clone(),
                        Running {
                            name: name.clone(),
                            runner,
                        },
                    );
                }
                Err(e) => tracing::warn!(
                    "playlist '{name}' on {}: {e}; only its first item is shown",
                    target.key
                ),
            }
        }
    }

    /// Tells each runner whether its target is paused at `now`.
    pub fn set_paused(&mut self, paused: impl Fn(&TargetKey) -> bool, now: Instant) {
        for (key, r) in &mut self.running {
            r.runner.set_paused(paused(key), now);
        }
    }

    /// The earliest deadline; `None`: nothing to wake up for.
    pub fn deadline(&self) -> Option<Instant> {
        self.running
            .values()
            .filter_map(|r| r.runner.deadline())
            .min()
    }

    /// Targets whose playlist should change at `now`.
    pub fn due(&self, now: Instant) -> Vec<TargetKey> {
        self.running
            .iter()
            .filter(|(_, r)| r.runner.deadline().is_some_and(|d| d <= now))
            .map(|(k, _)| k.clone())
            .collect()
    }

    pub fn keys(&self) -> Vec<TargetKey> {
        self.running.keys().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
    }

    pub fn get_mut(&mut self, key: &TargetKey) -> Option<&mut Running> {
        self.running.get_mut(key)
    }

    /// Where each playlist target of `plan` is, by target key as the engine names it. A
    /// playlist that could not be started has no item and no count.
    pub fn positions(&self, plan: &ExecutionPlan) -> BTreeMap<String, PlaylistPosition> {
        plan.targets
            .iter()
            .filter_map(|t| {
                let name = t.playlist.as_ref()?;
                let position = match self.running.get(&t.key) {
                    Some(r) => PlaylistPosition {
                        name: name.clone(),
                        item: r.runner.current_item().map(|i| i as u32),
                        count: r.runner.playlist().items.len() as u32,
                    },
                    None => PlaylistPosition {
                        name: name.clone(),
                        item: None,
                        count: 0,
                    },
                };
                Some((t.key.to_string(), position))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
