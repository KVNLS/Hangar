//! Switching a workspace to its other platform: one platform is attached at a time, and the
//! other one's assignment is parked for switching back.

use std::time::Instant;

use anyhow::anyhow;
use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_usecases::device_pool::{SwitchBy, Switched};

use crate::device_panel::Platform;

use super::policy::Assignment;
use super::{DevicePool, backend, default_base};

impl DevicePool {
    /// The platforms the workspace may use: those its apps build for, or every platform when
    /// none is detected. Read off the UI thread.
    pub fn platforms(&self, ws: WorkspaceId, cx: &mut Context<Self>) -> Task<Vec<Platform>> {
        let Some(root) = self.machine.workspace_root(ws) else {
            return Task::ready(Platform::ALL.to_vec());
        };
        cx.background_executor()
            .spawn(async move { allowed(backend::stack_platforms(&root)) })
    }

    /// Attaches `platform` to the workspace in place of the other one, which is parked (its
    /// device is shut down once nothing uses it, but kept). The workspace leaves the device it
    /// uses: the next in line gets it. It gets `base` when given (keeping its own device of that
    /// base), else its assignment of `platform`, else the default base. Decided when applied,
    /// so that requests made together apply in turn. Refused while a call or run of the
    /// workspace uses a device, and for a platform its repository does not build for. Resolves
    /// to the platform it left, `None` when `platform` was attached already (a different
    /// `base` is then assigned). Reserve a device afterwards ([`Self::reserve`]).
    pub fn switch_platform(
        &mut self,
        ws: WorkspaceId,
        platform: Platform,
        base: Option<String>,
        by: SwitchBy,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Option<Platform>>> {
        let Some(store) = self.store.clone() else {
            return Task::ready(Err(anyhow!("the database is not open")));
        };
        let Some(root) = self.machine.workspace_root(ws) else {
            return Task::ready(Err(anyhow!("unknown workspace")));
        };
        let assignment = self.resolve_assignment(ws, cx);
        cx.spawn(async move |this, cx| {
            assignment.await?;
            let (needs_default, bases) = this.read_with(cx, |pool, _| {
                (
                    base.is_none() && pool.machine.assignment_of(ws, platform).is_none(),
                    pool.machine.bases().clone(),
                )
            })?;
            let default = cx
                .background_executor()
                .spawn(async move {
                    check(platform, &allowed(backend::stack_platforms(&root)))?;
                    if needs_default {
                        default_base(&*store, platform, &bases).map(Some)
                    } else {
                        Ok(None)
                    }
                })
                .await?;
            this.update(cx, |pool, cx| {
                let applied = pool.apply_switch(ws, platform, base, default, by, cx);
                pool.run(cx);
                cx.notify();
                applied
            })?
        })
    }

    fn apply_switch(
        &mut self,
        ws: WorkspaceId,
        platform: Platform,
        base: Option<String>,
        default: Option<String>,
        by: SwitchBy,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Option<Platform>> {
        let now = Instant::now();
        let active = self.machine.assignment(ws).cloned();
        let chosen = base.is_some();
        let target = switch_target(
            platform,
            base,
            self.machine.assignment_of(ws, platform),
            default,
        )?;
        let new_base = target.base.clone();
        match self
            .machine
            .switch_platform(ws, target, by, now)
            .map_err(anyhow::Error::msg)?
        {
            Switched::Same => {
                if chosen && active.is_some_and(|a| a.base != new_base) {
                    let replaced = self
                        .machine
                        .assign(ws, platform, new_base, now)
                        .map_err(anyhow::Error::msg)?;
                    if let Some(replaced) = replaced {
                        self.drop_replaced(replaced, cx);
                    }
                }
                Ok(None)
            }
            Switched::Done { shutdown, replaced } => {
                if let Some(job) = shutdown {
                    let shutdown = self.run_shutdown(job, cx);
                    cx.spawn(async move |_, _| {
                        if let Err(err) = shutdown.await {
                            tracing::warn!(target: "device_pool", "shutdown of the parked device failed: {err:#}");
                        }
                    })
                    .detach();
                }
                if let Some(replaced) = replaced {
                    self.drop_replaced(replaced, cx);
                }
                Ok(active.map(|a| a.platform))
            }
        }
    }
}

/// The assignment a switch to `platform` attaches: the chosen base (with the workspace's own
/// device when `known` is of that base), else `known`, the workspace's assignment of `platform`
/// (active or parked), else the default base.
fn switch_target(
    platform: Platform,
    chosen: Option<String>,
    known: Option<&Assignment>,
    default: Option<String>,
) -> anyhow::Result<Assignment> {
    let known = known.filter(|a| a.platform == platform);
    match chosen {
        Some(base) => Ok(known
            .filter(|a| a.base == base)
            .cloned()
            .unwrap_or_else(|| Assignment::new(platform, base))),
        None => known
            .cloned()
            .or_else(|| default.map(|base| Assignment::new(platform, base)))
            .ok_or_else(|| {
                anyhow!(
                    "the workspace's {} device changed meanwhile; try again",
                    platform.label()
                )
            }),
    }
}

/// What a workspace may use given the platforms detected in its repository: all of them when
/// nothing was detected.
fn allowed(detected: Vec<Platform>) -> Vec<Platform> {
    if detected.is_empty() {
        Platform::ALL.to_vec()
    } else {
        detected
    }
}

fn check(platform: Platform, allowed: &[Platform]) -> anyhow::Result<()> {
    if allowed.contains(&platform) {
        Ok(())
    } else {
        Err(anyhow!(
            "this repository does not build for {}",
            platform.label()
        ))
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: the glob import of gpui would shadow `#[test]`.
    use super::{Assignment, Platform, allowed, check, switch_target};

    fn with_own(platform: Platform, base: &str, own: &str) -> Assignment {
        let mut a = Assignment::new(platform, base);
        a.own = Some(own.into());
        a
    }

    #[test]
    fn a_switch_attaches_the_chosen_base_else_the_known_assignment_else_the_default() {
        use Platform::*;
        let parked = with_own(Ios, "B", "CLONE");
        let target = |chosen: Option<&str>, known: Option<&Assignment>, default: Option<&str>| {
            switch_target(Ios, chosen.map(Into::into), known, default.map(Into::into))
                .map_err(|e| e.to_string())
        };
        assert_eq!(target(None, Some(&parked), Some("D")), Ok(parked.clone()));
        assert_eq!(target(None, None, Some("D")), Ok(Assignment::new(Ios, "D")));
        assert_eq!(
            target(Some("B"), Some(&parked), None),
            Ok(parked.clone()),
            "the chosen base keeps its simulator"
        );
        assert_eq!(
            target(Some("C"), Some(&parked), None),
            Ok(Assignment::new(Ios, "C"))
        );
        let android = Assignment::new(Android, "P");
        assert_eq!(
            target(None, Some(&android), Some("D")),
            Ok(Assignment::new(Ios, "D")),
            "another platform's assignment is not reused"
        );
        assert_eq!(
            target(None, None, None),
            Err("the workspace's iOS device changed meanwhile; try again".into())
        );
    }

    #[test]
    fn a_repository_that_builds_for_nothing_detected_allows_every_platform() {
        use Platform::*;
        assert_eq!(allowed(Vec::new()), [Ios, Android]);
        assert_eq!(allowed(vec![Android]), [Android]);
        assert!(check(Android, &[Ios, Android]).is_ok());
        assert_eq!(
            check(Android, &[Ios]).unwrap_err().to_string(),
            "this repository does not build for Android"
        );
    }
}
