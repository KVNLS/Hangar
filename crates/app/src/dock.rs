//! The Dock tile: a badge with the number of agents waiting on the user, and a bounce when one
//! starts waiting while Hangar is in the background. Both are public AppKit and work for an
//! unbundled `cargo run` binary. System notifications are in `notify`.

/// The Dock badge for `count` waiting agents: empty (no badge) at zero.
pub fn badge_label(count: usize) -> String {
    match count {
        0 => String::new(),
        n => n.to_string(),
    }
}

/// Sets the Dock badge; an empty label removes it. Main thread only.
#[cfg(target_os = "macos")]
pub fn set_badge(label: &str) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;

    let mtm = MainThreadMarker::new().context("the Dock badge must be set on the main thread")?;
    let label = (!label.is_empty()).then(|| NSString::from_str(label));
    NSApplication::sharedApplication(mtm)
        .dockTile()
        .setBadgeLabel(label.as_deref());
    Ok(())
}

/// Bounces the Dock icon once (an informational request). AppKit ignores it while Hangar is the
/// active app. Main thread only.
#[cfg(target_os = "macos")]
pub fn request_attention() -> anyhow::Result<()> {
    use anyhow::Context as _;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSRequestUserAttentionType};

    let mtm = MainThreadMarker::new().context("attention must be requested on the main thread")?;
    NSApplication::sharedApplication(mtm)
        .requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn set_badge(_label: &str) -> anyhow::Result<()> {
    anyhow::bail!("unsupported: the Dock badge exists only on macOS")
}

#[cfg(not(target_os = "macos"))]
pub fn request_attention() -> anyhow::Result<()> {
    anyhow::bail!("unsupported: Dock attention requests exist only on macOS")
}

#[cfg(test)]
mod tests {
    use super::badge_label;

    #[test]
    fn badge_is_empty_at_zero() {
        assert_eq!(badge_label(0), "");
        assert_eq!(badge_label(1), "1");
        assert_eq!(badge_label(12), "12");
    }
}
