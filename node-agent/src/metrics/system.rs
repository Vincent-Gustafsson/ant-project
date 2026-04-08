use sysinfo::{Disks, System};

use crate::metrics::types::{CpuMetrics, DiskMetrics, MemoryMetrics, SystemMetrics};

pub fn collect(sys: &mut System, disks: &mut Disks, node_name: &str) -> SystemMetrics {
    sys.refresh_cpu_all();
    sys.refresh_memory();
    disks.refresh(true);

    SystemMetrics {
        node_name: node_name.to_string(),
        cpu: collect_cpu(sys),
        memory: collect_memory(sys),
        disks: collect_disks(disks),
    }
}

fn collect_cpu(sys: &System) -> CpuMetrics {
    let cores: Vec<f32> = sys.cpus().iter().map(|c| c.cpu_usage()).collect();
    CpuMetrics {
        usage_percent: sys.global_cpu_usage(),
        core_count: cores.len(),
        core_usages: cores,
    }
}

fn collect_memory(sys: &System) -> MemoryMetrics {
    MemoryMetrics {
        total_bytes: sys.total_memory(),
        used_bytes: sys.used_memory(),
        available_bytes: sys.available_memory(),
        swap_total_bytes: sys.total_swap(),
        swap_used_bytes: sys.used_swap(),
    }
}

fn collect_disks(disks: &Disks) -> Vec<DiskMetrics> {
    disks
        .iter()
        .filter(|d| d.total_space() > 0)
        .map(|d| {
            let total = d.total_space();
            let available = d.available_space();
            let used = total - available;
            DiskMetrics {
                name: d.name().to_string_lossy().into_owned(),
                mount_point: d.mount_point().to_string_lossy().into_owned(),
                total_bytes: total,
                used_bytes: used,
                available_bytes: available,
                usage_percent: (used as f32 / total as f32) * 100.0,
            }
        })
        .collect()
}
