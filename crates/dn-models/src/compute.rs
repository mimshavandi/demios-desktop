//! Where local models run on this machine (T-INF-24): a graphics card, graphics that share the
//! system memory (Apple Silicon, integrated GPUs), or the processor. The callers (`dn`, the
//! desktop app) pass the GPUs llama.cpp reports (Metal on macOS, Vulkan on Windows and Linux);
//! this turns them into the [`Machine`] the fit estimator sizes models for, and a [`Compute`]
//! the UI shows, so running on the processor is a stated mode, never a silent fallback.

use crate::fit::{Machine, MemoryKind};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuKind {
    /// Its own memory (VRAM).
    Discrete,
    /// Shares the system memory (integrated graphics, Apple Silicon).
    Integrated,
}

/// A GPU llama.cpp can offload to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gpu {
    /// Human-readable name, e.g. "NVIDIA GeForce RTX 3060" or "Apple M3".
    pub name: String,
    /// "Metal", "Vulkan", "CUDA".
    pub backend: String,
    pub kind: GpuKind,
    /// Device memory llama.cpp reports (for integrated GPUs: the share it may use).
    pub memory_total: u64,
}

/// Why models run on the processor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CpuReason {
    /// This build has GPU support, but no usable GPU (or no driver) was found.
    NoGpu,
    /// This build has no GPU support at all.
    NoGpuSupport,
}

/// Where local models run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Compute {
    /// A graphics card with its own memory.
    Gpu {
        name: String,
        backend: String,
        vram: u64,
    },
    /// Graphics that share the system memory.
    SharedGpu { name: String, backend: String },
    /// The processor.
    Cpu { reason: CpuReason },
}

impl Compute {
    /// One sentence for the UI and `dn fit`.
    pub fn describe(&self) -> String {
        match self {
            Compute::Gpu { name, vram, .. } => format!(
                "Local models run on the graphics card: {name} ({:.0} GB).",
                *vram as f64 / (1u64 << 30) as f64
            ),
            Compute::SharedGpu { name, .. } => format!(
                "Local models run on the built-in graphics ({name}), which share this computer's memory."
            ),
            Compute::Cpu {
                reason: CpuReason::NoGpu,
            } => "Local models run on the processor: no graphics chip Demios can use was found, so replies are slower.".into(),
            Compute::Cpu {
                reason: CpuReason::NoGpuSupport,
            } => "Local models run on the processor: this build of Demios has no graphics-chip support, so replies are slower.".into(),
        }
    }

    pub fn is_cpu(&self) -> bool {
        matches!(self, Compute::Cpu { .. })
    }
}

/// GPUs from llama.cpp's device list: `(name, description, kind, memory_total)` per device,
/// where `kind` is dn-inference's `DeviceInfo::kind` (`gpu`, `igpu`, `cpu`, …). Devices
/// other than GPUs are skipped.
pub fn gpus_from<'a>(
    backend: &str,
    devices: impl IntoIterator<Item = (&'a str, &'a str, &'a str, u64)>,
) -> Vec<Gpu> {
    devices
        .into_iter()
        .filter_map(|(name, description, kind, memory_total)| {
            let kind = match kind {
                "gpu" => GpuKind::Discrete,
                "igpu" => GpuKind::Integrated,
                _ => return None,
            };
            Some(Gpu {
                name: if description.is_empty() {
                    name
                } else {
                    description
                }
                .to_string(),
                backend: backend.to_string(),
                kind,
                memory_total,
            })
        })
        .collect()
}

/// The machine to size models for, and where they run. `base` is [`crate::detect_machine`]
/// (RAM, unified memory on Apple Silicon); `gpus` what llama.cpp reports; `gpu_build` whether
/// this build has a GPU backend at all.
pub fn plan(base: Machine, gpus: &[Gpu], gpu_build: bool) -> (Machine, Compute) {
    let largest = |kind| {
        gpus.iter()
            .filter(|g| g.kind == kind)
            .max_by_key(|g| g.memory_total)
    };
    let mut m = base;
    if let MemoryKind::Unified { .. } = base.memory {
        // Apple Silicon: the GPU shares the RAM; its reported size is the Metal limit.
        if let Some(g) = largest(GpuKind::Integrated).or_else(|| largest(GpuKind::Discrete)) {
            m.memory = MemoryKind::Unified {
                gpu_budget: Some(g.memory_total).filter(|&b| b > 0),
            };
            return (
                m,
                Compute::SharedGpu {
                    name: g.name.clone(),
                    backend: g.backend.clone(),
                },
            );
        }
        return (m, cpu(gpu_build));
    }
    if let Some(g) = largest(GpuKind::Discrete).filter(|g| g.memory_total > 0) {
        m.memory = MemoryKind::Discrete {
            vram: g.memory_total,
        };
        return (
            m,
            Compute::Gpu {
                name: g.name.clone(),
                backend: g.backend.clone(),
                vram: g.memory_total,
            },
        );
    }
    if let Some(g) = largest(GpuKind::Integrated).filter(|g| g.memory_total > 0) {
        // Integrated graphics share the RAM: size against the smaller of the share the
        // driver allows and what the system can spare.
        let spare = Machine::cpu_only(m.total_ram).cpu_budget_bytes();
        m.memory = MemoryKind::Unified {
            gpu_budget: Some(g.memory_total.min(spare)),
        };
        return (
            m,
            Compute::SharedGpu {
                name: g.name.clone(),
                backend: g.backend.clone(),
            },
        );
    }
    m.memory = MemoryKind::CpuOnly;
    (m, cpu(gpu_build))
}

fn cpu(gpu_build: bool) -> Compute {
    Compute::Cpu {
        reason: if gpu_build {
            CpuReason::NoGpu
        } else {
            CpuReason::NoGpuSupport
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::GIB;

    fn gpu(name: &str, kind: GpuKind, gib: u64) -> Gpu {
        Gpu {
            name: name.into(),
            backend: "Vulkan".into(),
            kind,
            memory_total: gib * GIB,
        }
    }

    #[test]
    fn no_gpu_is_a_labelled_cpu_mode() {
        let (m, c) = plan(Machine::cpu_only(16 * GIB), &[], true);
        assert_eq!(m.memory, MemoryKind::CpuOnly);
        assert_eq!(
            c,
            Compute::Cpu {
                reason: CpuReason::NoGpu
            }
        );
        assert!(c.is_cpu());
        assert!(c.describe().contains("processor"));
        assert!(c.describe().contains("no graphics chip"));
        let (_, c) = plan(Machine::cpu_only(16 * GIB), &[], false);
        assert!(c.describe().contains("no graphics-chip support"));
    }

    #[test]
    fn discrete_gpu_sizes_by_its_vram() {
        let gpus = [
            gpu("Intel(R) UHD Graphics", GpuKind::Integrated, 8),
            gpu("NVIDIA GeForce RTX 3060", GpuKind::Discrete, 12),
            gpu("NVIDIA GeForce GT 1030", GpuKind::Discrete, 2),
        ];
        let (m, c) = plan(Machine::cpu_only(32 * GIB), &gpus, true);
        assert_eq!(m.memory, MemoryKind::Discrete { vram: 12 * GIB });
        assert_eq!(m.total_ram, 32 * GIB);
        assert!(
            c.describe().contains("RTX 3060 (12 GB)"),
            "{}",
            c.describe()
        );
    }

    #[test]
    fn integrated_gpu_shares_memory() {
        let (m, c) = plan(
            Machine::cpu_only(16 * GIB),
            &[gpu("AMD Radeon 780M", GpuKind::Integrated, 12)],
            true,
        );
        // 16 GiB RAM: at most 12.8 GiB to spare, and the driver allows 12 GiB.
        assert_eq!(
            m.memory,
            MemoryKind::Unified {
                gpu_budget: Some(12 * GIB)
            }
        );
        assert!(matches!(c, Compute::SharedGpu { .. }));
        let (m, _) = plan(
            Machine::cpu_only(8 * GIB),
            &[gpu("Intel Iris Xe", GpuKind::Integrated, 7)],
            true,
        );
        assert_eq!(
            m.memory,
            MemoryKind::Unified {
                gpu_budget: Some(6 * GIB)
            }
        );
    }

    #[test]
    fn apple_silicon_uses_the_metal_limit() {
        let apple = Gpu {
            name: "Apple M3".into(),
            backend: "Metal".into(),
            kind: GpuKind::Integrated,
            memory_total: 11 * GIB,
        };
        let (m, c) = plan(Machine::unified(16 * GIB), &[apple], true);
        assert_eq!(
            m.memory,
            MemoryKind::Unified {
                gpu_budget: Some(11 * GIB)
            }
        );
        assert!(c.describe().contains("Apple M3"));
    }

    #[test]
    fn devices_become_gpus() {
        let g = gpus_from(
            "Vulkan",
            [
                ("CPU", "Apple M3", "cpu", 0),
                ("Vulkan0", "NVIDIA GeForce RTX 4070", "gpu", 12 * GIB),
                ("Vulkan1", "", "igpu", 4 * GIB),
            ],
        );
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].name, "NVIDIA GeForce RTX 4070");
        assert_eq!(
            (g[1].name.as_str(), g[1].kind),
            ("Vulkan1", GpuKind::Integrated)
        );
    }

    #[test]
    fn a_gpu_reporting_no_memory_is_not_used() {
        let (m, c) = plan(
            Machine::cpu_only(16 * GIB),
            &[gpu("llvmpipe", GpuKind::Discrete, 0)],
            true,
        );
        assert_eq!(m.memory, MemoryKind::CpuOnly);
        assert!(c.is_cpu());
    }
}
