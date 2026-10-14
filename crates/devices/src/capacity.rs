//! Reading the host's capacity: memory, cores and memory pressure. The value types and the
//! rules over them live in `hangar_core::device::capacity` and are re-exported here.

pub use hangar_core::device::capacity::*;

use crate::android::AvdCost;
use crate::{Error, Result};

/// Reads the host's memory, cores and memory pressure. Blocking but cheap (sysctl and one
/// Mach call).
#[cfg(target_os = "macos")]
pub fn detect() -> Result<HostResources> {
    let total =
        mac::sysctl("hw.memsize")?.ok_or_else(|| Error::Parse("hw.memsize missing".into()))?;
    let (perf_cores, efficiency_cores) = match (
        mac::sysctl("hw.perflevel0.physicalcpu")?,
        mac::sysctl("hw.perflevel1.physicalcpu")?,
    ) {
        (Some(p), e) => (p, e.unwrap_or(0)),
        (None, _) => (
            mac::sysctl("hw.physicalcpu")?
                .ok_or_else(|| Error::Parse("hw.physicalcpu missing".into()))?,
            0,
        ),
    };
    Ok(HostResources {
        total_ram_mb: total / MB,
        available_ram_mb: mac::available_bytes()? / MB,
        perf_cores: u32::try_from(perf_cores).unwrap_or(u32::MAX),
        efficiency_cores: u32::try_from(efficiency_cores).unwrap_or(u32::MAX),
        pressure: pressure()?,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn detect() -> Result<HostResources> {
    Err(Error::Unsupported("host resources"))
}

#[cfg(target_os = "macos")]
const MB: u64 = 1024 * 1024;

/// `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warn, 4 critical.
pub fn pressure_from_level(level: u64) -> Result<Pressure> {
    Pressure::from_level(level)
        .ok_or_else(|| Error::Parse(format!("memory pressure level {level}")))
}

/// The current memory pressure, read from the kernel (cheap enough to poll every few seconds).
#[cfg(target_os = "macos")]
pub fn pressure() -> Result<Pressure> {
    let level = mac::sysctl("kern.memorystatus_vm_pressure_level")?
        .ok_or_else(|| Error::Parse("kern.memorystatus_vm_pressure_level missing".into()))?;
    pressure_from_level(level)
}

#[cfg(not(target_os = "macos"))]
pub fn pressure() -> Result<Pressure> {
    Err(Error::Unsupported("memory pressure"))
}

/// [`pressure`], unless `HANGAR_PRESSURE=normal|warn|critical` is set (tests and development).
/// Only this function reads the variable: [`pressure`] and [`detect`] always report the real
/// reading.
pub fn pressure_with_override() -> Result<Pressure> {
    resolve_override(std::env::var("HANGAR_PRESSURE").ok().as_deref(), pressure)
}

fn resolve_override(
    value: Option<&str>,
    read: impl FnOnce() -> Result<Pressure>,
) -> Result<Pressure> {
    match value {
        Some(v) => Pressure::parse(v)
            .ok_or_else(|| Error::Parse(format!("HANGAR_PRESSURE={v} (normal|warn|critical)"))),
        None => read(),
    }
}

/// What a running instance of the AVD costs: its guest RAM plus the emulator process around it.
pub fn android_cost(avd: &AvdCost) -> DeviceCost {
    DeviceCost::android(avd.ram_mb, avd.cores)
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::CString;

    use crate::{Error, Result};

    unsafe extern "C" {
        fn mach_port_deallocate(task: libc::mach_port_t, name: libc::mach_port_t) -> libc::c_int;
    }

    /// An integer sysctl (4 or 8 bytes); `None` when the name does not exist on this host.
    pub(super) fn sysctl(name: &str) -> Result<Option<u64>> {
        let cname = CString::new(name).map_err(|e| Error::Parse(e.to_string()))?;
        let mut buf = [0u8; 8];
        let mut len = buf.len();
        // SAFETY: `buf` is writable for `len` bytes and sysctlbyname writes at most `len`.
        let r = unsafe {
            libc::sysctlbyname(
                cname.as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if r != 0 {
            let err = std::io::Error::last_os_error();
            return if err.raw_os_error() == Some(libc::ENOENT) {
                Ok(None)
            } else {
                Err(Error::Io(err))
            };
        }
        match len {
            4 => Ok(Some(u64::from(u32::from_ne_bytes([
                buf[0], buf[1], buf[2], buf[3],
            ])))),
            8 => Ok(Some(u64::from_ne_bytes(buf))),
            other => Err(Error::Parse(format!("sysctl {name}: {other} bytes"))),
        }
    }

    /// (free + inactive + speculative) pages × page size. `host_statistics64` is the only source
    /// of the inactive count: no sysctl exposes it, and leaving it out undercounts by gigabytes.
    // libc deprecates its Mach bindings in favour of `mach2`, which is not a dependency here.
    #[allow(deprecated)]
    pub(super) fn available_bytes() -> Result<u64> {
        let page = sysctl("hw.pagesize")?.ok_or_else(|| Error::Parse("hw.pagesize".into()))?;
        // SAFETY: plain-data struct, all-zero is valid.
        let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
        let mut count = libc::HOST_VM_INFO64_COUNT;
        // SAFETY: `stats` holds `count` integers; the host port right is released below.
        let kr = unsafe {
            let host = libc::mach_host_self();
            let kr = libc::host_statistics64(
                host,
                libc::HOST_VM_INFO64,
                (&raw mut stats).cast(),
                &mut count,
            );
            mach_port_deallocate(libc::mach_task_self(), host);
            kr
        };
        if kr != libc::KERN_SUCCESS {
            return Err(Error::Command(format!(
                "host_statistics64: kern_return {kr}"
            )));
        }
        let pages = u64::from(stats.free_count)
            + u64::from(stats.inactive_count)
            + u64::from(stats.speculative_count);
        Ok(pages * page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_levels_and_override() {
        assert_eq!(pressure_from_level(1).ok(), Some(Pressure::Normal));
        assert_eq!(pressure_from_level(4).ok(), Some(Pressure::Critical));
        assert!(pressure_from_level(3).is_err());
        let real = || Ok(Pressure::Normal);
        assert_eq!(
            resolve_override(Some("Critical"), real).ok(),
            Some(Pressure::Critical)
        );
        assert_eq!(resolve_override(None, real).ok(), Some(Pressure::Normal));
        assert!(resolve_override(Some("high"), real).is_err());
    }

    #[test]
    fn android_cost_reads_the_avd() {
        let c = android_cost(&AvdCost {
            ram_mb: 4096,
            cores: 4,
        });
        assert_eq!((c.ram_mb, c.cores), (5632, 4.0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "prints this host's resources"]
    fn print_detected_host() -> Result<()> {
        let h = detect()?;
        println!("{h:#?}");
        let avd = android_cost(&AvdCost {
            ram_mb: 4096,
            cores: 4,
        });
        println!(
            "cpu_capacity {} | slots iOS {} | slots Android 4c/4G {}",
            cpu_capacity(&h),
            recommended_slots(&h, &DeviceCost::ios_simulator()),
            recommended_slots(&h, &avd)
        );
        println!(
            "can boot iOS: {:?}",
            can_boot_another(&h, &DeviceCost::ios_simulator())
        );
        Ok(())
    }
}
