#pragma once
#include "lemon/backends/backend_registry.h"
#include "lemon/backends/backend_utils.h"
#include "lemon/server_capabilities.h"
#include "lemon/wrapped_server.h"
namespace lemon::backends {
class Asr4allServer : public WrappedServer, public ITranscriptionServer {
public:
    bool observes_inference_completion() const override { return true; }
  Asr4allServer(const std::string &log_level, ModelManager *models,
                BackendManager *backends);
  void load(const std::string &, const ModelInfo &, const RecipeOptions &,
            bool = false) override;
  json audio_transcriptions(const json &request) override;
};
namespace asr4all {
std::unique_ptr<WrappedServer> create(const BackendContext &);
const BackendSpec *spec();
const BackendOps *ops();
constexpr uint32_t capabilities() {
  return capability_mask_of<Asr4allServer>();
}
} // namespace asr4all
} // namespace lemon::backends
