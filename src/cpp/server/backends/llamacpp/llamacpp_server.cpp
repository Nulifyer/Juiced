#include "lemon/backends/llamacpp/llamacpp_server.h"
#include "lemon/backends/llamacpp/llamacpp.h"
#include "lemon/backends/llamacpp/llamacpp_gguf.h"
#include "lemon/backends/llamacpp/llamacpp_request.h"
#include "llamacpp_system_utils.h"
#include "lemon/backends/backend_registry.h"
#include "lemon/backends/backend_ops.h"
#include "lemon/backends/backend_utils.h"
#include "lemon/gguf_capabilities.h"
#include "lemon/gguf_reader.h"
#include "lemon/model_manager.h"
#include <algorithm>
#include <climits>
#include <cmath>
#include <cctype>
#include <filesystem>
#include <regex>
#include <system_error>
#include <utility>
#include "lemon/auto_tune.h"
#include "lemon/backend_manager.h"
#include "lemon/runtime_config.h"
#include "lemon/utils/custom_args.h"
#include "lemon/utils/content_hash.h"
#include "lemon/utils/recipe_arg_resolver.h"
#include "lemon/utils/process_manager.h"
#include "lemon/utils/json_utils.h"
#include "lemon/utils/path_utils.h"
#include "lemon/error_types.h"
#include "lemon/system_info.h"
#include <algorithm>
#include <climits>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <iostream>
#include <lemon/utils/aixlog.hpp>
#include <set>
#ifdef __APPLE__
#include <pwd.h>
#include <unistd.h>
#endif

#ifdef _WIN32
    #include <windows.h>
#elif defined(__APPLE__)
    #include <mach-o/dyld.h>
    #include <limits.h>
#else
    #include <sys/stat.h>
    #include <unistd.h>
#endif

namespace fs = std::filesystem;
using namespace lemon::utils;

namespace lemon {
namespace backends {

// Helper to push reserved flags and their aliases
static void push_reserved(std::set<std::string>& reserved,
                    const std::string& key,
                    const std::vector<std::string>& aliases) {
    reserved.insert(key);
    reserved.insert(aliases.begin(), aliases.end());
}

// Helper to add a flag-only argument (e.g., --jinja, --embeddings)
static void push_arg(std::vector<std::string>& args,
                    std::set<std::string>& reserved,
                    const std::string& key,
                    const std::vector<std::string>& aliases = {}) {
    args.push_back(key);
    push_reserved(reserved, key, aliases);
}

// Helper to add a flag-value pair (e.g., --port 13305, -m model.gguf)
static void push_arg(std::vector<std::string>& args,
                    std::set<std::string>& reserved,
                    const std::string& key,
                    const std::string& value,
                    const std::vector<std::string>& aliases = {}) {
    args.push_back(key);
    args.push_back(value);
    push_reserved(reserved, key, aliases);
}

static std::string resolve_llamacpp_backend(const std::string& backend) {
    if (backend == "rocm") {
        // Map "rocm" to the appropriate channel based on config
        std::string channel = "stable";  // default to stable for now
        if (auto* cfg = RuntimeConfig::global()) {
            channel = cfg->rocm_channel();
        }
        return "rocm-" + channel;
    }
    return backend;
}

static bool is_llamacpp_rocm_backend(const std::string& backend) {
    return backend == "rocm-stable" || backend == "rocm-nightly";
}

static bool is_llamacpp_cuda_backend(const std::string& backend) {
    return backend == "cuda";
}

static bool is_dflash_draft_checkpoint(std::string checkpoint) {
    std::transform(checkpoint.begin(), checkpoint.end(), checkpoint.begin(),
                   [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    size_t separator = checkpoint.find_last_of("/:\\");
    std::string filename = separator == std::string::npos
                               ? checkpoint
                               : checkpoint.substr(separator + 1);
    return filename.rfind("dflash-", 0) == 0 || filename == "dflash.gguf";
}

static std::string resolve_llamacpp_runtime_args(const ModelInfo& model_info,
                                                 const std::string& custom_args,
                                                 bool merge_args) {
    if (!merge_args) return custom_args;

    std::vector<RuntimeArgDefault> defaults;

    const std::string draft_checkpoint = model_info.checkpoint("draft");
    const bool has_dflash_label =
        std::find(model_info.labels.begin(), model_info.labels.end(), "dflash") !=
        model_info.labels.end();
    const bool is_dflash_draft =
        !draft_checkpoint.empty() && is_dflash_draft_checkpoint(draft_checkpoint);
    const bool uses_mtp =
        std::find(model_info.labels.begin(), model_info.labels.end(), "mtp") !=
        model_info.labels.end();

    if (is_dflash_draft && has_dflash_label) {
        defaults.push_back({"--spec-type draft-dflash", "--spec-type"});
    } else if (uses_mtp) {
        defaults.push_back({"--spec-type draft-mtp", "--spec-type"});
    }

    // An auto slot count also enables the unified KV buffer, which advertises the
    // full ctx_size to every slot instead of dividing it. Pinning the count keeps
    // requests from implicitly sharing a divided context budget. Engine limits
    // can still cap the actual per-request context below the requested value.
    defaults.push_back({"--parallel 1", "--parallel", {"-np"}});

    return append_runtime_arg_defaults(custom_args, defaults);
}

static std::string trim_version_prefix(const std::string& version) {
    if (!version.empty() && version[0] == 'v') {
        return version.substr(1);
    }
    return version;
}

static std::string trim_to_major_minor(const std::string& version) {
    // Trim to MAJOR.MINOR format (e.g., "7.12.0" -> "7.12")
    std::string trimmed = trim_version_prefix(version);
    size_t second_dot = trimmed.find('.', trimmed.find('.') + 1);
    if (second_dot != std::string::npos) {
        return trimmed.substr(0, second_dot);
    }
    return trimmed;
}

static std::string get_therock_version() {
    auto config = JsonUtils::load_from_file(utils::get_resource_path("resources/backend_versions.json"));
    if (!config.contains("therock") || !config["therock"].is_object() ||
        !config["therock"].contains("version") || !config["therock"]["version"].is_string()) {
        throw std::runtime_error("backend_versions.json is missing 'therock.version'");
    }
    return trim_to_major_minor(config["therock"]["version"].get<std::string>());
}

InstallParams LlamaCppServer::get_install_params(const std::string& backend, const std::string& version) {
    InstallParams params;

    const std::string resolved_backend = resolve_llamacpp_backend(backend);

    if (resolved_backend == "hybrid-rocm") {
#ifdef __linux__
        params.repo = "Nulifyer/llama-xdna-hybrid";
        params.filename = "llama-xdna-hybrid-linux-rocm-gfx1150-x86_64.tar.gz";
        return params;
#else
        throw std::runtime_error("This hybrid bundle is available on Linux gfx1150/XDNA2 only");
#endif
    }
    if (resolved_backend == "system") {
        return params; // Return empty params for system backend
    }

    if (resolved_backend == "rocm-stable") {
        params.repo = "lemonade-sdk/llama.cpp";
        std::string therock_ver = get_therock_version();
#ifdef _WIN32
        params.filename = "llama-" + version + "-bin-win-rocm-" + therock_ver + "-x64.zip";
#elif defined(__linux__)
        params.filename = "llama-" + version + "-bin-ubuntu-rocm-" + therock_ver + "-x64.tar.gz";
#else
        throw std::runtime_error("ROCm stable llamacpp is currently supported on Windows and Linux only");
#endif
    } else if (resolved_backend == "rocm-nightly") {
        params.repo = "lemonade-sdk/llamacpp-rocm";
        std::string target_arch =
            SystemInfo::rocm_asset_family(SystemInfo::get_rocm_arch());
        if (target_arch.empty()) {
            throw std::runtime_error(
                SystemInfo::get_unsupported_backend_error("llamacpp", "rocm-nightly")
            );
        }
#ifdef _WIN32
        params.filename = "llama-" + version + "-windows-rocm-" + target_arch + "-x64.zip";
#elif defined(__linux__)
        params.filename = "llama-" + version + "-ubuntu-rocm-" + target_arch + "-x64.zip";
#else
        throw std::runtime_error("ROCm nightly llamacpp only supported on Windows and Linux");
#endif
    } else if (resolved_backend == "cuda") {
        params.repo = "lemonade-sdk/llama.cpp";
        std::string target_arch = SystemInfo::get_cuda_arch();
        if (target_arch.empty()) {
            throw std::runtime_error(
                SystemInfo::get_unsupported_backend_error("llamacpp", "cuda")
            );
        }
        // lemonade-sdk/llama.cpp releases publish per-Compute-Capability binaries
        // and embed the build tag in the asset filename, e.g.
        // llama-b1011-ubuntu-cuda-sm_120-x64.tar.xz.
#ifdef _WIN32
        params.filename = "llama-" + version + "-windows-cuda-" + target_arch + "-x64.7z";
#elif defined(__linux__)
#if defined(__aarch64__)
        params.filename = "llama-" + version + "-ubuntu-cuda-" + target_arch + "-arm64.tar.xz";
#else
        params.filename = "llama-" + version + "-ubuntu-cuda-" + target_arch + "-x64.tar.xz";
#endif
#else
        throw std::runtime_error("CUDA llamacpp is currently supported on Windows and Linux only");
#endif
    } else if (resolved_backend == "metal") {
        params.repo = "ggml-org/llama.cpp";
#ifdef __APPLE__
        params.filename = "llama-" + version + "-bin-macos-arm64.tar.gz";
#else
        throw std::runtime_error("Metal llamacpp only supported on macOS");
#endif
    } else if (resolved_backend == "cpu") {
        params.repo = "ggml-org/llama.cpp";
#ifdef _WIN32
        params.filename = "llama-" + version + "-bin-win-cpu-x64.zip";
#elif defined(__linux__)
#if defined(__aarch64__)
        params.filename = "llama-" + version + "-bin-ubuntu-arm64.tar.gz";
#else
        params.filename = "llama-" + version + "-bin-ubuntu-x64.tar.gz";
#endif
#else
        throw std::runtime_error("CPU llamacpp not supported on this platform");
#endif
    } else {  // vulkan
        params.repo = "ggml-org/llama.cpp";
#ifdef _WIN32
        params.filename = "llama-" + version + "-bin-win-vulkan-x64.zip";
#elif defined(__linux__)
#if defined(__aarch64__)
        params.filename = "llama-" + version + "-bin-ubuntu-vulkan-arm64.tar.gz";
#else
        params.filename = "llama-" + version + "-bin-ubuntu-vulkan-x64.tar.gz";
#endif
#else
        throw std::runtime_error("Vulkan llamacpp only supported on Windows and Linux");
#endif
    }

    return params;
}

LlamaCppServer::LlamaCppServer(const std::string& log_level, ModelManager* model_manager, BackendManager* backend_manager)
    : WrappedServer("llama-server", log_level, model_manager, backend_manager) {
}

json LlamaCppServer::execution_environment_inputs(const std::map<std::string, std::string>& environment, const std::string& directory) {
    const std::set<std::string> numeric = {
        "LLAMA_ARG_THREADS", "LLAMA_ARG_CTX_SIZE", "LLAMA_ARG_N_PREDICT", "LLAMA_ARG_BATCH", "LLAMA_ARG_UBATCH",
        "LLAMA_ARG_N_PARALLEL", "LLAMA_ARG_CACHE_RAM", "LLAMA_ARG_ROPE_SCALE", "LLAMA_ARG_ROPE_FREQ_BASE",
        "LLAMA_ARG_ROPE_FREQ_SCALE", "LLAMA_ARG_YARN_ORIG_CTX", "LLAMA_ARG_YARN_EXT_FACTOR", "LLAMA_ARG_YARN_ATTN_FACTOR",
        "LLAMA_ARG_YARN_BETA_SLOW", "LLAMA_ARG_YARN_BETA_FAST", "OMP_NUM_THREADS", "OMP_THREAD_LIMIT",
        "OPENBLAS_NUM_THREADS", "GOTO_NUM_THREADS", "MKL_NUM_THREADS"
    };
    const std::map<std::string, std::set<std::string>> enums = {
        {"LLAMA_ARG_FLASH_ATTN", {"on", "off", "auto", "0", "1"}},
        {"LLAMA_ARG_CACHE_TYPE_K", {"f32", "f16", "bf16", "q8_0", "q4_0", "q4_1", "iq4_nl", "q5_0", "q5_1"}},
        {"LLAMA_ARG_CACHE_TYPE_V", {"f32", "f16", "bf16", "q8_0", "q4_0", "q4_1", "iq4_nl", "q5_0", "q5_1"}},
        {"LLAMA_ARG_ROPE_SCALING_TYPE", {"none", "linear", "yarn"}},
        {"OMP_DYNAMIC", {"true", "false", "TRUE", "FALSE"}},
        {"OMP_WAIT_POLICY", {"active", "passive", "ACTIVE", "PASSIVE"}}
    };
    const std::set<std::string> files = {"LLAMA_ARG_CHAT_TEMPLATE_FILE"};
    const std::vector<std::string> relevant = {"LLAMA_", "GGML_", "GGUF_", "MTMD_", "OMP_", "GOMP_", "KMP_", "MKL_", "OPENBLAS_", "GOTO_", "CUDA_", "HIP_", "ROCR_", "HSA_", "SYCL_", "ZE_", "VK_", "LD_", "DYLD_"};
    json result = {{"scalars", json::object()}, {"files", json::object()}, {"unresolved", false}};
    static const std::regex number("[+-]?([0-9]+(\\.[0-9]*)?|\\.[0-9]+)([eE][+-]?[0-9]+)?");
    for (const auto& [key, value] : environment) {
        if (files.count(key)) {
            if (value.empty()) { result["unresolved"] = true; continue; }
            const fs::path path(value);
            result["files"][key] = (path.is_absolute() ? path : fs::path(directory) / path).lexically_normal().string();
        } else if (numeric.count(key)) {
            if (value.size() > 64 || !std::regex_match(value, number)) result["unresolved"] = true;
            else result["scalars"][key] = value;
        } else if (const auto allowed = enums.find(key); allowed != enums.end()) {
            if (!allowed->second.count(value)) result["unresolved"] = true;
            else result["scalars"][key] = value;
        } else if (key == "LD_LIBRARY_PATH") {
            result["scalars"][key] = value;
        } else if (std::any_of(relevant.begin(), relevant.end(), [&](const auto& prefix) { return key.rfind(prefix, 0) == 0; })) {
            result["unresolved"] = true;
        }
    }
    return result;
}

json LlamaCppServer::execution_argument_inputs(const std::string& arguments) {
    const auto tokens = utils::parse_custom_args(arguments);
    const auto controls = utils::build_custom_args_map(tokens);
    const std::map<std::string, size_t> scalar = {
        {"--parallel", 1}, {"-np", 1}, {"--slot-save-path", 1},
        {"--threads", 1}, {"-t", 1}, {"--threads-batch", 1}, {"-tb", 1},
        {"--batch-size", 1}, {"-b", 1}, {"--ubatch-size", 1}, {"-ub", 1},
        {"--cache-ram", 1}, {"-cram", 1}, {"--cache-type-k", 1}, {"-ctk", 1}, {"--cache-type-v", 1}, {"-ctv", 1},
        {"--flash-attn", 1}, {"-fa", 1}, {"--seed", 1}, {"-s", 1},
        {"--n-predict", 1}, {"-n", 1}, {"--rope-scaling", 1},
        {"--rope-freq-base", 1}, {"--rope-freq-scale", 1}, {"--spec-type", 1},
        {"--chat-template", 1}, {"--control-vector-layer-range", 2}
    };
    const std::set<std::string> switches = {"--mmap", "--no-mmap", "--mlock",
        "--warmup", "--no-warmup", "--context-shift", "--no-context-shift",
        "--cont-batching", "--no-cont-batching"};
    const std::set<std::string> file_flags = {"--chat-template-file", "--lora", "--lora-scaled",
        "--control-vector", "--control-vector-scaled"};
    std::map<std::string, std::string> files;
    std::set<std::string> unknown;
    if (!tokens.empty() && tokens.front().front() != '-') unknown.insert("positional-arguments");
    for (const auto& [flag, occurrences] : controls) {
        if (file_flags.count(flag)) {
            size_t index = 0;
            for (const auto& values : occurrences) {
                if (values.size() != 1) { unknown.insert(flag); continue; }
                const bool list = flag != "--chat-template-file";
                const bool scaled = flag == "--lora-scaled" || flag == "--control-vector-scaled";
                size_t begin = 0;
                do {
                    const auto end = list ? values.front().find(',', begin) : std::string::npos;
                    auto path = values.front().substr(begin, end == std::string::npos ? end : end - begin);
                    if (scaled) {
                        const auto colon = path.rfind(':');
                        bool valid = colon != std::string::npos && colon > 0 && colon + 1 < path.size();
                        if (valid) try {
                            size_t read = 0;
                            const auto scale = std::stod(path.substr(colon + 1), &read);
                            valid = read == path.size() - colon - 1 && std::isfinite(scale);
                        } catch (const std::exception&) { valid = false; }
                        if (!valid) { unknown.insert(flag); break; }
                        path.resize(colon);
                    }
                    if (path.empty() || files.size() >= 4096) { unknown.insert(flag); break; }
                    files[flag + ":" + std::to_string(index++)] = std::move(path);
                    if (end == std::string::npos) break;
                    begin = end + 1;
                } while (begin <= values.front().size());
            }
        } else {
            const auto entry = scalar.find(flag);
            const auto arity = entry == scalar.end() ? size_t(0) : entry->second;
            if (entry == scalar.end() && !switches.count(flag)) { unknown.insert(flag); continue; }
            for (const auto& values : occurrences) if (values.size() != arity) unknown.insert(flag);
        }
    }
    return {{"files", files}, {"unknown", unknown}};
}

void LlamaCppServer::load(const std::string& model_name,
                         const ModelInfo& model_info,
                         const RecipeOptions& options,
                         bool do_not_upgrade) {
    {
        std::lock_guard<std::mutex> state(state_mutex_);
        context_observation_ = json::object();
        execution_identity_ = json::object();
    }
    ResourceAdmission::Lease acquisition;
    if (model_manager_ && model_info.extra<bool>("hf_load", false))
        acquisition = model_manager_->admit_acquisition(model_name);

    LOG(INFO, "LlamaCpp") << "Loading model: " << model_name << std::endl;

    LOG(DEBUG, "LlamaCpp") << "Per-model settings: " << options.to_log_string() << std::endl;

    int ctx_size = options.get_option("ctx_size");

    std::string llamacpp_device = options.get_option("llamacpp_device");
    std::string llamacpp_backend_option = options.get_option("llamacpp_backend");
    std::string llamacpp_backend = resolve_llamacpp_backend(llamacpp_backend_option);
    std::string llamacpp_args = options.get_option("llamacpp_args");

    // Opaque CLI controls can select remote weights, another model directory or
    // an unbounded slot policy. The protected subset only qualifies the native
    // single-slot default and managed snapshot storage. Keep interpretation here.
    const auto controls = parse_custom_args(llamacpp_args);
    bool qualified_controls = controls.size() % 2 == 0;
    bool parallel = false, storage = false;
    for (size_t i = 0; qualified_controls && i < controls.size(); i += 2) {
        if ((controls[i] == "--parallel" || controls[i] == "-np") && !parallel && controls[i + 1] == "1") parallel = true;
        else if (controls[i] == "--slot-save-path" && !storage && !controls[i + 1].empty()) storage = true;
        else qualified_controls = false;
    }
    if (model_manager_ && !qualified_controls)
        acquisition = model_manager_->admit_acquisition(model_name);

    RuntimeConfig::validate_backend_choice("llamacpp", llamacpp_backend_option);

    LOG(INFO, "LlamaCpp") << "Using LlamaCpp Backend: " << llamacpp_backend << std::endl;

    bool use_gpu = (llamacpp_backend != "cpu");

    // Update device type based on the actual backend selected.
    device_type_ = effective_device(options);

    // Install llama-server if needed (use per-model backend)
    backend_manager_->install_backend(llamacpp::spec()->recipe, llamacpp_backend);

    // Use pre-resolved GGUF path. Skipped for hf_load models because llama-server
    // sources the weights itself via -hf; those models may not have local files.
    std::string gguf_path = model_info.resolved_path();
    if (gguf_path.empty() && !model_info.extra<bool>("hf_load", false)) {
        throw std::runtime_error("GGUF file not found for checkpoint: " + model_info.checkpoint());
    }

    if (!gguf_path.empty()) {
        LOG(DEBUG, "LlamaCpp") << "Using GGUF: " << gguf_path << std::endl;
    }

    // Get mmproj path for vision models and drafter path for mtp or other drafting strategies
    std::string mmproj_path = model_info.resolved_path("mmproj");
    std::string draft_path = model_info.resolved_path("draft");

    port_ = choose_port();

    std::string executable = BackendUtils::get_backend_binary_path(*llamacpp::spec(), llamacpp_backend);

    bool supports_embeddings = (model_info.type == ModelType::EMBEDDING);
    bool supports_reranking = (model_info.type == ModelType::RERANKING);

    // Build command arguments while tracking reserved flags
    std::vector<std::string> args;
    std::set<std::string> reserved_flags;

    // hf_load delegates model+mmproj resolution to llama-server's -hf flag. This
    // is required for models like Qwen2.5-Omni where the manual -m + --mmproj
    // path rejects audio content parts in /v1/chat/completions — the -hf path
    // drives the dual-clip (vision+audio) context correctly.
    if (model_info.extra<bool>("hf_load", false)) {
        push_arg(args, reserved_flags, "-hf", model_info.checkpoint(),
                 std::vector<std::string>{"--hf-repo", "-mr", "--hf-file", "-mf"});
    } else {
        push_arg(args, reserved_flags, "-m", gguf_path, std::vector<std::string>{"--model"});
    }
    push_arg(args, reserved_flags, "--ctx-size", std::to_string(ctx_size), std::vector<std::string>{"-c"});

    if (!llamacpp_device.empty() && llamacpp_backend != "hybrid-rocm") {
        BackendUtils::validate_device_backend_match(llamacpp_backend, llamacpp_device);
        push_arg(args, reserved_flags, "--device", llamacpp_device);
    }
    push_reserved(reserved_flags, "--device", std::vector<std::string>{"-dev"});

    push_arg(args, reserved_flags, "--port", std::to_string(port_));
    push_arg(args, reserved_flags, "--jinja", std::vector<std::string>{"--no-jinja"});
    push_arg(args, reserved_flags, "--metrics");

    // To detect silent CPU fallback, raise llama-server verbosity in debug so the
    // device-selection lines ("llama_prepare_model_devices: using device ...",
    // "offloaded N/N layers to GPU") are emitted. At the default threshold they are suppressed.
    if (is_debug()) {
        args.push_back("-lv");
        args.push_back("4");
    }

    LOG(DEBUG, "LlamaCpp") << "Using backend: " << llamacpp_backend << "\n"
            << "[LlamaCpp] Use GPU: " << (use_gpu ? "true" : "false") << std::endl;

    // Add mmproj file if present (for vision models). Skip when hf_load is set —
    // llama-server resolves the mmproj companion itself from the HF repo.
    if (!mmproj_path.empty() && !model_info.extra<bool>("hf_load", false)) {
        push_arg(args, reserved_flags, "--mmproj", mmproj_path);
        if (!use_gpu) {
            LOG(DEBUG, "LlamaCpp") << "Skipping mmproj argument since GPU mode is not enabled" << std::endl;
            push_arg(args, reserved_flags, "--no-mmproj-offload");
        }
    }
    push_reserved(reserved_flags, "--mmproj", std::vector<std::string>{"-mm", "-mmu", "--mmproj-url", "--no-mmproj", "--mmproj-auto", "--no-mmproj-auto"});

    const bool has_dflash_label =
        std::find(model_info.labels.begin(), model_info.labels.end(), "dflash") != model_info.labels.end();
    const bool is_dflash_draft =
        !draft_path.empty() && is_dflash_draft_checkpoint(model_info.checkpoint("draft"));
    const bool use_draft_checkpoint =
        !draft_path.empty() && (!is_dflash_draft || has_dflash_label);

    if (use_draft_checkpoint) {
        push_arg(args, reserved_flags, "--model-draft", draft_path);
    }
    push_reserved(reserved_flags, "--model-draft", std::vector<std::string>{"-md", "--spec-draft-model"});

    // Add embeddings support if the model supports it
    if (supports_embeddings) {
        LOG(INFO, "LlamaCpp") << "Model supports embeddings, adding --embeddings flag" << std::endl;
        push_arg(args, reserved_flags, "--embeddings");
    }
    push_reserved(reserved_flags, "--embeddings", std::vector<std::string>{"--embedding"});

    // Add reranking support if the model supports it
    if (supports_reranking) {
        LOG(INFO, "LlamaCpp") << "Model supports reranking, adding --reranking flag" << std::endl;
        push_arg(args, reserved_flags, "--reranking");
    }
    push_reserved(reserved_flags, "--reranking", std::vector<std::string>{"--rerank"});

    // Validate and append custom arguments
    if (!llamacpp_args.empty()) {
        std::string validation_error = validate_custom_args(llamacpp_args, reserved_flags);
        if (!validation_error.empty()) {
            throw std::invalid_argument(
                "Invalid custom llama-server arguments:\n" + validation_error
            );
        }

        LOG(DEBUG, "LlamaCpp") << "Adding custom arguments: " << llamacpp_args << std::endl;
        std::vector<std::string> custom_args_vec = parse_custom_args(llamacpp_args);
        args.insert(args.end(), custom_args_vec.begin(), custom_args_vec.end());
    }

    LOG(INFO, "LlamaCpp") << "Starting llama-server..." << std::endl;

    auto env_vars = BackendUtils::llamacpp_library_environment(executable, llamacpp_backend);
    if (llamacpp_backend == "hybrid-rocm") {
        if (!llamacpp_device.empty() && !std::regex_match(llamacpp_device, std::regex("ROCm[0-9]+")))
            throw std::invalid_argument("Hybrid requires one ROCm GPU device");
        executable = path_to_utf8(fs::path(executable).parent_path() / "hybrid-server");
        env_vars.push_back({"HYBRID_GPU_DEVICE", llamacpp_device.empty() ? "ROCm0" : llamacpp_device});
        env_vars.push_back({"HYBRID_REQUIRE_NPU", "1"});
        const int memory_mib = options.get_option("llamacpp_hybrid_memory_mib");
        const int min_prompt = options.get_option("llamacpp_hybrid_min_prompt");
        if (memory_mib < 128 || memory_mib > 16384 || min_prompt < 1024 || min_prompt > 32768)
            throw std::invalid_argument("Hybrid memory must be 128–16384 MiB and minimum prefill 1024–32768 tokens");
        env_vars.push_back({"GGML_XDNA_MAX_COPY_GB", std::to_string(memory_mib / 1024.0)});
        env_vars.push_back({"GGML_XDNA_MIN_BATCH", std::to_string(min_prompt)});
    }
#ifdef _WIN32
    // For ROCm on Windows with gfx1151, set OCL_SET_SVMSIZE
    // This is a patch to enable loading larger models
    if (is_llamacpp_rocm_backend(llamacpp_backend)) {
        std::string new_path;

        if (llamacpp_backend == "rocm-stable") {
            std::string rocm_arch = SystemInfo::get_rocm_arch();
            if (!rocm_arch.empty()) {
                // Prepend ALL TheRock runtime dirs (not just _rocm_sdk_core/bin) so
                // HIP + BLAS DLLs resolve; _rocm_sdk_core/bin alone → STATUS_DLL_NOT_FOUND.
                std::string therock_dirs = BackendUtils::join_runtime_dirs(
                    BackendUtils::get_therock_lib_paths(rocm_arch));
                if (!therock_dirs.empty()) {
                    new_path = therock_dirs;
                }
            }
        }

        if (!new_path.empty()) {
            const char* existing_path = std::getenv("PATH");
            if (existing_path && strlen(existing_path) > 0) {
                new_path += ";" + std::string(existing_path);
            }
            env_vars.push_back({"PATH", new_path});
        }

        // Windows DLL search order checks System32 BEFORE PATH, so a stale
        // System32 amdhip64_7.dll shadows the TheRock runtime on PATH. Copying
        // to the exe directory overrides both.
        if (llamacpp_backend == "rocm-stable") {
            std::string rocm_arch = SystemInfo::get_rocm_arch();
            if (!rocm_arch.empty()) {
                BackendUtils::stage_therock_hip_runtime(
                    rocm_arch, fs::path(executable).parent_path());
            }
        }

        std::string arch = lemon::SystemInfo::get_rocm_arch();
        if ((arch == "gfx1151") || (arch == "gfx1152")){
            env_vars.push_back({"OCL_SET_SVM_SIZE", "262144"});
            LOG(DEBUG, "LlamaCpp") << "Setting OCL_SET_SVM_SIZE=262144 for gfx1151/gfx1152 (enables loading larger models)" << std::endl;
        }
    } else if (is_llamacpp_cuda_backend(llamacpp_backend)) {
        // CUDA Windows builds bundle cudart64_*.dll, cublas64_*.dll, etc. next to
        // llama-server.exe. Prepend the executable directory to PATH so the loader
        // resolves them before any system-wide CUDA install.
        fs::path exe_dir = fs::absolute(fs::path(executable)).parent_path();
        std::string new_path = path_to_utf8(exe_dir);

        const char* existing_path = std::getenv("PATH");
        if (existing_path && strlen(existing_path) > 0) {
            new_path += ";" + std::string(existing_path);
        }
        env_vars.push_back({"PATH", new_path});
        LOG(DEBUG, "LlamaCpp") << "Prepending CUDA exe dir to PATH: " << path_to_utf8(exe_dir) << std::endl;
    }
#endif

    if (is_llamacpp_cuda_backend(llamacpp_backend)) {
        std::string existing_llama_device = utils::get_environment_variable_utf8("LLAMA_ARG_DEVICE");
        const bool has_llama_device_override = !existing_llama_device.empty();

        bool skip_visible_devices = false;
        if (!llamacpp_device.empty()) {
            LOG(INFO, "LlamaCpp")
                << "Using explicit llama.cpp CUDA device selection: " << llamacpp_device
                << std::endl;
            skip_visible_devices = true;
        } else if (has_llama_device_override) {
            LOG(INFO, "LlamaCpp")
                << "Respecting existing LLAMA_ARG_DEVICE=" << existing_llama_device
                << std::endl;
            skip_visible_devices = true;
        }
        BackendUtils::apply_cuda_env_vars(env_vars, "LlamaCpp", skip_visible_devices);
    }

#ifdef __APPLE__
    // Forward GGML_METAL_NO_RESIDENCY to llama-server if set in the parent
    // environment. Metal residency sets crash on paravirtualized GPUs (e.g.
    // GitHub Actions macOS runners with MTLGPUFamilyApple5).
    const char* no_residency = std::getenv("GGML_METAL_NO_RESIDENCY");
    if (no_residency) {
        env_vars.push_back({"GGML_METAL_NO_RESIDENCY", no_residency});
        LOG(DEBUG, "LlamaCpp") << "Forwarding GGML_METAL_NO_RESIDENCY=" << no_residency << std::endl;
    }

    // Ensure HOME is set in the child. llama.cpp b8884+ (libllama-common's
    // fs_get_cache_directory / hf_cache::migrate_old_cache_to_hf_cache)
    // calls getenv("HOME") during CLI arg parsing and passes the result
    // straight into std::string without a NULL check, segfaulting when
    // HOME is unset. LaunchDaemons installed at /Library/LaunchDaemons/
    // get a minimal env from launchd and do not inherit HOME, so llama-server
    // crashes before the model ever loads. Terminal/sudo spawns preserve
    // HOME and do not hit this.
    //
    // Upstream fix in flight: https://github.com/ggml-org/llama.cpp/pull/22263
    // Once that PR merges and lemonade's pinned llama.cpp version (in
    // src/cpp/resources/backend_versions.json) includes it, this HOME
    // fallback can be deleted.
    const char* home = std::getenv("HOME");
    if (!home || home[0] == '\0') {
        struct passwd* pw = getpwuid(getuid());
        std::string fallback_home = (pw && pw->pw_dir) ? pw->pw_dir : "/var/root";
        env_vars.push_back({"HOME", fallback_home});
        LOG(DEBUG, "LlamaCpp") << "Parent HOME unset; setting child HOME=" << fallback_home << std::endl;
    }
#endif

    // Start process (inherit output if debug logging enabled, filter health check spam)
    // Keep llama-server output visible at info log level.
    std::string process_executable = executable;
    std::string working_dir;
#ifdef _WIN32
    // Avoid inheriting a protected launcher cwd while keeping Linux/macOS behavior unchanged.
    fs::path executable_path = fs::absolute(fs::path(executable));
    process_executable = path_to_utf8(executable_path);
    working_dir = path_to_utf8(executable_path.parent_path());
#endif

    ServerCommand command;
    command.program = process_executable;
    command.args = std::move(args);
    command.env = std::move(env_vars);
    command.port = port_;

    const auto identity_deadline = std::chrono::steady_clock::now() + std::chrono::seconds(120);
    const auto identity_cancelled = [&] {
        return load_cancelled() || current_request_cancel_context().cancelled() ||
            std::chrono::steady_clock::now() >= identity_deadline;
    };
    const auto argument_inputs = execution_argument_inputs(llamacpp_args);
    const auto argument_files = argument_inputs.at("files").get<std::map<std::string, std::string>>();
    json identity = {{"schema", 4}, {"recipe", model_info.recipe}, {"captured", false},
        {"scope", "declared-argument-process-image-and-initial-environment"},
        {"unresolved", json::array({"nonexecutable-runtime-assets", "mapped-page-immutability", "physical-device-identity", "runtime-environment-mutation"})}};
    const auto executable_identity = [&] {
        return ModelManager::execution_files({{"executable", process_executable}}, identity_cancelled).at("executable").at(0);
    };
    try {
        if (!argument_inputs.at("unknown").empty())
            throw std::runtime_error("Unresolved execution argument inputs: " + argument_inputs.at("unknown").dump());
        identity.update(ModelManager::execution_inputs(model_info, identity_cancelled));
        identity["launch_executable"] = executable_identity();
        identity["argument_files"] = ModelManager::execution_files(argument_files, identity_cancelled);
        identity["captured"] = true;
    } catch (const std::exception& error) {
        if (load_cancelled() || current_request_cancel_context().cancelled())
            throw AdmissionError("request_cancelled", "Execution identity capture was cancelled", 499);
        identity["reason"] = error.what();
    }
    bool inherit_llama_output = (log_level_ == "info") || is_debug();
    start_server(
        std::make_unique<NativeProcess>(ProcessOutput{inherit_llama_output, true}, working_dir),
        command);
    std::optional<std::string> observed_template;
    // Inspect once while startup still owns this generation. Discovery reads the
    // resulting snapshot and never sends per-model network probes under Router locks.
    try {
        const auto response = utils::HttpClient::get("http://127.0.0.1:" + std::to_string(port_) + "/props", {}, 2,
            utils::HttpSecurityPolicy::TrustedLoopback, current_request_cancel_context(), 1024 * 1024);
        if (response.status_code == 200) {
            const auto props = json::parse(response.body);
            if (props.contains("chat_template") && props.at("chat_template").is_string())
                observed_template = utils::string_sha256(props.at("chat_template").get_ref<const std::string&>());
            const auto positive = [](const json& value, uint64_t maximum) -> json {
                if (!value.is_number_integer() || (value.is_number_integer() && !value.is_number_unsigned() && value.get<int64_t>() <= 0) ||
                    value.get<uint64_t>() == 0 || value.get<uint64_t>() > maximum) return nullptr;
                return value;
            };
            const auto settings = props.value("default_generation_settings", json::object());
            const auto per_request = positive(settings.value("n_ctx", json()), uint64_t(INT64_MAX));
            const auto slots = positive(props.value("total_slots", json()), 4096);
            json observation = {{"source", "llamacpp:/props"}, {"per_request_tokens", per_request}, {"slots", slots},
                {"observed_at_ms", std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::system_clock::now().time_since_epoch()).count()}};
            if (get_model_type() == ModelType::LLM) {
                try {
                    const auto response = utils::HttpClient::get("http://127.0.0.1:" + std::to_string(port_) + "/slots", {}, 2,
                        utils::HttpSecurityPolicy::TrustedLoopback, current_request_cancel_context(), 1024 * 1024);
                    const auto inventory = json::parse(response.body);
                    std::set<int> ids;
                    bool valid = response.status_code == 200 && inventory.is_array() &&
                        !inventory.empty() && inventory.size() <= 4096 &&
                        (slots.is_null() || inventory.size() == slots.get<size_t>());
                    if (valid) for (const auto& slot : inventory) {
                        if (!slot.is_object() || !slot.contains("id") || !slot.at("id").is_number_integer() ||
                            slot.at("id") < 0 || slot.at("id") > INT_MAX || !ids.insert(slot.at("id").get<int>()).second) {
                            valid = false;
                            break;
                        }
                    }
                    if (valid) {
                        observation["slot_ids"] = ids;
                        observation["slots"] = ids.size();
                        observation["slot_source"] = "llamacpp:/slots";
                    } else observation["slots"] = nullptr;
                } catch (const std::exception&) {
                    if (current_request_cancel_context().cancelled()) throw;
                    LOG(WARNING, "LlamaCpp") << "Execution slot inventory is unavailable" << std::endl;
                }
            }
            const auto build = props.value("build_info", json());
            if (build.is_string() && build.get_ref<const std::string&>().size() <= 256) observation["engine_build"] = build;
            std::lock_guard<std::mutex> state(state_mutex_);
            context_observation_ = std::move(observation);
        }
    } catch (const std::exception&) {
        if (current_request_cancel_context().cancelled())
            throw AdmissionError("request_cancelled", "Model load was cancelled during context inspection", 499);
        LOG(WARNING, "LlamaCpp") << "Runtime context observation is unavailable; discovery will label configured values" << std::endl;
    }
    if (identity.value("captured", false)) {
        try {
            identity["runtime_image"] = capture_runtime_inputs(identity_cancelled);
            const auto [environment, directory] = runtime_environment();
            auto inputs = execution_environment_inputs(environment, directory);
            if (inputs.at("unresolved").get<bool>())
                throw std::runtime_error("Unresolved backend execution environment inputs");
            inputs.erase("unresolved");
            inputs["source"] = "owned-process-initial-exec-environment";
            inputs["files"] = ModelManager::execution_files(inputs.at("files").get<std::map<std::string, std::string>>(), identity_cancelled);
            if (inputs.at("files").contains("LLAMA_ARG_CHAT_TEMPLATE_FILE")) {
                const auto& facts = inputs.at("files").at("LLAMA_ARG_CHAT_TEMPLATE_FILE");
                if (!observed_template || facts.size() != 1 || facts.at(0).at("sha256") != *observed_template)
                    throw std::runtime_error("Environment template content does not match the observed applied template");
            }
            if (!runtime_inputs_current(identity_cancelled))
                throw std::runtime_error("Runtime environment changed during content capture");
            identity["environment"] = std::move(inputs);
            const auto verified = ModelManager::execution_inputs(model_info, identity_cancelled);
            if (identity.at("files") != verified.at("files") || identity.at("launch_executable") != executable_identity() ||
                identity.at("argument_files") != ModelManager::execution_files(argument_files, identity_cancelled))
                throw AdmissionError("execution_inputs_changed", "Loaded input content changed during backend startup", 409);
        } catch (const AdmissionError&) { throw; }
        catch (const std::exception& error) {
            if (load_cancelled() || current_request_cancel_context().cancelled())
                throw AdmissionError("request_cancelled", "Execution identity verification was cancelled", 499);
            identity["captured"] = false;
            identity["reason"] = error.what();
        }
    }
    {
        std::lock_guard<std::mutex> state(state_mutex_);
        identity["engine"] = context_observation_.value("engine_build", std::string("unknown"));
        identity["context"] = context_observation_;
        identity["context"].erase("observed_at_ms");
        identity["device_type"] = device_type_to_string(device_type_);
        execution_identity_ = std::move(identity);
    }

}

json LlamaCppServer::normalize_response_model(json response, const json& request) const {
    if (response.is_object() && response.contains("model")) {
        response["model"] = request.value("model", get_model_name());
    }
    return response;
}

RequestEnvelope LlamaCppServer::resolve_request(InferenceOperation operation, const json& request, bool protected_mode) const {
    if (operation == InferenceOperation::State)
        return WrappedServer::resolve_request(operation, request, protected_mode);
    if (protected_mode && !((get_model_type() == ModelType::LLM &&
            (operation == InferenceOperation::Chat || operation == InferenceOperation::Completion || operation == InferenceOperation::Responses)) ||
            (get_model_type() == ModelType::EMBEDDING && operation == InferenceOperation::Embedding) ||
            (get_model_type() == ModelType::RERANKING && operation == InferenceOperation::Rerank)))
        throw AdmissionError("workload_unqualified", "This model/provider operation has no qualified request envelope");
    return llamacpp::resolve_request(operation, request, protected_mode);
}

json LlamaCppServer::chat_completion(const json& request) {
    return normalize_response_model(
        forward_request("/v1/chat/completions",
                        llamacpp::sanitize_tool_schema_limits(
                            JsonUtils::with_legacy_max_tokens_alias(request))),
        request);
}

json LlamaCppServer::completion(const json& request) {
    return normalize_response_model(
        forward_request("/v1/completions",
                        JsonUtils::with_legacy_max_tokens_alias(request)),
        request);
}

json LlamaCppServer::embeddings(const json& request) {
    return forward_request("/v1/embeddings", request);
}

json LlamaCppServer::reranking(const json& request) {
    return forward_request("/v1/rerank", request);
}

json LlamaCppServer::get_slots() {
    return forward_get_request("/slots");
}

json LlamaCppServer::slots_action(int slot_id, const std::string& action, const json& request_body) {
    return forward_request("/slots/" + std::to_string(slot_id) + "?action=" + action, request_body);
}

json LlamaCppServer::tokenize(const json& request_body) {
    return forward_request("/tokenize", request_body);
}

json LlamaCppServer::count_anthropic_tokens(const json& request) {
    return forward_request("/v1/messages/count_tokens", request);
}

json LlamaCppServer::responses(const json& request) {
    return normalize_response_model(
        forward_request("/v1/responses",
                        llamacpp::sanitize_tool_schema_limits(request)),
        request);
}

void LlamaCppServer::forward_streaming_request(const std::string& endpoint,
                                               const std::string& request_body,
                                               httplib::DataSink& sink,
                                               bool sse,
                                               long timeout_seconds,
                                               TelemetryCallback telemetry_callback) {
    std::string body = request_body;
    if (endpoint == "/v1/chat/completions" || endpoint == "/v1/responses") {
        json request = json::parse(request_body, nullptr, false);
        if (!request.is_discarded()) {
            if (endpoint == "/v1/chat/completions") {
                JsonUtils::add_legacy_max_tokens_alias(request);
            }
            body = llamacpp::sanitize_tool_schema_limits(std::move(request)).dump();
        }
    }

    WrappedServer::forward_streaming_request(
        endpoint, body, sink, sse, timeout_seconds, telemetry_callback);
}

} // namespace backends
} // namespace lemon

namespace lemon {
namespace backends {
namespace llamacpp {

std::unique_ptr<WrappedServer> create(const BackendContext& ctx) {
    return make_server<LlamaCppServer>(ctx);
}

namespace {
std::string system_llamacpp_version() {
    std::string output;
    #ifdef _WIN32
    std::string command = "llama-server --version 2>NUL";
    int rc = lemon::utils::ProcessManager::run_command(command, output);
    #else
    FILE* pipe = popen("llama-server --version 2>/dev/null", "r");
    if (!pipe) {
        return "unknown";
    }

    char buffer[256];
    if (fgets(buffer, sizeof(buffer), pipe) != nullptr) {
        output = buffer;
    }

    pclose(pipe);
    #endif

    // Parse version from output like "version: 3432 (e2b2a632)" or "llama.cpp version b3432"
    if (!output.empty()) {
        // Try to find a version number
        std::regex version_regex(R"(version:\s*(\d+)|version\s+b?(\d+))");
        std::smatch match;
        if (std::regex_search(output, match, version_regex)) {
            for (size_t i = 1; i < match.size(); ++i) {
                if (match[i].matched) {
                    return "b" + match[i].str();
                }
            }
        }
        return "detected";
    }

    return "unknown";
}


bool is_ggml_hip_plugin_available() {
#ifdef __linux__
    // Allow distros/packagers that install outside the FHS paths below
    // (e.g. NixOS, custom prefixes) to point directly at libggml-hip.so.
    if (detail::ggml_hip_env_override_available()) {
        return true;
    }

    // A self-built llama.cpp resolved from PATH ships libggml-hip.so next to
    // the binary (build tree) or in a sibling lib/ directory (installed tree).
    // find_executable_in_path() returns the bare name on POSIX, so walk PATH
    // here to recover the directory the binary actually lives in.
    if (const char* path_env = std::getenv("PATH"); path_env && *path_env) {
        if (detail::ggml_hip_plugin_near_llama_server(path_env)) {
            return true;
        }
    }

    // On Linux x86_64, check common system library paths for the HIP plugin
    std::vector<std::string> possible_paths = {
        // Debian/Ubuntu multiarch path (most common)
        "/usr/lib/x86_64-linux-gnu/ggml/backends0/libggml-hip.so",
        // Arch package paths
        "/usr/lib/libggml-hip.so",
        "/usr/lib/ggml/libggml-hip.so",
        "/usr/lib64/ggml/libggml-hip.so",
        // Standard Linux paths
        "/usr/lib/ggml/backends0/libggml-hip.so",
        "/usr/lib64/ggml/backends0/libggml-hip.so"
    };

    for (const auto& path : possible_paths) {
        if (fs::exists(path)) {
            return true;
        }
    }
#endif

    return false;
}


// llamacpp model-management behavior: GGUF metadata + capability labels.
class LlamaCppOps : public BackendOps {
public:
    json execution_compatibility(const ModelInfo& info, const std::string&, const std::string& backend) const override {
        const bool gguf = !info.gguf.architecture.empty() &&
            (info.type == ModelType::LLM || info.type == ModelType::EMBEDDING || info.type == ModelType::RERANKING);
        json result = {{"compatible", gguf}, {"reuse", true},
                {"reason", gguf ? "Reuse the downloaded GGUF without changing its weights" : "Requires a compatible GGUF and architecture metadata"},
                {"changes", json::array()}};
        if (backend == "hybrid-rocm") {
            const std::set<std::string> architectures = {"llama", "qwen2", "qwen3", "qwen3moe", "qwen35", "qwen35moe"};
            if (!architectures.count(info.gguf.architecture)) {
                result["compatible"] = false;
                result["reason"] = "This matched hybrid engine has no reviewed importer for the architecture";
            }
            result["changes"] = json::array({"Eligible NPU prefill uses reduced-precision arithmetic; GPU weights and decode retain the source quantization", "Partial prefill only: routed experts, attention and decode stay on ROCm"});
        }
        return result;
    }

    void resolve_runtime_options(const ModelInfo& info, RecipeOptions& options) const override {
        const auto context = options.get_option("ctx_size");
        if (info.type == ModelType::EMBEDDING && context.is_number_integer() &&
            context.get<int64_t>() >= 0 && context.get<int64_t>() < EMBEDDING_CTX_SIZE)
            options.set_option("ctx_size", EMBEDDING_CTX_SIZE);
        const json merge_args_value = options.get_option("merge_args");
        const bool merge_args =
            merge_args_value.is_boolean() ? merge_args_value.get<bool>() : true;
        const json custom_args_value = options.get_option("llamacpp_args");
        const std::string custom_args =
            custom_args_value.is_string() ? custom_args_value.get<std::string>() : "";
        options.set_option(
            "llamacpp_args",
            resolve_llamacpp_runtime_args(info, custom_args, merge_args));
    }

    void populate_metadata(ModelInfo& info, const BackendOpsContext&) const override {
        const std::string gguf_path = info.resolved_path();
        if (gguf_path.size() < 5) {
            return;
        }
        std::string ext = gguf_path.substr(gguf_path.size() - 5);
        std::transform(ext.begin(), ext.end(), ext.begin(), ::tolower);
        if (ext != ".gguf") {
            return;
        }
        std::error_code ec;
        if (!std::filesystem::exists(lemon::utils::path_from_utf8(gguf_path), ec)) {
            return;
        }
        GgufMetadata meta;
        if (!read_gguf_metadata(meta, gguf_path)) {
            return;
        }
        info.max_context_window = meta.context_length;
        info.gguf = std::move(meta);
        // GGUF vision/tool metadata are LLM capabilities. Don't apply them to
        // embedding/reranking models, or labels like tool-calling would
        // reclassify the model away from its endpoint type.
        if (info.type == ModelType::LLM) {
            apply_gguf_capability_labels(info.labels, info.gguf.caps);
        }
    }

    std::string resolve_checkpoint_path(const ModelInfo& info,
                                        const CheckpointResolveContext& ctx) const override {
        // The main checkpoint is a GGUF file (with sharding/variant resolution);
        // auxiliary checkpoints (mmproj, …) use the shared default.
        if (ctx.type == "main") {
            return resolve_gguf_path(ctx.model_cache_path, ctx.variant);
        }
        return BackendOps::resolve_checkpoint_path(info, ctx);
    }

    std::optional<std::map<std::string, std::string>> argument_file_inputs(const RecipeOptions& options) const override {
        const auto settings = options.to_resolved_json();
        const auto parsed = LlamaCppServer::execution_argument_inputs(settings.value("llamacpp_args", std::string()));
        if (!parsed.at("unknown").empty()) return std::nullopt;
        auto files = parsed.at("files").get<std::map<std::string, std::string>>();
        const auto environment_template = utils::get_environment_variable_utf8("LLAMA_ARG_CHAT_TEMPLATE_FILE");
        if (!environment_template.empty()) files["LLAMA_ARG_CHAT_TEMPLATE_FILE"] = environment_template;
        return files;
    }

    std::string find_imported_checkpoint(const std::string& import_dir) const override {
        // The primary artifact is the (non-mmproj) GGUF file.
        return resolve_gguf_path(import_dir, "");
    }

    std::string validate_registration_checkpoint(const std::string& checkpoint) const override {
        // A GGUF checkpoint must name its quant via CHECKPOINT:VARIANT.
        std::string lower = checkpoint;
        std::transform(lower.begin(), lower.end(), lower.begin(), ::tolower);
        if (lower.find("gguf") != std::string::npos &&
            checkpoint.find(':') == std::string::npos) {
            return "You are required to provide a 'variant' in the checkpoint field when "
                   "registering a GGUF model. The variant is provided as CHECKPOINT:VARIANT. "
                   "For example: Qwen/Qwen2.5-Coder-3B-Instruct-GGUF:Q4_0 or "
                   "Qwen/Qwen2.5-Coder-3B-Instruct-GGUF:qwen2.5-coder-3b-instruct-q4_0.gguf";
        }
        return "";
    }

    std::string validate_checkpoint_file(const std::string& resolved_path) const override {
        // A .gguf file in the cache must start with the GGUF magic, else it's a
        // truncated/corrupt download and the model is not really present.
        std::error_code ec;
        std::filesystem::path p = lemon::utils::path_from_utf8(resolved_path);
        if (std::filesystem::is_directory(p, ec)) {
            return "";
        }
        std::string ext = resolved_path.size() >= 5 ? resolved_path.substr(resolved_path.size() - 5) : "";
        std::transform(ext.begin(), ext.end(), ext.begin(), ::tolower);
        if (ext != ".gguf") {
            return "";
        }
        std::ifstream in(p, std::ios::binary);
        char magic[4] = {};
        in.read(magic, sizeof(magic));
        bool ok = in.gcount() == static_cast<std::streamsize>(sizeof(magic)) &&
                  magic[0] == 'G' && magic[1] == 'G' && magic[2] == 'U' && magic[3] == 'F';
        return ok ? "" : "Invalid GGUF cache file";
    }

    std::string resolve_version(const std::string& backend,
                                const std::string& file_version) const override {
        // The PATH-installed "system" llama-server has no version.txt; query it.
        if (backend == "system") {
            return system_llamacpp_version();
        }
        return file_version;
    }

    InstallCheck check_install(const std::string& backend, bool binary_found) const override {
        // The system llama-server also needs the ggml HIP plugin for ROCm GPU
        // acceleration when an AMD GPU (KFD) is present.
        if (binary_found && backend == "system") {
#ifdef __linux__
            if (std::filesystem::exists("/sys/class/kfd") && !is_ggml_hip_plugin_available()) {
                return {false, "HIP plugin libggml-hip.so not installed"};
            }
#endif
        }
        return {binary_found, ""};
    }
};
}  // namespace

const BackendSpec* spec() { return make_spec<LlamaCppServer>(descriptor); }
const BackendOps* ops() { return single_ops<LlamaCppOps>(); }
}  // namespace llamacpp
}  // namespace backends
}  // namespace lemon
