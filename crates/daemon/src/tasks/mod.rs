pub mod health;
pub mod idle;
pub mod lag_monitor;
pub mod orphan_sweep;
pub mod pulse;
pub mod registry_sweep;
pub mod suspend;
pub mod usage;

#[cfg(target_os = "linux")]
pub mod crash_watcher;
#[cfg(target_os = "linux")]
pub mod ebpf_audit;
