#pragma once

#include "lemon/backends/backend_registry.h"

#include "lemon/wrapped_server.h"
#include "lemon/backends/backend_utils.h"
#include <string>

namespace lemon {
namespace backends {

class LlamaCppServer : public WrappedServer, public IEmbeddingsServer, public IRerankingServer, public ISlotsServer, public ITokenizerServer {
public:
    static InstallParams get_install_params(const std::string& backend, const std::string& version);


    LlamaCppServer(const std::string& log_level,
                   ModelManager* model_manager,
                   BackendManager* backend_manager);

    void load(const std::string& model_name,
             const ModelInfo& model_info,
             const RecipeOptions& options,
             bool do_not_upgrade = false) override;

    DeviceType effective_device(const RecipeOptions& options) const override {
        const std::string backend = options.get_option("llamacpp_backend");
        return backend == "hybrid-rocm" ? DEVICE_GPU | DEVICE_NPU
             : backend == "cpu" ? DEVICE_CPU : DEVICE_GPU;
    }
    SlotPolicy effective_slot_policy(const RecipeOptions& options) const override {
        const std::string backend = options.get_option("llamacpp_backend");
        return backend == "hybrid-rocm" ? SlotPolicy::ExclusiveNpu : SlotPolicy::Standard;
    }

    // ICompletionServer implementation
    json chat_completion(const json& request) override;
    json completion(const json& request) override;
    json responses(const json& request) override;

    void forward_streaming_request(const std::string& endpoint,
                                   const std::string& request_body,
                                   httplib::DataSink& sink,
                                   bool sse = true,
                                   long timeout_seconds = 0,
                                   TelemetryCallback telemetry_callback = nullptr) override;

    // IEmbeddingsServer implementation
    json embeddings(const json& request) override;

    // IRerankingServer implementation
    json reranking(const json& request) override;

    // ISlotsServer implementation
    json get_slots() override;
    json slots_action(int slot_id, const std::string& action, const json& request_body) override;

    // ITokenizerServer implementation
    json tokenize(const json& request) override;
    json count_anthropic_tokens(const json& request) override;

private:
    // llama-server echoes the local .gguf path it was launched with (`-m <path>`)
    // in the OpenAI `model` field. Rewrite it to the client-facing model id so
    // responses don't leak absolute filesystem paths (and usernames).
    json normalize_response_model(json response, const json& request) const;
};

namespace llamacpp {
// Factory for the llamacpp backend (constructs the server class — lemond only).
std::unique_ptr<WrappedServer> create(const BackendContext& ctx);
const BackendSpec* spec();
const BackendOps* ops();
constexpr uint32_t capabilities() { return capability_mask_of<LlamaCppServer>(); }
}  // namespace llamacpp
}  // namespace backends
}  // namespace lemon
