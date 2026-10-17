//! Simulator copies the pool made for workspaces that no longer exist. Removing a project used
//! to drop its archived workspaces without their device assignments, leaving their copies in
//! `simctl`, hidden from Settings › Devices. Only this database's own assignments (active and
//! parked) count, so copies made by another Hangar (another `HANGAR_DB`) are never touched.

use std::collections::HashSet;

use hangar_core::WorkspaceId;
use hangar_core::ports::Store;

use super::policy::Assignment;
use crate::device_panel::Platform;
use crate::setting_keys;

/// Keys of the active and the parked assignments ([`setting_keys::device_assignment`],
/// [`setting_keys::device_parked`]).
const PREFIXES: [&str; 2] = ["device:", "device_parked:"];

/// A copy left behind: the setting that names it and the simulator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Orphan {
    pub key: String,
    pub udid: String,
}

/// Assignments of workspaces not in `existing` that own an iOS copy still in `on_mac`.
/// Assignments whose copy is already gone are removed on the way. Blocks.
pub fn find(
    store: &dyn Store,
    existing: impl IntoIterator<Item = WorkspaceId>,
    on_mac: &HashSet<String>,
) -> anyhow::Result<Vec<Orphan>> {
    let live: HashSet<String> = existing
        .into_iter()
        .flat_map(|ws| {
            [
                setting_keys::device_assignment(ws).key().to_owned(),
                setting_keys::device_parked(ws).key().to_owned(),
            ]
        })
        .collect();
    let mut orphans = Vec::new();
    for prefix in PREFIXES {
        for key in store.setting_keys(prefix)? {
            if live.contains(&key) {
                continue;
            }
            let assignment: Option<Assignment> = store
                .read_setting(&key)?
                .and_then(|json| serde_json::from_str(&json).ok());
            match assignment {
                Some(Assignment {
                    platform: Platform::Ios,
                    own: Some(udid),
                    ..
                }) if on_mac.contains(&udid) => orphans.push(Orphan { key, udid }),
                _ => store.remove_setting(&key)?,
            }
        }
    }
    Ok(orphans)
}

/// Deletes the copies and their assignments. Blocks; returns how many were deleted.
pub fn delete(store: &dyn Store, orphans: &[Orphan]) -> anyhow::Result<usize> {
    let mut deleted = 0;
    for orphan in orphans {
        crate::adapters::device_catalog(Platform::Ios).delete_clone(&orphan.udid)?;
        store.remove_setting(&orphan.key)?;
        deleted += 1;
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hangar_usecases::settings::SettingsExt as _;

    fn assignment(platform: Platform, own: Option<&str>) -> Assignment {
        Assignment {
            platform,
            base: "BASE".into(),
            own: own.map(str::to_owned),
            serial: None,
        }
    }

    #[test]
    fn only_copies_of_deleted_workspaces_still_on_the_mac() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::adapters::open_store(&dir.path().join("hangar.db")).unwrap();
        let (kept, gone, stale, android) = (
            WorkspaceId::new(),
            WorkspaceId::new(),
            WorkspaceId::new(),
            WorkspaceId::new(),
        );
        let save = |ws, a: &Assignment| {
            store
                .set_setting(&setting_keys::device_assignment(ws), a)
                .unwrap()
        };
        save(kept, &assignment(Platform::Ios, Some("KEPT")));
        save(gone, &assignment(Platform::Ios, Some("GONE")));
        save(stale, &assignment(Platform::Ios, Some("ALREADY-DELETED")));
        save(android, &assignment(Platform::Android, Some("avd")));
        let on_mac: HashSet<String> = ["KEPT", "GONE", "SOMEONE-ELSES"].map(str::to_owned).into();

        let orphans = find(store.as_ref(), [kept], &on_mac).unwrap();
        assert_eq!(
            orphans,
            [Orphan {
                key: setting_keys::device_assignment(gone).key().to_owned(),
                udid: "GONE".into(),
            }]
        );
        // The stale and Android assignments were cleared; the kept one stays.
        assert_eq!(
            store.setting_keys(PREFIXES[0]).unwrap().len(),
            2,
            "{:?}",
            store.setting_keys(PREFIXES[0])
        );
    }

    #[test]
    fn parked_copies_count_too() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::adapters::open_store(&dir.path().join("hangar.db")).unwrap();
        let (kept, gone, stale) = (WorkspaceId::new(), WorkspaceId::new(), WorkspaceId::new());
        let park = |ws, a: &Assignment| {
            store
                .set_setting(&setting_keys::device_parked(ws), a)
                .unwrap()
        };
        store
            .set_setting(
                &setting_keys::device_assignment(kept),
                &assignment(Platform::Android, None),
            )
            .unwrap();
        park(kept, &assignment(Platform::Ios, Some("KEPT")));
        park(gone, &assignment(Platform::Ios, Some("GONE")));
        park(stale, &assignment(Platform::Ios, Some("ALREADY-DELETED")));
        let on_mac: HashSet<String> = ["KEPT", "GONE"].map(str::to_owned).into();

        let orphans = find(store.as_ref(), [kept], &on_mac).unwrap();
        assert_eq!(
            orphans,
            [Orphan {
                key: setting_keys::device_parked(gone).key().to_owned(),
                udid: "GONE".into(),
            }]
        );
        let parked = store.setting_keys(PREFIXES[1]).unwrap();
        assert_eq!(parked.len(), 2, "the stale one is cleared: {parked:?}");
        assert_eq!(store.setting_keys(PREFIXES[0]).unwrap().len(), 1);
    }
}
