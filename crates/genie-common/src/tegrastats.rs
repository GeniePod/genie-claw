use anyhow::Result;
use serde::Serialize;

/// Parsed snapshot from a single `tegrastats` output line.
///
/// `tegrastats` is NVIDIA's built-in Jetson monitoring tool.
/// Format (Orin Nano): `RAM 2345/7620MB (lfb 234x4MB) SWAP 0/0MB ... GR3D_FREQ 50% ...`
#[derive(Debug, Clone, Serialize)]
pub struct TegraSnapshot {
    pub timestamp_ms: u64,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub swap_used_mb: u64,
    pub swap_total_mb: u64,
    pub gpu_freq_pct: u8,
    pub cpu_loads: Vec<CpuCore>,
    pub gpu_temp_c: Option<f32>,
    pub cpu_temp_c: Option<f32>,
    pub power_mw: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CpuCore {
    pub id: u8,
    pub load_pct: u8,
    pub freq_mhz: u32,
}

impl TegraSnapshot {
    /// Available RAM in MB (total - used).
    pub fn ram_available_mb(&self) -> u64 {
        self.ram_total_mb.saturating_sub(self.ram_used_mb)
    }
}

/// Parse a single line of `tegrastats` output.
///
/// Example line:
/// ```text
/// RAM 2345/7620MB (lfb 234x4MB) SWAP 0/3810MB (cached 0MB) CPU [20%@1510,15%@1510,10%@1510,8%@1510,off,off] ... GR3D_FREQ 50% ... gpu@42C cpu@38.5C ... VDD_IN 4500mW/4500mW
/// ```
pub fn parse_line(line: &str, timestamp_ms: u64) -> Result<TegraSnapshot> {
    let ram = parse_ram(line)?;
    let swap = parse_swap(line);
    let gpu_freq = parse_gpu_freq(line);
    let cpus = parse_cpus(line);
    let gpu_temp = parse_temp(line, "gpu@");
    let cpu_temp = parse_temp(line, "cpu@");
    let power = parse_power(line);

    Ok(TegraSnapshot {
        timestamp_ms,
        ram_used_mb: ram.0,
        ram_total_mb: ram.1,
        swap_used_mb: swap.0,
        swap_total_mb: swap.1,
        gpu_freq_pct: gpu_freq,
        cpu_loads: cpus,
        gpu_temp_c: gpu_temp,
        cpu_temp_c: cpu_temp,
        power_mw: power,
    })
}

fn parse_ram(line: &str) -> Result<(u64, u64)> {
    // "RAM 2345/7620MB"
    let ram_pos = line
        .find("RAM ")
        .ok_or_else(|| anyhow::anyhow!("no RAM field"))?;
    let rest = &line[ram_pos + 4..];
    let mb_pos = rest.find("MB").unwrap_or(rest.len());
    let fraction = &rest[..mb_pos];
    let (used, total) = fraction
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("no / in RAM field"))?;
    Ok((used.trim().parse()?, total.trim().parse()?))
}

fn parse_swap(line: &str) -> (u64, u64) {
    // "SWAP 0/3810MB"
    if let Some(pos) = line.find("SWAP ") {
        let rest = &line[pos + 5..];
        if let Some(mb_pos) = rest.find("MB") {
            let fraction = &rest[..mb_pos];
            if let Some((used, total)) = fraction.split_once('/')
                && let (Ok(u), Ok(t)) = (used.trim().parse(), total.trim().parse())
            {
                return (u, t);
            }
        }
    }
    (0, 0)
}

fn parse_gpu_freq(line: &str) -> u8 {
    // "GR3D_FREQ 50%"
    if let Some(pos) = line.find("GR3D_FREQ ") {
        let rest = &line[pos + 10..];
        if let Some(pct_pos) = rest.find('%')
            && let Ok(v) = rest[..pct_pos].trim().parse()
        {
            return v;
        }
    }
    0
}

fn parse_cpus(line: &str) -> Vec<CpuCore> {
    // "CPU [20%@1510,15%@1510,off,off]"
    let mut cores = Vec::new();
    if let Some(start) = line.find("CPU [") {
        let rest = &line[start + 5..];
        if let Some(end) = rest.find(']') {
            for (i, part) in rest[..end].split(',').enumerate() {
                let part = part.trim();
                if part == "off" {
                    continue;
                }
                if let Some((load_str, freq_str)) = part.split_once('@') {
                    let load = load_str.trim_end_matches('%').parse().unwrap_or(0);
                    let freq = freq_str.parse().unwrap_or(0);
                    cores.push(CpuCore {
                        id: i as u8,
                        load_pct: load,
                        freq_mhz: freq,
                    });
                }
            }
        }
    }
    cores
}

fn parse_temp(line: &str, prefix: &str) -> Option<f32> {
    // "gpu@42C" or "cpu@38.5C"
    if let Some(pos) = line.find(prefix) {
        let rest = &line[pos + prefix.len()..];
        let end = rest.find('C').unwrap_or(rest.len());
        return rest[..end].trim().parse().ok();
    }
    None
}

fn parse_power(line: &str) -> Option<u32> {
    // "VDD_IN 4500mW/4500mW" — we want the instantaneous (first) value
    if let Some(pos) = line.find("VDD_IN ") {
        let rest = &line[pos + 7..];
        if let Some(mw_pos) = rest.find("mW") {
            return rest[..mw_pos].trim().parse().ok();
        }
    }
    None
}

/// Read `/proc/meminfo` and return MemAvailable in MB.
///
/// Synchronous filesystem I/O — callers on a shared async executor (every
/// caller in this workspace: `genie-core`, `genie-api`, `genie-governor` are
/// all `tokio::main(flavor = "current_thread")`) must go through
/// [`mem_available_mb_async`] instead of calling this directly, so a slow
/// `/proc` read (e.g. under cgroup throttling or heavy memory pressure)
/// can't stall every other concurrent session on the same thread.
pub fn mem_available_mb() -> Result<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo")?;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb_str = rest.trim().trim_end_matches(" kB").trim();
            let kb: u64 = kb_str.parse()?;
            return Ok(kb / 1024);
        }
    }
    anyhow::bail!("MemAvailable not found in /proc/meminfo")
}

/// Async wrapper around [`mem_available_mb`] for callers running on a shared
/// `current_thread` executor. Moves the synchronous `/proc/meminfo` read to
/// Tokio's blocking thread pool instead of running it directly on the
/// caller's task — the same pattern `memory::with_shared_memory` already
/// established for the same class of bug.
pub async fn mem_available_mb_async() -> Result<u64> {
    tokio::task::spawn_blocking(mem_available_mb)
        .await
        .unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "RAM 2345/7620MB (lfb 234x4MB) SWAP 0/3810MB (cached 0MB) CPU [20%@1510,15%@1510,10%@1510,8%@1510,off,off] EMC_FREQ 0% GR3D_FREQ 50% VIC_FREQ 0% APE 174 gpu@42C cpu@38.5C iwlwifi@37C CV0@-256C CV1@-256C CV2@-256C SOC2@38.5C SOC0@40C SOC1@37.5C tj@42C VDD_IN 4500mW/4500mW VDD_CPU_GPU_CV 799mW/799mW VDD_SOC 1598mW/1598mW";

    /// `mem_available_mb_async` must return the same result as the
    /// synchronous version — the only difference is running on Tokio's
    /// blocking pool instead of the calling task.
    #[tokio::test]
    async fn mem_available_mb_async_matches_sync_version() {
        let sync_result = mem_available_mb();
        let async_result = mem_available_mb_async().await;
        assert_eq!(sync_result.is_ok(), async_result.is_ok());
        if let (Ok(sync_val), Ok(async_val)) = (sync_result, async_result) {
            assert_eq!(sync_val, async_val);
        }
    }

    #[test]
    fn parse_ram_values() {
        let snap = parse_line(SAMPLE, 0).unwrap();
        assert_eq!(snap.ram_used_mb, 2345);
        assert_eq!(snap.ram_total_mb, 7620);
        assert_eq!(snap.ram_available_mb(), 7620 - 2345);
    }

    #[test]
    fn parse_swap_values() {
        let snap = parse_line(SAMPLE, 0).unwrap();
        assert_eq!(snap.swap_used_mb, 0);
        assert_eq!(snap.swap_total_mb, 3810);
    }

    #[test]
    fn parse_gpu_freq_value() {
        let snap = parse_line(SAMPLE, 0).unwrap();
        assert_eq!(snap.gpu_freq_pct, 50);
    }

    #[test]
    fn parse_cpu_cores() {
        let snap = parse_line(SAMPLE, 0).unwrap();
        assert_eq!(snap.cpu_loads.len(), 4); // 4 active, 2 off
        assert_eq!(snap.cpu_loads[0].load_pct, 20);
        assert_eq!(snap.cpu_loads[0].freq_mhz, 1510);
    }

    #[test]
    fn parse_temperatures() {
        let snap = parse_line(SAMPLE, 0).unwrap();
        assert_eq!(snap.gpu_temp_c, Some(42.0));
        assert_eq!(snap.cpu_temp_c, Some(38.5));
    }

    #[test]
    fn parse_power_draw() {
        let snap = parse_line(SAMPLE, 0).unwrap();
        assert_eq!(snap.power_mw, Some(4500));
    }

    /// RAM is the required anchor field — a line missing it must be a hard
    /// `Err`, not a zeroed-out snapshot.
    #[test]
    fn parse_line_without_ram_field_errors() {
        let line = "SWAP 0/3810MB (cached 0MB) CPU [20%@1510] GR3D_FREQ 50% gpu@42C cpu@38.5C VDD_IN 4500mW/4500mW";
        assert!(parse_line(line, 0).is_err());
    }

    /// Every field except RAM must degrade to its documented default when
    /// absent, so a minimal RAM-only line still yields a usable snapshot.
    #[test]
    fn parse_line_ram_only_uses_documented_defaults() {
        let line = "RAM 1000/8000MB (lfb 10x4MB)";
        let snap = parse_line(line, 0).unwrap();
        assert_eq!(snap.ram_used_mb, 1000);
        assert_eq!(snap.ram_total_mb, 8000);
        assert_eq!(snap.swap_used_mb, 0);
        assert_eq!(snap.swap_total_mb, 0);
        assert_eq!(snap.gpu_freq_pct, 0);
        assert!(snap.cpu_loads.is_empty());
        assert_eq!(snap.gpu_temp_c, None);
        assert_eq!(snap.cpu_temp_c, None);
        assert_eq!(snap.power_mw, None);
    }

    /// `parse_cpus` drops `off` cores but surviving cores must keep their
    /// physical bracket index rather than being renumbered densely.
    #[test]
    fn parse_cpus_keeps_physical_index_across_off_cores() {
        let line = "RAM 1000/8000MB CPU [off,30%@1000,off,45%@2000]";
        let snap = parse_line(line, 0).unwrap();
        assert_eq!(snap.cpu_loads.len(), 2);
        assert_eq!(snap.cpu_loads[0].id, 1);
        assert_eq!(snap.cpu_loads[0].load_pct, 30);
        assert_eq!(snap.cpu_loads[0].freq_mhz, 1000);
        assert_eq!(snap.cpu_loads[1].id, 3);
        assert_eq!(snap.cpu_loads[1].load_pct, 45);
        assert_eq!(snap.cpu_loads[1].freq_mhz, 2000);
    }

    /// All cores reported `off` is a valid state (idle low-power cluster)
    /// and must yield an empty core list rather than an error.
    #[test]
    fn parse_cpus_all_off_yields_empty_list() {
        let line = "RAM 1000/8000MB CPU [off,off,off,off]";
        let snap = parse_line(line, 0).unwrap();
        assert!(snap.cpu_loads.is_empty());
    }
}
