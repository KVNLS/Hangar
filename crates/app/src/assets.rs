//! The app's asset source: gpui-kit's default component icons plus the few extra Lucide icons
//! Hangar uses (the default bundle has no house, lock, power, volume or rotate-left icons).

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        ArrowLeft, House, LayoutGrid, Lock, Power, RotateCcw, RotateCw, Volume1, Volume2
    ]
);

#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::assets::IconName;

    #[test]
    fn serves_extra_and_default_icons() {
        for icon in [IconName::House, IconName::RotateCcw, IconName::Volume2] {
            let path = icon.path();
            assert!(Assets.load(&path).unwrap().is_some(), "{path}");
        }
        assert!(Assets.load("icons/rotate-cw.svg").unwrap().is_some());
        assert!(!matches!(Assets.load("icons/no-such-icon.svg"), Ok(Some(_))));
    }
}
