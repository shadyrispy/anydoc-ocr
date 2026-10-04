use super::{session, *};
use crate::core::config::ModelInferenceConfig;
use crate::core::inference::ModelSource;
use ort::logging::LogLevel;
use std::sync::Mutex;

/// Session 池大小（anydoc-orch A1）：`ANYDOC_ORT_SESSION_POOL` 显式设置时加载
/// N 份同模型 session（1..=8），`predict` 并发经轮转分池消除单 session 锁 convoy；
/// 未设置 = 1（上游行为，零变化）。
///
/// 内存代价：每 session 持有独立权重 + arena。tiny/small 档全模型组约 30–100 MB
/// 权重，×池 4 ≈ ≤400 MB，8 GB 预算可承受；formula/大档部署请先实测再开池。
/// CUDA EP 下保持 1：onnxruntime#4829（FormulaNet Loop 串扰）要求 CUDA 工作
/// 驱动级串行，多 session 不改变该约束。
fn session_pool_size(common_cfg: Option<&crate::core::config::OrtSessionConfig>) -> usize {
    let want: Option<usize> = std::env::var("ANYDOC_ORT_SESSION_POOL")
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let Some(n) = want else { return 1 };
    let cuda = common_cfg
        .and_then(|c| c.execution_providers.as_ref())
        .is_some_and(|eps| {
            eps.iter().any(|ep| {
                matches!(
                    ep,
                    crate::core::config::OrtExecutionProvider::CUDA { .. }
                        | crate::core::config::OrtExecutionProvider::TensorRT { .. }
                )
            })
        });
    if cuda {
        1
    } else {
        n.clamp(1, 8)
    }
}

impl OrtInfer {
    /// First declared input name of the loaded session.
    ///
    /// MinerU's ONNX wrappers bind `session.get_inputs()[0].name` rather than a
    /// hard-coded key, and exported graphs disagree on naming (`x`, `image`,
    /// `encoder/image`). Callers that pass `input_name: None` opt into this
    /// auto-detection; only an empty graph falls back to `"x"`.
    fn first_input_name(session: &Session) -> String {
        session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .unwrap_or_else(|| "x".to_string())
    }

    /// Creates a new OrtInfer instance with default ONNX Runtime settings and a single session.
    pub fn new(
        model_source: impl Into<ModelSource>,
        input_name: Option<&str>,
    ) -> Result<Self, OCRError> {
        let source = model_source.into();
        let session = session::load_session_with(
            source.clone(),
            |builder| Ok(builder.with_log_level(LogLevel::Error)?),
            Some("verify model path and compatibility with selected execution providers"),
        )?;
        let model_name = "unknown_model".to_string();
        let resolved_input_name =
            input_name.map(str::to_string).unwrap_or_else(|| Self::first_input_name(&session));

        Ok(OrtInfer {
            // 此路径无 OrtSessionConfig（不读 EP 配置），恒单 session——保持上游行为。
            sessions: vec![Mutex::new(session)],
            next_idx: std::sync::atomic::AtomicUsize::new(0),
            input_name: resolved_input_name,
            model_path: source.display_path(),
            model_name,
            run_options: None,
        })
    }

    /// Creates a new OrtInfer instance from ModelInferenceConfig, applying ORT session
    /// configuration.
    pub fn from_config(
        common: &ModelInferenceConfig,
        model_source: impl Into<ModelSource>,
        input_name: Option<&str>,
    ) -> Result<Self, OCRError> {
        let source = model_source.into();

        // Workaround for a non-deterministic data race in ORT's CUDA EP that
        // corrupts arena buffers reused across `session.run()` calls.
        // Concretely, PP-FormulaNet's autoregressive Loop produces correct
        // tokens on the first run and pure garbage (max-trip-count) on every
        // subsequent run unless CUDA work is serialized at the driver level.
        Self::ensure_cuda_launch_blocking_if_needed(common);

        let pool = session_pool_size(common.ort_session.as_ref());
        let mut sessions = Vec::with_capacity(pool);
        let mut first_input_name: Option<String> = None;
        let mut run_options: Option<ort::session::RunOptions> = None;
        for i in 0..pool {
            let session = session::load_session_with(
                source.clone(),
                |builder| {
                    if let Some(cfg) = &common.ort_session {
                        Self::apply_ort_config(builder, cfg)
                    } else {
                        Ok(builder.with_log_level(LogLevel::Error)?)
                    }
                },
                Some("check device/EP configuration and model file"),
            )
            .map_err(|e| {
                // 池中途失败：报哪个 session 建不起来（1 = 首个，与原行为一致）
                if i == 0 {
                    e
                } else {
                    OCRError::InvalidInput {
                        message: format!(
                            "Model '{}': failed to build session {}/{} in pool: {e}",
                            common.model_name.as_deref().unwrap_or("unknown_model"),
                            i + 1,
                            pool
                        ),
                    }
                }
            })?;
            if i == 0 {
                first_input_name = Some(Self::first_input_name(&session));
                // 0.10.0 的 CUDA arena shrinkage（防多页 PDF OOM）。池化下 CUDA 恒为 1
                // session（见 `session_pool_size`），故只需对首个 session 求一次
                // run_options；`RunOptions` 与 session 无绑定关系，可安全复用到全池。
                run_options = Self::arena_shrinkage_run_options(common, &session)?;
            }
            sessions.push(Mutex::new(session));
        }

        let model_name = common
            .model_name
            .clone()
            .unwrap_or_else(|| "unknown_model".to_string());
        let resolved_input_name = match (input_name, first_input_name) {
            (Some(n), _) => n.to_string(),
            (None, Some(detected)) => detected,
            (None, None) => "x".to_string(), // pool 恒 >=1，理论不可达
        };

        Ok(OrtInfer {
            sessions,
            next_idx: std::sync::atomic::AtomicUsize::new(0),
            input_name: resolved_input_name,
            model_path: source.display_path(),
            model_name,
            run_options,
        })
    }

    /// Builds the run options that return idle CUDA arena memory after each
    /// run, when [`OrtSessionConfig::arena_shrinkage`] is enabled and the
    /// session actually runs on a CUDA device.
    ///
    /// A failed CUDA provider registration falls back to CPU without an error,
    /// and ONNX Runtime rejects every run whose shrink list names a device the
    /// session has no arena for. So the device comes from the session's
    /// registered allocators, not just the requested configuration.
    ///
    /// [`OrtSessionConfig::arena_shrinkage`]: crate::core::config::OrtSessionConfig::arena_shrinkage
    fn arena_shrinkage_run_options(
        common: &ModelInferenceConfig,
        session: &ort::session::Session,
    ) -> Result<Option<ort::session::RunOptions>, OCRError> {
        let Some(device_id) = Self::arena_shrinkage_device(common) else {
            return Ok(None);
        };
        if !Self::session_has_cuda_allocator(session, device_id) {
            tracing::warn!(
                "CUDA arena shrinkage requested for gpu:{device_id}, but the session has no \
                 CUDA allocator there (CUDA provider not registered); continuing without it"
            );
            return Ok(None);
        }
        let device = format!("gpu:{device_id}");
        let build = || -> ort::Result<ort::session::RunOptions> {
            let mut options = ort::session::RunOptions::new()?;
            options.set("memory.enable_memory_arena_shrinkage", &device)?;
            Ok(options)
        };
        build().map(Some).map_err(|e| OCRError::ConfigError {
            message: format!("failed to enable CUDA arena shrinkage on {device}: {e}"),
        })
    }

    /// The CUDA device whose arena to shrink (the first CUDA execution
    /// provider's), or `None` when shrinkage is off or no CUDA provider is
    /// configured.
    pub(super) fn arena_shrinkage_device(common: &ModelInferenceConfig) -> Option<i32> {
        let cfg = common.ort_session.as_ref()?;
        if cfg.arena_shrinkage != Some(true) {
            return None;
        }
        cfg.execution_providers
            .as_ref()?
            .iter()
            .find_map(|ep| match ep {
                // Without the `cuda` feature no CUDA provider is registered, so
                // there is no CUDA arena to shrink.
                #[cfg(feature = "cuda")]
                crate::core::config::OrtExecutionProvider::CUDA { device_id, .. } => {
                    Some(device_id.unwrap_or(0))
                }
                _ => None,
            })
    }

    /// Whether the session holds an allocator for CUDA device `device_id`,
    /// i.e. the CUDA execution provider registered for that device.
    fn session_has_cuda_allocator(session: &ort::session::Session, device_id: i32) -> bool {
        use ort::memory::{AllocationDevice, Allocator, AllocatorType, MemoryInfo, MemoryType};
        MemoryInfo::new(
            AllocationDevice::CUDA,
            device_id,
            AllocatorType::Device,
            MemoryType::Default,
        )
        .and_then(|info| Allocator::new(session, info))
        .is_ok()
    }

    fn ensure_cuda_launch_blocking_if_needed(common: &ModelInferenceConfig) {
        use crate::core::config::OrtExecutionProvider;
        let model_name = common.model_name.as_deref().unwrap_or_default();
        let needs_formula_workaround = model_name.to_ascii_lowercase().contains("formulanet");
        if !needs_formula_workaround {
            return;
        }

        let wants_cuda = common
            .ort_session
            .as_ref()
            .and_then(|c| c.execution_providers.as_ref())
            .is_some_and(|eps| {
                eps.iter().any(|ep| {
                    matches!(
                        ep,
                        OrtExecutionProvider::CUDA { .. } | OrtExecutionProvider::TensorRT { .. }
                    )
                })
            });
        if !wants_cuda {
            return;
        }
        ensure_cuda_launch_blocking();
    }
}

/// Idempotently sets `CUDA_LAUNCH_BLOCKING=1`, respecting any value already
/// present in the environment.
///
/// MUST be called before the first CUDA session in the process is created: the
/// CUDA runtime reads this variable once, at context initialization, so setting
/// it after another CUDA session already exists has no effect. Pipelines that
/// build several CUDA models should therefore call this up front (before
/// building any adapter) rather than relying on a per-model trigger.
///
/// Works around onnxruntime#4829: PP-FormulaNet's autoregressive `Loop`
/// corrupts CUDA-EP arena buffers reused across `session.run()` calls when its
/// runs interleave with other models', producing garbage tokens. Serializing
/// CUDA launches avoids the race.
pub fn ensure_cuda_launch_blocking() {
    static SET_ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    SET_ONCE.get_or_init(|| {
        if std::env::var_os("CUDA_LAUNCH_BLOCKING").is_none() {
            // SAFETY: set_var is not thread-safe in general, but OnceLock
            // serializes us, and callers must invoke this before any CUDA work.
            unsafe { std::env::set_var("CUDA_LAUNCH_BLOCKING", "1") };
            tracing::info!(
                "set CUDA_LAUNCH_BLOCKING=1 to work around onnxruntime#4829 (PP-FormulaNet CUDA Loop race)"
            );
        }
    });
}
