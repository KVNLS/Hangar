//! The app's asset source: gpui-kit's default component icons plus the extra Lucide icons Hangar
//! uses (device controls and the design system's chrome) that the default bundle lacks, plus the
//! Hangar brand marks.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        Activity,
        ArrowLeft,
        ArrowRightLeft,
        AtSign,
        Brain,
        Camera,
        Circle,
        CircleDot,
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
        Move,
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
        Trash,
        Volume1,
        Volume2,
        Wrench,
        X,
    ]
);

/// The logo (8e) on its full-bleed tile, for in-app use through `img()`.
pub const MARK_PATH: &str = "brand/hangar-mark.svg";
/// The logo on the macOS icon grid (1024 canvas, transparent margin), for the Dock.
pub const APP_ICON_PATH: &str = "brand/hangar-app-icon.svg";
pub const APP_ICON_SVG: &[u8] = include_bytes!("../assets/brand/hangar-app-icon.svg");

const BRAND: [(&str, &[u8]); 2] = [
    (MARK_PATH, include_bytes!("../assets/brand/hangar-mark.svg")),
    (APP_ICON_PATH, APP_ICON_SVG),
];

#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = BRAND.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.extend(
            BRAND
                .iter()
                .filter(|(p, _)| p.starts_with(path))
                .map(|(p, _)| SharedString::from(*p)),
        );
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

    #[test]
    fn brand_marks_load_and_rasterize() {
        let renderer = gpui_kit::SvgRenderer::new(std::sync::Arc::new(Assets));
        for path in [MARK_PATH, APP_ICON_PATH] {
            let bytes = Assets.load(path).unwrap().expect(path);
            let image = renderer.render_single_frame(&bytes, 1.0).unwrap();
            let pixels = image.as_bytes(0).unwrap();
            assert!(
                pixels.as_chunks::<4>().0.iter().any(|p| p[3] == 255),
                "{path}"
            );
        }
        assert!(Assets.list("brand/").unwrap().len() >= 2);
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
