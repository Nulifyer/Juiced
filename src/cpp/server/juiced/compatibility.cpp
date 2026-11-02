#include "lemon/juiced/service.h"
#include "lemon/model_manager.h"
#include "lemon/router.h"
#include "lemon/server.h"
#include "lemon/utils/http_client.h"
#include <chrono>
#include <cstdlib>
#include <stdexcept>

namespace lemon {
namespace {
using json = nlohmann::json;
std::string environment(const char *key, const std::string &fallback = "") {
  const auto *value = std::getenv(key);
  return value ? value : fallback;
}
long long seconds() {
  return std::chrono::duration_cast<std::chrono::seconds>(
             std::chrono::steady_clock::now().time_since_epoch())
      .count();
}
void respond(httplib::Response &res, const json &data, int status = 200) {
  res.status = status;
  res.set_content(data.dump(), "application/json");
  res.set_header("Cache-Control", "no-store");
}
} // namespace

json JuicedService::protocols() {
  json voice;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    voice = voice_status_;
  }
  return {
      {"object", "juiced.protocols"},
      {"protocols",
       json::array(
           {{{"id", "openai"},
             {"base_path", "/v1"},
             {"status", "implemented_subset"},
             {"features",
              {"models", "chat.completions", "responses", "embeddings",
               "audio.transcriptions", "audio.speech", "images.generations"}},
             {"limits",
              {"Features depend on the selected model and backend.",
               "Persistent snapshots require qualified Chat Completions "
               "profiles; other chat protocols require Memory mode.",
               "Provider-hosted tools and stored conversations are not "
               "implemented."}}},
            {{"id", "ollama"},
             {"base_path", ""},
             {"status", "implemented_subset"},
             {"features",
              {"tags", "show", "ps", "chat", "generate", "embed",
               "embeddings"}},
             {"limits",
              {"create, copy, push and blob upload are not implemented.",
               "Use Juiced management for model acquisition and deletion."}}},
            {{"id", "anthropic"},
             {"base_path", ""},
             {"status", "implemented_subset"},
             {"features", {"messages", "messages.count_tokens"}},
             {"limits",
              {"Messages token counting requires a supported backend "
               "(llama.cpp b12032).",
               "Provider-hosted tools are not implemented."}}},
            {{"id", "automatic1111"},
             {"base_path", ""},
             {"status", "implemented_subset"},
             {"features",
              {"sd-models", "options", "txt2img", "samplers", "schedulers",
               "progress"}},
             {"limits",
              {"sd-cpp deployments only; explicit model selection is required "
               "when ambiguous.",
               "img2img, scripts, file-based LoRAs, hires and global interrupt "
               "are not exposed."}}},
            {{"id", "wyoming"},
             {"transport", "tcp"},
             {"status", "optional_listener"},
             {"listener", voice},
             {"features", {"describe", "transcribe", "synthesize"}},
             {"limits",
              {"Private network only; Wyoming does not provide authentication.",
               "English PCM speech through the same Juiced speech workers; "
               "voice changes refresh after listener restart."}}}})}};
}

void JuicedService::compatibility_routes(httplib::Server &http) {
  for (const std::string prefix : {"/api/v0/", "/api/v1/", "/v0/", "/v1/"}) {
    http.Get(prefix + "juiced/protocols",
             [this](const auto &, auto &res) { respond(res, protocols()); });
  }
  for (const std::string op :
       {"sd-models", "options", "samplers", "schedulers", "progress",
        "upscalers", "sd-vae", "latent-upscale-modes"}) {
    http.Get("/sdapi/v1/" + op, [this, op](const auto &req, auto &res) {
      image_compatibility(req, res, op);
    });
  }
  for (const std::string op : {"txt2img", "options", "interrupt", "img2img"}) {
    http.Post("/sdapi/v1/" + op, [this, op](const auto &req, auto &res) {
      image_compatibility(req, res, op);
    });
  }
}

std::string JuicedService::image_model(const std::string &requested) {
  std::string selected = requested;
  if (selected.empty()) {
    std::lock_guard<std::mutex> lock(mutex_);
    selected = state_.value("image_model", "");
  }
  const auto models = server_.model_manager_->get_downloaded_models();
  if (!selected.empty()) {
    if (!server_.model_manager_->model_exists(selected))
      throw std::invalid_argument("Unknown image model");
    const auto resolved = server_.model_manager_->resolve_model_name(selected);
    const auto info = server_.model_manager_->get_model_info(resolved);
    const auto name = server_.model_manager_->get_public_model_name(resolved);
    if (!info.downloaded || info.recipe != "sd-cpp" || !allows(name))
      throw std::invalid_argument("Select an enabled, downloaded sd-cpp model");
    return name;
  }
  for (const auto &[id, info] : models) {
    if (info.recipe != "sd-cpp" || !allows(id))
      continue;
    if (!selected.empty())
      throw std::invalid_argument("Multiple image models are enabled; supply "
                                  "model or select sd_model_checkpoint");
    selected = server_.model_manager_->get_public_model_name(id);
  }
  if (selected.empty())
    throw std::invalid_argument("Enable an image model first");
  return selected;
}

void JuicedService::image_compatibility(const httplib::Request &req,
                                        httplib::Response &res,
                                        const std::string &op) {
  try {
    auto body = req.body.empty() ? json::object() : json::parse(req.body);
    if (!body.is_object())
      throw std::invalid_argument("Expected a JSON object");
    if (op == "sd-models") {
      json data = json::array();
      for (const auto &[id, info] :
           server_.model_manager_->get_downloaded_models()) {
        if (info.recipe != "sd-cpp" || !allows(id))
          continue;
        const auto name = server_.model_manager_->get_public_model_name(id);
        data.push_back({{"title", name},
                        {"model_name", name},
                        {"hash", nullptr},
                        {"sha256", nullptr},
                        {"filename", name},
                        {"config", nullptr}});
      }
      respond(res, data);
      return;
    }
    if (op == "interrupt" || op == "img2img") {
      respond(res,
              {{"error",
                "This operation is not supported by Juiced's SD API subset"}},
              501);
      return;
    }
    if (op == "upscalers" || op == "sd-vae" || op == "latent-upscale-modes") {
      respond(res, json::array());
      return;
    }
    if (op == "options" && req.method == "POST") {
      if (body.size() != 1 || !body.contains("sd_model_checkpoint") ||
          !body["sd_model_checkpoint"].is_string())
        throw std::invalid_argument(
            "Only sd_model_checkpoint may be changed through this interface");
      const auto selected =
          image_model(body.at("sd_model_checkpoint").get<std::string>());
      {
        std::lock_guard<std::mutex> lock(mutex_);
        state_["image_model"] = selected;
        persist_locked();
      }
      respond(res, nullptr);
      return;
    }
    std::string requested = body.value("model", req.get_param_value("model"));
    if (body.contains("override_settings")) {
      const auto &overrides = body["override_settings"];
      if (!overrides.is_object())
        throw std::invalid_argument("override_settings must be an object");
      if (overrides.contains("sd_model_checkpoint")) {
        const auto checkpoint =
            overrides.at("sd_model_checkpoint").get<std::string>();
        if (!requested.empty() && requested != checkpoint)
          throw std::invalid_argument("Conflicting model selections");
        requested = checkpoint;
      }
      if (overrides.size() >
          (overrides.contains("sd_model_checkpoint") ? 1u : 0u))
        throw std::invalid_argument("Unsupported override_settings field");
    }
    const auto model = image_model(requested);
    if (op == "options") {
      respond(res, {{"sd_model_checkpoint", model}});
      return;
    }
    if (op == "txt2img") {
      const std::set<std::string> supported = {
          "model",
          "prompt",
          "negative_prompt",
          "width",
          "height",
          "steps",
          "cfg_scale",
          "seed",
          "batch_size",
          "sampler_name",
          "scheduler",
          "clip_skip",
          "override_settings",
          "override_settings_restore_afterwards",
          "n_iter",
          "send_images",
          "save_images",
          "enable_hr",
          "alwayson_scripts"};
      for (const auto &[key, value] : body.items())
        if (!supported.count(key))
          throw std::invalid_argument("Unsupported SD API field: " + key);
      if (body.value("n_iter", 1) != 1 || body.value("enable_hr", false) ||
          body.value("save_images", false) ||
          !body.value("send_images", true) ||
          !body.value("alwayson_scripts", json::object()).empty())
        throw std::invalid_argument(
            "Only one iteration with returned images and no "
            "scripts/hires/server-side saving is supported");
      if (!body.contains("prompt") || !body["prompt"].is_string() ||
          body["prompt"].get<std::string>().empty())
        throw std::invalid_argument("prompt is required");
      if (body.value("batch_size", 1) < 1 || body.value("batch_size", 1) > 4)
        throw std::invalid_argument("batch_size must be between 1 and 4");
      for (const auto &key : {"width", "height"})
        if (body.contains(key) &&
            (!body[key].is_number_integer() || body[key].get<int>() < 64 ||
             body[key].get<int>() > 4096))
          throw std::invalid_argument(
              "Image dimensions must be integers between 64 and 4096");
      for (const auto &key :
           {"override_settings", "override_settings_restore_afterwards",
            "n_iter", "send_images", "save_images", "enable_hr",
            "alwayson_scripts"})
        body.erase(key);
      body["model"] = model;
      server_.auto_load_model_if_needed(model, json::object());
      auto result = server_.router_->image_sdapi(body);
      respond(res, result, result.contains("error") ? 502 : 200);
      return;
    }
    std::string address;
    for (const auto &loaded : server_.router_->get_all_loaded_models())
      if (loaded.value("model_name", "") == model)
        address = loaded.value("backend_url", "");
    if (address.empty()) {
      if (op == "progress") {
        respond(res, {{"progress", 0.0},
                      {"eta_relative", 0},
                      {"state", {{"job_count", 0}, {"job_no", 0}}},
                      {"current_image", nullptr},
                      {"textinfo", "Model is unloaded"}});
      } else
        respond(
            res,
            {{"error",
              "Load the selected image model to discover its backend options"}},
            409);
      return;
    }
    if (address.size() >= 3 && address.substr(address.size() - 3) == "/v1")
      address.resize(address.size() - 3);
    const auto response = utils::HttpClient::get(
        address + "/sdapi/v1/" + op +
            (op == "progress" ? "?skip_current_image=true" : ""),
        {}, 10, utils::HttpSecurityPolicy::TrustedLoopback);
    respond(res, json::parse(response.body), response.status_code);
  } catch (const json::exception &) {
    respond(res, {{"error", "Invalid SD API request"}}, 400);
  } catch (const std::invalid_argument &e) {
    respond(res, {{"error", e.what()}}, 409);
  } catch (const std::exception &e) {
    respond(res, {{"error", e.what()}}, 502);
  }
}

void JuicedService::reconcile_voice() {
  const bool enabled = environment("JUICED_WYOMING_ENABLED", "false") == "true";
  const auto executable = environment("JUICED_VOICE_PYTHON");
  const auto script = environment("JUICED_VOICE_SCRIPT");
  const auto uri = environment("JUICED_WYOMING_URI", "tcp://0.0.0.0:10300");
  json models = json::object({{"stt", json::array()}, {"tts", json::array()}});
  {
    std::lock_guard<std::mutex> lock(mutex_);
    for (const auto &m : catalog_) {
      const auto name = m.value("id", "");
      if (!m.value("downloaded", false) ||
          state_["policies"].value(name, "disabled") == "disabled" ||
          m.value("available", true) == false)
        continue;
      if (m.value("service", "") == "Transcribe")
        models["stt"].push_back(name);
      if (m.value("recipe", "") == "kokoro")
        models["tts"].push_back(name);
    }
  }
  const auto signature = models.dump();
  const bool configured = enabled && !executable.empty() && !script.empty() &&
                          (!models["stt"].empty() || !models["tts"].empty());
  if (!configured || signature != voice_signature_) {
    voice_process_.reset();
    voice_signature_ = signature;
    voice_retry_after_ = 0;
  }
  if (configured && (!voice_process_ || !voice_process_->running()) &&
      seconds() >= voice_retry_after_) {
    voice_process_.reset();
    voice_retry_after_ = seconds() + 15;
    try {
      auto process =
          std::make_unique<NativeProcess>(ProcessOutput{true, false});
      ServerCommand command;
      command.program = executable;
      command.args = {
          "-u",
          script,
          "--url",
          "http://127.0.0.1:" + std::to_string(server_.config_->port()) + "/v1",
          "--uri",
          uri,
          "--models",
          signature};
      command.env = {{"STT_OPENAI_KEY", server_.api_key_},
                     {"TTS_OPENAI_KEY", server_.api_key_}};
      process->start(command);
      voice_process_ = std::move(process);
    } catch (const std::exception &) {
    }
  }
  std::lock_guard<std::mutex> lock(mutex_);
  voice_status_ = {
      {"enabled", enabled},
      {"configured", configured},
      {"process_running", voice_process_ && voice_process_->running()},
      {"uri", uri},
      {"models", models},
      {"implementation", "wyoming_openai 0.8.0"}};
}
} // namespace lemon
