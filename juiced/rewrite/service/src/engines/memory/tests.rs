use super::*;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

fn mib(bytes: u64) -> u64 {
    bytes / MIB
}

/// `ornith-ai/Ornith-1.5-35B-A3B-GGUF` Q6_K metadata (qwen35moe: 40 hybrid layers, one in four
/// with attention, plus one MTP block).
pub(crate) fn qwen35moe() -> ModelShape {
    ModelShape {
        block_count: Some(41),
        embedding_length: Some(2048),
        head_count: Some(16),
        head_count_kv: Some(2),
        key_length: Some(256),
        value_length: Some(256),
        full_attention_interval: Some(4),
        nextn_predict_layers: Some(1),
        ssm_conv_kernel: Some(4),
        ssm_inner_size: Some(4096),
        ssm_state_size: Some(128),
        ssm_group_count: Some(16),
        vocab_size: Some(248_320),
        ..ModelShape::default()
    }
}

fn qwen35_4b() -> ModelShape {
    ModelShape {
        block_count: Some(32),
        embedding_length: Some(2560),
        head_count_kv: Some(4),
        nextn_predict_layers: None,
        tied_embedding_bytes: Some(644 * MIB),
        ..qwen35moe()
    }
}

fn dense_llama() -> ModelShape {
    ModelShape {
        block_count: Some(54),
        embedding_length: Some(5120),
        head_count: Some(32),
        head_count_kv: Some(8),
        key_length: Some(128),
        value_length: Some(128),
        vocab_size: Some(131_072),
        ..ModelShape::default()
    }
}

fn gemma3() -> ModelShape {
    ModelShape {
        block_count: Some(34),
        embedding_length: Some(2560),
        head_count: Some(8),
        head_count_kv: Some(4),
        key_length: Some(256),
        value_length: Some(256),
        sliding_window: Some(1024),
        vocab_size: Some(262_144),
        ..ModelShape::default()
    }
}

fn gemma4_e4b() -> ModelShape {
    let pattern = (0..42).map(|layer| layer % 6 != 5).collect();
    ModelShape {
        block_count: Some(42),
        embedding_length: Some(2560),
        head_count: Some(8),
        head_count_kv: Some(2),
        key_length: Some(512),
        value_length: Some(512),
        key_length_swa: Some(256),
        value_length_swa: Some(256),
        sliding_window: Some(512),
        sliding_window_pattern: Some(pattern),
        shared_kv_layers: Some(18),
        vocab_size: Some(262_144),
        ..ModelShape::default()
    }
}

fn gpt_oss() -> ModelShape {
    ModelShape {
        block_count: Some(24),
        embedding_length: Some(2880),
        head_count: Some(64),
        head_count_kv: Some(8),
        key_length: Some(64),
        value_length: Some(64),
        sliding_window: Some(128),
        vocab_size: Some(201_088),
        ..ModelShape::default()
    }
}

fn cpu(context: u64) -> Plan {
    Plan::new(context, 1, false)
}

#[test]
fn hybrid_layers_charge_only_attention_kv_and_fixed_recurrent_state_per_slot() {
    let shape = qwen35moe();
    let breakdown = llama(&shape, Some("qwen35moe"), 0, 0, &cpu(262_144)).unwrap();
    // llama-fit-params b12032: Host context 5182 MiB (10 attention layers + 30 recurrent).
    assert_eq!(mib(breakdown.context_bytes), 5182);
    assert_eq!(
        mib(llama(&shape, Some("qwen35moe"), 0, 0, &cpu(32_768))
            .unwrap()
            .context_bytes),
        702
    );
    let four = Plan::new(262_144, 4, false);
    assert_eq!(
        mib(llama(&shape, Some("qwen35moe"), 0, 0, &four)
            .unwrap()
            .context_bytes),
        5371
    );
    let mut quantized = cpu(262_144);
    quantized.cache_type_k = CacheType::Q8_0;
    quantized.cache_type_v = CacheType::Q8_0;
    // Engine: 2785 MiB with q8_0 K and V.
    let q8 = mib(llama(&shape, Some("qwen35moe"), 0, 0, &quantized)
        .unwrap()
        .context_bytes);
    assert!((2780..=2790).contains(&q8), "{q8}");
    let small = qwen35_4b();
    assert_eq!(
        mib(llama(&small, Some("qwen35"), 0, 0, &cpu(16_384))
            .unwrap()
            .context_bytes),
        562
    );
    assert_eq!(
        mib(llama(&small, Some("qwen35"), 0, 0, &cpu(262_144))
            .unwrap()
            .context_bytes),
        8242
    );
}

#[test]
fn sliding_window_layers_are_capped_at_the_window_unless_swa_full() {
    let gemma = gemma3();
    assert_eq!(
        mib(llama(&gemma, Some("gemma3"), 0, 0, &cpu(131_072))
            .unwrap()
            .context_bytes),
        2734
    );
    assert_eq!(
        mib(llama(&gemma, Some("gemma3"), 0, 0, &cpu(8192))
            .unwrap()
            .context_bytes),
        334
    );
    let pattern = gemma4_e4b();
    assert_eq!(
        mib(llama(&pattern, Some("gemma4"), 0, 0, &cpu(131_072))
            .unwrap()
            .context_bytes),
        2088
    );
    let mut full = cpu(131_072);
    full.swa_full = true;
    assert_eq!(
        mib(llama(&pattern, Some("gemma4"), 0, 0, &full)
            .unwrap()
            .context_bytes),
        7168
    );
    assert_eq!(
        mib(llama(&gpt_oss(), Some("gpt-oss"), 0, 0, &cpu(131_072))
            .unwrap()
            .context_bytes),
        3090
    );
    let unknown = llama(&gemma, Some("unknown-swa"), 0, 0, &cpu(131_072)).unwrap();
    assert_eq!(
        mib(unknown.context_bytes),
        34 * 131_072 * 4 * 512 * 2 / 1024 / 1024
    );
}

#[test]
fn dense_models_match_the_engine_and_compute_tracks_vocabulary_and_mask() {
    let shape = dense_llama();
    let breakdown = llama(&shape, Some("llama"), 0, 0, &cpu(32_768)).unwrap();
    assert_eq!(mib(breakdown.context_bytes), 6912);
    assert_eq!(
        mib(llama(&shape, Some("llama"), 0, 0, &cpu(131_072))
            .unwrap()
            .context_bytes),
        27_648
    );
    // Engine compute: 338 MiB at 32K and 434 MiB at 128K on CPU; the estimate stays above.
    assert!((338..=400).contains(&mib(breakdown.compute_bytes)));
    let long = llama(&shape, Some("llama"), 0, 0, &cpu(131_072)).unwrap();
    assert!((434..=500).contains(&mib(long.compute_bytes)));
    let ornith = llama(&qwen35moe(), Some("qwen35moe"), 0, 0, &cpu(262_144)).unwrap();
    assert!((761..=800).contains(&mib(ornith.compute_bytes)));
    let mut gpu = Plan::new(262_144, 1, true);
    gpu.flash_attention = None;
    let device = llama(&qwen35moe(), Some("qwen35moe"), 0, 0, &gpu).unwrap();
    // Vulkan0 757 + Host 272 MiB.
    assert!((1029..=1100).contains(&mib(device.compute_bytes)));
    let mut off = Plan::new(32_768, 1, true);
    off.flash_attention = Some(false);
    assert!(
        mib(llama(&shape, Some("llama"), 0, 0, &off)
            .unwrap()
            .compute_bytes)
            >= 4206 + 108
    );
}

#[test]
fn projector_draft_prompt_cache_and_checkpoints_are_charged_beyond_the_engine_fit() {
    let shape = qwen35moe();
    let mut plan = Plan::new(262_144, 1, true);
    let plain = llama(&shape, Some("qwen35moe"), 29_208_731_392, 0, &plan).unwrap();
    assert_eq!(plain.draft_bytes, 0);
    assert_eq!(plain.projector_bytes, 0);
    // Juiced's launch prompt cache (1 GiB) plus 32 checkpoints of 62.8 MiB.
    assert_eq!(
        mib(plain.other_bytes),
        1024 + 32 * 2_195_456 * 30 / 1024 / 1024
    );
    let mut engine = plan.clone();
    engine.cache_ram_mib = None;
    let engine = llama(&shape, Some("qwen35moe"), 29_208_731_392, 0, &engine).unwrap();
    // The engine's 8 GiB default stops at one full context.
    assert_eq!(
        mib(engine.other_bytes),
        mib(engine.context_bytes) + 32 * 2_195_456 * 30 / 1024 / 1024
    );
    plan.mtp = true;
    plan.cache_ram_mib = Some(1024);
    let full = llama(
        &shape,
        Some("qwen35moe"),
        29_208_731_392,
        902_822_240,
        &plan,
    )
    .unwrap();
    assert_eq!(mib(full.draft_bytes), 512 + 256);
    assert_eq!(full.projector_bytes, 902_822_240 + 902_822_240 / 4);
    assert_eq!(
        mib(full.other_bytes),
        1024 + 32 * 30 * 2_195_456 / 1024 / 1024
    );
    let gib = full.total() as f64 / GIB;
    assert!((36.0..39.0).contains(&gib), "{gib}");
    let mut off = plan.clone();
    off.cache_ram_mib = Some(0);
    off.ctx_checkpoints = 0;
    let core = llama(&shape, Some("qwen35moe"), 29_208_731_392, 902_822_240, &off).unwrap();
    assert_eq!(core.other_bytes, 0);
    assert!((34.0..36.0).contains(&(core.total() as f64 / GIB)));
    let dense = llama(&dense_llama(), Some("llama"), 1, 0, &cpu(32_768)).unwrap();
    assert_eq!(dense.other_bytes, 1024 * MIB);
    let tied = llama(
        &qwen35_4b(),
        Some("qwen35"),
        4_482_402_656,
        0,
        &Plan::new(16_384, 1, true),
    )
    .unwrap();
    assert_eq!(tied.weights_bytes, 4_482_402_656 + 644 * MIB);
    let cpu_tied = llama(&qwen35_4b(), Some("qwen35"), 4_482_402_656, 0, &cpu(16_384)).unwrap();
    assert_eq!(cpu_tied.weights_bytes, 4_482_402_656);
}

#[test]
fn launch_arguments_select_the_memory_plan() {
    let args: Vec<String> = [
        "--model",
        "{model}",
        "-c",
        "65536",
        "--parallel=2",
        "-ctk",
        "q8_0",
        "--cache-type-v",
        "q4_0",
        "-fa",
        "on",
        "-ub",
        "256",
        "--swa-full",
        "--cache-ram",
        "0",
        "--spec-type",
        "draft-mtp",
        "-ngl",
        "0",
        "--ctx-checkpoints",
        "4",
    ]
    .map(str::to_owned)
    .into();
    let plan = Plan::from_arguments(&args, 4096, 1, true);
    assert_eq!(plan.context_tokens, 65_536);
    assert_eq!(plan.slots, 2);
    assert!(!plan.gpu);
    assert_eq!(plan.cache_type_k, CacheType::Q8_0);
    assert_eq!(plan.cache_type_v, CacheType::Q4_0);
    assert_eq!(plan.flash_attention, Some(true));
    assert_eq!(plan.ubatch, 256);
    assert!(plan.swa_full && plan.mtp);
    assert_eq!(plan.cache_ram_mib, Some(0));
    assert_eq!(plan.ctx_checkpoints, 4);
    let defaults = Plan::from_arguments(&["--jinja".to_owned()], 8192, 1, true);
    assert_eq!(defaults.cache_ram_mib, Some(1024));
    assert_eq!(defaults, Plan::new(8192, 1, true));
    assert_eq!(
        fit_arguments(&args, "/m.gguf"),
        [
            "-m",
            "/m.gguf",
            "-c",
            "65536",
            "--parallel",
            "2",
            "-ctk",
            "q8_0",
            "--cache-type-v",
            "q4_0",
            "-fa",
            "on",
            "-ub",
            "256",
            "--swa-full",
            "-ngl",
            "0",
            "--fit-print",
            "on",
        ]
    );
}

#[test]
fn fit_print_output_parses_per_device_rows_and_rejects_noise() {
    let devices = parse_fit_print("Vulkan0 26784 5182 757 \nHost 397 0 272 \n").unwrap();
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0].device, "Vulkan0");
    assert_eq!(devices[1].compute_bytes, 272 * MIB);
    assert!(parse_fit_print("").is_none());
    assert!(parse_fit_print("error: unknown argument").is_none());
    assert!(parse_fit_print("Host x 1 2").is_none());
    let estimate = Breakdown {
        weights_bytes: 29_208_731_392,
        projector_bytes: 7,
        draft_bytes: 8,
        other_bytes: 9,
        ..Breakdown::default()
    };
    let fitted = with_fit(&estimate, &devices);
    assert_eq!(fitted.weights_bytes, 29_208_731_392);
    assert_eq!(fitted.context_bytes, 5182 * MIB);
    assert_eq!(fitted.compute_bytes, (757 + 272) * MIB);
    assert_eq!(
        (
            fitted.projector_bytes,
            fitted.draft_bytes,
            fitted.other_bytes
        ),
        (7, 8, 9)
    );
}

#[test]
fn auto_context_is_the_largest_fitting_step_within_the_trained_limit() {
    let shape = qwen35moe();
    let total = |context| {
        let plan = Plan::new(context, 1, true);
        llama(&shape, Some("qwen35moe"), 29_208_731_392, 0, &plan).map(|b| b.total())
    };
    assert_eq!(auto_context(Some(262_144), u64::MAX, total), 262_144);
    let chosen = auto_context(Some(262_144), (34.0 * GIB) as u64, total);
    assert!(chosen % 1024 == 0 && chosen < 262_144 && chosen > 4096);
    assert!(total(chosen).unwrap() <= (34.0 * GIB) as u64);
    assert!(total(chosen + 1024).unwrap() > (34.0 * GIB) as u64);
    assert_eq!(auto_context(Some(262_144), 1, total), MINIMUM_AUTO_CONTEXT);
    assert_eq!(auto_context(None, u64::MAX, total), UNKNOWN_TRAINED_CAP);
    assert_eq!(auto_context(Some(2048), u64::MAX, total), 2048);
}

#[test]
fn defaults_add_metrics_cache_mtp_and_architecture_samplers_without_overriding() {
    let defaults = LlamaDefaults::for_model(&qwen35moe(), Some("qwen35moe"), 1);
    assert!(defaults.mtp);
    let mut args: Vec<String> = ["--model", "{model}", "--temperature", "0.3"]
        .map(str::to_owned)
        .into();
    defaults.apply(&mut args);
    let joined = args.join(" ");
    assert!(joined.contains("--metrics"));
    assert!(joined.contains("--spec-type draft-mtp"));
    assert!(joined.contains("--top-k 20"));
    assert!(joined.contains("--chat-template-kwargs {\"preserve_thinking\":true}"));
    assert!(!joined.contains("--temp 1.0"));
    let again = args.clone();
    defaults.apply(&mut args);
    assert_eq!(args, again);
    assert!(!LlamaDefaults::for_model(&qwen35moe(), Some("qwen35moe"), 2).mtp);
    assert!(!LlamaDefaults::for_model(&dense_llama(), Some("llama"), 1).mtp);
    assert!(
        LlamaDefaults::for_model(&dense_llama(), Some("llama"), 1)
            .architecture
            .is_empty()
    );
}

#[test]
#[ignore = "requires JUICED_TEST_GGUF_DIR with GGUF headers; prints the estimate per file"]
fn real_gguf_headers_estimate() {
    let directory = std::env::var_os("JUICED_TEST_GGUF_DIR").expect("JUICED_TEST_GGUF_DIR");
    let mut entries: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "gguf")
        })
        .collect();
    entries.sort();
    let contexts: Vec<u64> = std::env::var("JUICED_TEST_CONTEXTS")
        .unwrap_or_else(|_| "32768".into())
        .split(',')
        .map(|value| value.parse().unwrap())
        .collect();
    for path in entries {
        let mut file = std::fs::File::open(&path).unwrap();
        let bytes = file.metadata().unwrap().len();
        let Ok((declared, shape)) = crate::profiles::read_gguf_shape(&mut file) else {
            continue;
        };
        for context in &contexts {
            for gpu in [false, true] {
                let plan = Plan::new(*context, 1, gpu);
                if let Some(breakdown) =
                    llama(&shape, declared.architecture.as_deref(), bytes, 0, &plan)
                {
                    println!(
                        "{} ctx={context} gpu={gpu} weights={} context={} compute={} other={} total={}",
                        path.file_name().unwrap().to_string_lossy(),
                        mib(breakdown.weights_bytes),
                        mib(breakdown.context_bytes),
                        mib(breakdown.compute_bytes),
                        mib(breakdown.other_bytes),
                        mib(breakdown.total())
                    );
                }
            }
        }
    }
}
