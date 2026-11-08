#include "lemon/backends/asr4all/asr4all_server.h"
#include "lemon/backends/asr4all/asr4all.h"
#include "lemon/error_types.h"
#include "lemon/model_manager.h"
#include "lemon/utils/http_client.h"
#include "lemon/utils/process_manager.h"
#include <filesystem>

namespace lemon::backends {
Asr4allServer::Asr4allServer(const std::string &level, ModelManager *models,
                             BackendManager *backends)
    : WrappedServer("futo-asr-server", level, models, backends) {}

void Asr4allServer::load(const std::string &name, const ModelInfo &model,
                         const RecipeOptions &options, bool) {
  const auto executable =
      BackendUtils::get_backend_binary_path(*asr4all::spec(), "cpu");
  if (executable.empty() || !std::filesystem::is_regular_file(executable))
    throw std::runtime_error(
        "Set asr4all.cpu_bin to the preinstalled FUTO server directory");
  if (!std::filesystem::is_regular_file(model.resolved_path()))
    throw std::runtime_error("FUTO GGUF checkpoint is missing");
  device_type_ = DEVICE_CPU;
  port_ = utils::ProcessManager::find_free_port(8001);
  if (!port_)
    throw std::runtime_error("No free FUTO backend port");
  ServerCommand command;
  command.program = executable;
  command.args = {"--model",       model.resolved_path(),
                  "--port",        std::to_string(port_),
                  "--threads",     options.get_option("asr4all_threads"),
                  "--futo-commit", options.get_option("asr4all_commit"),
                  "--futo-right",  options.get_option("asr4all_right")};
  command.port = port_;
  start_server(
      std::make_unique<NativeProcess>(ProcessOutput{is_debug(), false}),
      command);
}

json Asr4allServer::audio_transcriptions(const json &request) {
  if (!request.contains("file_data") ||
      request.at("file_data").get<std::string>().empty())
    throw std::runtime_error("Missing audio data");
  std::vector<utils::MultipartField> fields;
  utils::MultipartField file;
  file.name = "file";
  file.data = request.at("file_data").get<std::string>();
  file.filename = "audio.wav";
  file.content_type = "audio/wav";
  fields.push_back(std::move(file));
  for (const auto *key : {"language", "response_format", "prompt"}) {
    if (!request.contains(key))
      continue;
    utils::MultipartField field;
    field.name = key;
    field.data = request.at(key).get<std::string>();
    fields.push_back(std::move(field));
  }
  const auto cancel = current_request_cancel_context();
  if (cancel.cancelled())
    return {{"error", {{"message", "Transcription cancelled before dispatch"}, {"type", "request_cancelled"}}}};
  BackendRequestScope transfer(*this, BackendRequestKind::NonStreaming);
  auto response = utils::HttpClient::post_multipart(
      "http://127.0.0.1:" + std::to_string(port_) + "/inference", fields, 180,
      utils::HttpSecurityPolicy::TrustedLoopback, cancel);
  note_request_completed();
  if (response.status_code != 200)
    return json{{"error",
                 {{"message", "FUTO transcription failed: " + response.body},
                  {"type", "audio_processing_error"},
                  {"status_code", response.status_code}}}};
  if (request.value("response_format", "json") == "text")
    return json{{"text", response.body}};
  return json::parse(response.body);
}
namespace asr4all {
std::unique_ptr<WrappedServer> create(const BackendContext &ctx) {
  return make_server<Asr4allServer>(ctx);
}
const BackendSpec *spec() {
  static const BackendSpec spec("asr4all", "futo-asr-server");
  return &spec; // Preinstalled binary override, no release asset
                // auto-installer.
}
const BackendOps *ops() { return default_backend_ops(); }
} // namespace asr4all
} // namespace lemon::backends
