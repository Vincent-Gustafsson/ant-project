use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct CpuMetrics {
    pub usage_percent: f32,
    pub core_usages: Vec<f32>,
    pub core_count: usize,
}

#[derive(Debug, Serialize)]
pub struct MemoryMetrics {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct DiskMetrics {
    pub name: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub usage_percent: f32,
}

#[derive(Debug, Serialize)]
pub struct SystemMetrics {
    pub node_name: String,
    pub cpu: CpuMetrics,
    pub memory: MemoryMetrics,
    pub disks: Vec<DiskMetrics>,
}
