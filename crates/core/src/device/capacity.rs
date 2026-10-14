//! Host capacity rules: how many simulators and emulators this machine can run at once. Pure
//! data and arithmetic; reading the host lives behind [`crate::ports::HostMonitor`].

/// RAM kept for macOS, the IDE, the agents and the builds.
pub const RESERVE_RAM_MB: u64 = 8192;
/// Cores kept for the same.
pub const RESERVE_CORES: f32 = 4.0;
/// Upper bound on concurrently running devices, whatever the host.
pub const MAX_SLOTS: u32 = 8;
/// Free RAM that must remain after one more device has booted.
pub const BOOT_HEADROOM_MB: u64 = 2048;

/// The kernel's memory pressure level (`kern.memorystatus_vm_pressure_level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressure {
    Normal,
    Warn,
    Critical,
}

impl Pressure {
    /// `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warn, 4 critical.
    pub fn from_level(level: u64) -> Option<Self> {
        match level {
            1 => Some(Self::Normal),
            2 => Some(Self::Warn),
            4 => Some(Self::Critical),
            _ => None,
        }
    }

    /// `normal`, `warn` or `critical` (the `HANGAR_PRESSURE` values).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "normal" => Some(Self::Normal),
            "warn" => Some(Self::Warn),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

impl std::fmt::Display for Pressure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Normal => "normal",
            Self::Warn => "warn",
            Self::Critical => "critical",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostResources {
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    pub perf_cores: u32,
    pub efficiency_cores: u32,
    pub pressure: Pressure,
}

/// What one running device asks of the host.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeviceCost {
    pub ram_mb: u64,
    pub cores: f32,
}

impl DeviceCost {
    pub fn ios_simulator() -> Self {
        Self {
            ram_mb: 2560,
            cores: 1.0,
        }
    }

    /// An AVD's guest RAM and cores plus the emulator process around it.
    pub fn android(guest_ram_mb: u64, cores: u32) -> Self {
        Self {
            ram_mb: guest_ram_mb + 1536,
            cores: cores as f32,
        }
    }
}

/// Efficiency cores count for half a performance core.
pub fn cpu_capacity(host: &HostResources) -> f32 {
    host.perf_cores as f32 + host.efficiency_cores as f32 / 2.0
}

/// How many devices of `cost` fit next to the reserve, between 1 and [`MAX_SLOTS`].
pub fn recommended_slots(host: &HostResources, cost: &DeviceCost) -> u32 {
    let by_ram = host
        .total_ram_mb
        .saturating_sub(RESERVE_RAM_MB)
        .checked_div(cost.ram_mb)
        .unwrap_or(u64::MAX);
    let spare_cores = (cpu_capacity(host) - RESERVE_CORES).max(0.0);
    let by_cpu = if cost.cores > 0.0 {
        (spare_cores / cost.cores).floor() as u64
    } else {
        u64::MAX
    };
    let slots = by_ram.min(by_cpu).clamp(1, u64::from(MAX_SLOTS));
    u32::try_from(slots).unwrap_or(MAX_SLOTS)
}

/// Why another device should not boot now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BootRefusal {
    #[error("memory pressure is {0}: close a device or an app before booting another")]
    Pressure(Pressure),
    #[error(
        "only {available_mb} MB of RAM available, booting another device needs {needed_mb} MB \
         ({device_mb} MB for the device + {headroom_mb} MB headroom)",
        headroom_mb = BOOT_HEADROOM_MB
    )]
    LowMemory {
        available_mb: u64,
        needed_mb: u64,
        device_mb: u64,
    },
}

/// Refuses under memory pressure or when the device would leave less than
/// [`BOOT_HEADROOM_MB`] available.
pub fn can_boot_another(host: &HostResources, cost: &DeviceCost) -> Result<(), BootRefusal> {
    if host.pressure != Pressure::Normal {
        return Err(BootRefusal::Pressure(host.pressure));
    }
    let needed_mb = cost.ram_mb + BOOT_HEADROOM_MB;
    if host.available_ram_mb < needed_mb {
        return Err(BootRefusal::LowMemory {
            available_mb: host.available_ram_mb,
            needed_mb,
            device_mb: cost.ram_mb,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(ram_gb: u64, perf: u32, eff: u32) -> HostResources {
        HostResources {
            total_ram_mb: ram_gb * 1024,
            available_ram_mb: ram_gb * 512,
            perf_cores: perf,
            efficiency_cores: eff,
            pressure: Pressure::Normal,
        }
    }

    fn avd_4c_4g() -> DeviceCost {
        DeviceCost::android(4096, 4)
    }

    #[test]
    fn slots_per_profile() {
        let ios = DeviceCost::ios_simulator();
        // 8 GB: nothing left after the reserve, still one slot.
        assert_eq!(recommended_slots(&host(8, 4, 4), &ios), 1);
        assert_eq!(recommended_slots(&host(8, 4, 4), &avd_4c_4g()), 1);
        // 16 GB / 4P+4E: RAM allows 3, CPU (4 + 2 - 4) / 1 allows 2.
        assert_eq!(recommended_slots(&host(16, 4, 4), &ios), 2);
        assert_eq!(recommended_slots(&host(16, 4, 4), &avd_4c_4g()), 1);
        // 48 GB / 6P+12E: RAM 16, CPU (6 + 6 - 4) / 1 = 8.
        assert_eq!(recommended_slots(&host(48, 6, 12), &ios), 8);
        // RAM (49152 - 8192) / 5632 = 7, CPU (12 - 4) / 4 = 2.
        assert_eq!(recommended_slots(&host(48, 6, 12), &avd_4c_4g()), 2);
        // Never above the cap.
        assert_eq!(recommended_slots(&host(512, 64, 0), &ios), MAX_SLOTS);
    }

    #[test]
    fn cpu_capacity_halves_efficiency_cores() {
        assert_eq!(cpu_capacity(&host(48, 6, 12)), 12.0);
        assert_eq!(cpu_capacity(&host(16, 8, 0)), 8.0);
    }

    #[test]
    fn android_cost_adds_the_emulator_overhead() {
        let c = avd_4c_4g();
        assert_eq!(c.ram_mb, 5632);
        assert_eq!(c.cores, 4.0);
    }

    #[test]
    fn boot_refusals() {
        let ios = DeviceCost::ios_simulator();
        let mut h = host(48, 6, 12);
        h.available_ram_mb = 4608;
        assert_eq!(can_boot_another(&h, &ios), Ok(()));
        h.available_ram_mb = 4607;
        let refusal = can_boot_another(&h, &ios);
        assert_eq!(
            refusal,
            Err(BootRefusal::LowMemory {
                available_mb: 4607,
                needed_mb: 4608,
                device_mb: 2560,
            })
        );
        let message = refusal.err().map(|r| r.to_string()).unwrap_or_default();
        assert!(message.contains("4607 MB") && message.contains("4608 MB"));
        h.available_ram_mb = 40_000;
        h.pressure = Pressure::Warn;
        assert_eq!(
            can_boot_another(&h, &ios),
            Err(BootRefusal::Pressure(Pressure::Warn))
        );
        h.pressure = Pressure::Critical;
        assert!(can_boot_another(&h, &ios).is_err());
    }

    #[test]
    fn pressure_levels_and_names() {
        assert_eq!(Pressure::from_level(1), Some(Pressure::Normal));
        assert_eq!(Pressure::from_level(2), Some(Pressure::Warn));
        assert_eq!(Pressure::from_level(4), Some(Pressure::Critical));
        assert_eq!(Pressure::from_level(3), None);
        assert_eq!(Pressure::parse(" Critical"), Some(Pressure::Critical));
        assert_eq!(Pressure::parse("high"), None);
        assert_eq!(Pressure::Warn.to_string(), "warn");
    }
}
