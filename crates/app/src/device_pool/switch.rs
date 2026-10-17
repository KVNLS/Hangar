//! Switching a workspace to its other platform: one platform is attached at a time, and the
//! other one's assignment is parked for switching back.

use std::time::Instant;

use anyhow::anyhow;
use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_usecases::device_pool::Switched;

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
    /// device is shut down but kept). The workspace leaves the device it uses: the next in line
    /// gets it. Its parked assignment of `platform` is taken back, else it gets the default
    /// base. Refused while a call or run of the workspace uses a device, and for a platform its
    /// repository does not build for. Resolves to the platform it left, `None` when `platform`
    /// was attached already. Reserve a device afterwards ([`Self::reserve`]).
    pub fn switch_platform(
        &mut self,
        ws: WorkspaceId,
        platform: Platform,
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
            if assignment.await?.platform == platform {
                return Ok(None);
            }
            let (parked, bases) = this.read_with(cx, |pool, _| {
                (
                    pool.machine
                        .parked(ws)
                        .filter(|a| a.platform == platform)
                        .cloned(),
                    pool.machine.bases().clone(),
                )
            })?;
            let target = cx
                .background_executor()
                .spawn(async move {
                    check(platform, &allowed(backend::stack_platforms(&root)))?;
                    match parked {
                        Some(a) => Ok(a),
                        None => default_base(&*store, platform, &bases)
                            .map(|base| Assignment::new(platform, base)),
                    }
                })
                .await?;
            this.update(cx, |pool, cx| {
                let from = pool.machine.assignment(ws).map(|a| a.platform);
                let switched = pool
                    .machine
                    .switch_platform(ws, target, Instant::now())
                    .map_err(anyhow::Error::msg)?;
                let from = match switched {
                    Switched::Same => None,
                    Switched::Done { shutdown, replaced } => {
                        if let Some(job) = shutdown {
                            let shutdown = pool.run_shutdown(job, cx);
                            cx.spawn(async move |_, _| {
                                if let Err(err) = shutdown.await {
                                    tracing::warn!(target: "device_pool", "shutdown of the parked device failed: {err:#}");
                                }
                            })
                            .detach();
                        }
                        if let Some(replaced) = replaced {
                            pool.drop_replaced(replaced, cx);
                        }
                        from
                    }
                };
                pool.run(cx);
                cx.notify();
                Ok(from)
            })?
        })
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
    use super::{Platform, allowed, check};

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
