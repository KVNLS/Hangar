//! The app's asset source: gpui-kit's default component icons plus the extra Lucide icons Hangar
//! uses (device controls and the design system's chrome) that the default bundle lacks.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        Activity,
        ArrowLeft,
        AtSign,
        Camera,
        Columns2,
        FileDiff,
        GitBranch,
        GitCommitHorizontal,
        GitPullRequest,
        Hammer,
        House,
        Keyboard,
        LayoutGrid,
        ListCollapse,
        Lock,
        MessageSquare,
        Pencil,
        Power,
        RotateCcw,
        RotateCw,
        RotateCwSquare,
        Rows2,
        Smartphone,
        Square,
        SunMoon,
        Terminal,
        Volume1,
        Volume2,
        X,
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
        for icon in [
            IconName::House,
            IconName::RotateCcw,
            IconName::Volume2,
            IconName::GitBranch,
        ] {
            let path = icon.path();
            assert!(Assets.load(&path).unwrap().is_some(), "{path}");
        }
        assert!(Assets.load("icons/rotate-cw.svg").unwrap().is_some());
        assert!(!matches!(
            Assets.load("icons/no-such-icon.svg"),
            Ok(Some(_))
        ));
    }

    /// `IconName` covers the whole Lucide catalog, but an icon missing from the asset source
    /// renders blank. Every `IconName::X` / `Lucide::X` in the app's sources must load.
    #[test]
    fn every_icon_used_in_the_sources_is_served() {
        fn kebab(name: &str) -> String {
            let mut out = String::new();
            let mut prev: Option<char> = None;
            for c in name.chars() {
                let boundary = prev.is_some_and(|p| {
                    (c.is_ascii_uppercase() && (p.is_ascii_lowercase() || p.is_ascii_digit()))
                        || (c.is_ascii_digit() && p.is_ascii_alphabetic())
                });
                if boundary {
                    out.push('-');
                }
                out.push(c.to_ascii_lowercase());
                prev = Some(c);
            }
            out
        }
        fn visit(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    visit(&path, files);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        let mut files = Vec::new();
        visit(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let mut missing = Vec::new();
        for file in files {
            let source = std::fs::read_to_string(&file).unwrap();
            for prefix in ["IconName::", "Lucide::"] {
                for (at, _) in source.match_indices(prefix) {
                    let name: String = source[at + prefix.len()..]
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric())
                        .collect();
                    if !name.starts_with(|c: char| c.is_ascii_uppercase()) {
                        continue;
                    }
                    let path = format!("icons/{}.svg", kebab(&name));
                    if !matches!(Assets.load(&path), Ok(Some(_))) {
                        missing.push(format!("{name} ({})", file.display()));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "add to ExtraIcons: {missing:?}");
    }
}
