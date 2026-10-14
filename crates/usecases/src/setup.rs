//! The setup checklist: probe the machine, then apply the rules of [`hangar_core::setup`].

use hangar_core::ports::ToolProbe;
use hangar_core::setup::{SetupCheck, checklist};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetupReport {
    pub checks: Vec<SetupCheck>,
}

impl SetupReport {
    /// Failing checks Hangar cannot work without.
    pub fn blockers(&self) -> impl Iterator<Item = &SetupCheck> {
        self.checks.iter().filter(|c| c.required && !c.is_ok())
    }

    /// Every failing check, required ones first, otherwise in checklist order.
    pub fn failing(&self) -> Vec<&SetupCheck> {
        let mut failing: Vec<&SetupCheck> = self.checks.iter().filter(|c| !c.is_ok()).collect();
        failing.sort_by_key(|c| !c.required);
        failing
    }

    pub fn is_ready(&self) -> bool {
        self.blockers().next().is_none()
    }
}

/// Blocking: call it off the UI thread.
pub fn check(probe: &dyn ToolProbe) -> SetupReport {
    SetupReport {
        checks: checklist(&probe.probe()),
    }
}

#[cfg(test)]
mod tests {
    use hangar_core::setup::GitFact;

    use super::*;
    use crate::fakes::FakeToolProbe;

    #[test]
    fn required_failures_come_first() {
        let probe = FakeToolProbe::default();
        let report = check(&probe);
        let failing: Vec<&str> = report.failing().iter().map(|c| c.id.as_str()).collect();
        assert_eq!(failing[..2], ["git", "agent"]);
        assert!(!report.is_ready());
        assert_eq!(report.blockers().count(), 2);
    }

    #[test]
    fn ready_once_essentials_and_an_agent_are_there() {
        let mut probe = FakeToolProbe::default();
        probe.facts.git = GitFact::Installed {
            version: "2.50.1".into(),
            name: Some("Ada".into()),
            email: Some("ada@example.com".into()),
        };
        probe.facts.agents[0].path = Some("/bin/claude".into());
        let report = check(&probe);
        assert!(report.is_ready());
        // Optional gaps are still listed.
        assert!(report.failing().iter().any(|c| c.id == "gh"));
    }
}
