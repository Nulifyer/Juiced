#include "lemon/juiced/service.h"
#include "lemon/model_manager.h"
#include "lemon/server.h"
#include "lemon/utils/path_utils.h"
#include <algorithm>
#include <chrono>
#include <cstdlib>
#include <fstream>
#include <random>
#include <regex>

namespace lemon {
namespace {
using json = nlohmann::json;
std::string configured(const char *key, const std::string &fallback) {
  const auto value = std::getenv(key);
  return value && *value ? value : fallback;
}
json read(const std::filesystem::path &file) {
  std::ifstream input(file);
  if (!input)
    return json::object();
  return json::parse(input, nullptr, false);
}
void save(const std::filesystem::path &file, const json &data) {
  const auto temp = file.string() + ".tmp";
  {
    std::ofstream out(temp);
    out << data.dump(2);
    out.flush();
    if (!out)
      throw std::runtime_error("Could not save preparation job");
  }
  std::filesystem::rename(temp, file);
}
std::string prepared_name(const std::string &source, const std::string &quant) {
  const std::regex suffix(
      "[-_](IQ[0-9][A-Z0-9_]*|Q[0-9][A-Z0-9_]*|F16|BF16|F32)$",
      std::regex::icase);
  return std::regex_replace(source, suffix, "") + "-" + quant;
}
} // namespace
JuicedService::json JuicedService::prepared_artifacts() {
  json result = json::array(),
       definitions = read(std::filesystem::path(server_.config_dir_) /
                          "user_models.json");
  const auto cache = directory_ / "prepared";
  if (!std::filesystem::exists(cache))
    return result;
  for (const auto &entry : std::filesystem::directory_iterator(cache)) {
    const auto id = entry.path().filename().string();
    if (entry.is_symlink() || !entry.is_directory() ||
        !std::regex_match(id, std::regex("[a-f0-9]{64}")))
      continue;
    const auto file = entry.path() / "model.gguf";
    const auto manifest = read(entry.path() / "manifest.json");
    if (!std::filesystem::is_regular_file(file) || !manifest.is_object())
      continue;
    json references = json::array();
    if (definitions.is_object())
      for (const auto &[name, definition] : definitions.items()) {
        for (const auto &[role, checkpoint] :
             definition.value("checkpoints", json::object()).items())
          if (checkpoint.is_string() &&
              std::filesystem::weakly_canonical(
                  checkpoint.get<std::string>()) ==
                  std::filesystem::weakly_canonical(file))
            references.push_back(name);
      }
    result.push_back(
        {{"id", id},
         {"bytes", std::filesystem::file_size(file)},
         {"quant", manifest.value("recipe", json::object()).value("quant", "")},
         {"models", references},
         {"removable", references.empty()}});
  }
  return result;
}
JuicedService::json
JuicedService::preparation_plan(const std::string &requested) {
  const auto info = server_.model_manager_->get_model_info(requested);
  const auto name =
      server_.model_manager_->get_public_model_name(info.model_name);
  auto source = std::filesystem::path(info.resolved_path());
  bool gguf = std::filesystem::is_regular_file(source) &&
              !info.gguf.architecture.empty();
  if (!gguf && !source.empty() && !std::filesystem::is_directory(source))
    source = source.parent_path();
  bool hf = std::filesystem::is_directory(source) &&
            std::filesystem::exists(source / "config.json");
  std::string reason;
  bool text_only = false;
  if (info.type != ModelType::LLM && info.type != ModelType::EMBEDDING &&
      info.type != ModelType::RERANKING)
    reason = "This preparation recipe supports language models, embeddings and "
             "rerankers";
  else if (!info.downloaded || (!gguf && !hf))
    reason = "Download a complete GGUF or original Hugging Face snapshot first";
  if (hf) {
    const auto config = read(source / "config.json");
    const std::set<std::string> architectures = {
        "llama", "qwen2", "qwen3", "qwen3_moe", "qwen3_5", "qwen3_5_moe"};
    const auto text = config.value("text_config", config);
    for (const auto &key :
         {"vision_config", "audio_config", "vision_encoder", "audio_encoder"})
      if (config.contains(key) && !config[key].is_null())
        text_only = true;
    if (!architectures.count(text.value("model_type", "")))
      reason = "No reviewed local converter for this architecture";
    if (config.contains("quantization_config") ||
        text.contains("quantization_config"))
      reason = "This recipe needs original floating-point weights; quantized "
               "checkpoint import requires a separate adapter";
  }
  const auto tools =
      configured("JUICED_PREPARATION_ROOT", "/opt/juiced/preparation");
  const auto quantizer =
      configured("JUICED_QUANTIZER", "/opt/juiced/preparation/llama-quantize");
  const bool tools_ready = std::filesystem::exists(tools + "/bin/python3") &&
                           std::filesystem::exists(tools + "/prepare.py") &&
                           std::filesystem::exists(quantizer);
  json changes = json::array();
  if (gguf)
    changes.push_back(
        "Requantizing adds numerical error. A higher target bit count cannot "
        "recover precision already lost in the downloaded weights");
  else
    changes.push_back("Floating-point weights are converted through F16; "
                      "lower-bit variants reduce precision");
  if (text_only)
    changes.push_back(
        "This is a text-only conversion: vision/audio tensors are omitted. Use "
        "a publisher GGUF with its companion file to retain those services");
  changes.push_back("Original weights and companion files are retained; a "
                    "separate model starts Disabled");
  json result = {{"model", name},
                 {"source", source.string()},
                 {"text_only", text_only},
                 {"source_format", gguf ? "gguf"
                                   : hf ? "safetensors"
                                        : "unknown"},
                 {"compatible", reason.empty()},
                 {"reason", reason},
                 {"tools_ready", tools_ready},
                 {"tool_revision", "669204ee2b9004845b34f26466a91d003edc98aa"},
                 {"changes", changes},
                 {"targets", json::array()}};
  for (const auto &quant : {"Q4_K_M", "Q5_K_M", "Q6_K", "Q8_0"})
    result["targets"].push_back(
        {{"quant", quant},
         {"name", prepared_name(name, text_only ? "Text-" + std::string(quant)
                                                : quant)},
         {"exists",
          server_.model_manager_->model_exists(prepared_name(
              name, text_only ? "Text-" + std::string(quant) : quant))},
         {"format", "gguf"},
         {"runtime", "llamacpp"}});
  return result;
}
void JuicedService::preparation_api(const httplib::Request &req,
                                    httplib::Response &res) {
  try {
    json result;
    if (req.method == "GET") {
      if (req.has_param("cache"))
        result = {{"artifacts", prepared_artifacts()}};
      else if (req.has_param("id")) {
        const auto id = req.get_param_value("id");
        {
          std::lock_guard<std::mutex> lock(mutex_);
          if (!state_["preparations"].contains(id))
            throw std::invalid_argument("Unknown preparation job");
          result["job"] = state_["preparations"][id];
        }
        std::ifstream log(directory_ / "preparations" / id / "output.log",
                          std::ios::binary);
        if (log) {
          log.seekg(0, std::ios::end);
          const std::streamoff size = log.tellg();
          log.seekg(size > std::streamoff(65536) ? size - std::streamoff(65536)
                                                 : std::streamoff(0));
          result["output"] =
              std::string(std::istreambuf_iterator<char>(log), {});
        }
      } else if (req.has_param("model"))
        result = preparation_plan(req.get_param_value("model"));
      else {
        std::lock_guard<std::mutex> lock(mutex_);
        result = {{"jobs", state_["preparations"]}};
      }
    } else {
      const auto body = json::parse(req.body);
      ResourceAdmission::Lease mutation;
      if (body.value("action", "") != "cancel")
        mutation = server_.router_->admit_management(body.value("model", body.value("id", std::string("preparation"))), "preparation-entry");
      if (body.value("action", "") == "delete_cache") {
        const auto id = body.at("id").get<std::string>();
        if (!std::regex_match(id, std::regex("[a-f0-9]{64}")))
          throw std::invalid_argument("Invalid prepared artifact identifier");
        const auto artifacts = prepared_artifacts();
        bool removable = false;
        for (const auto &artifact : artifacts)
          if (artifact["id"] == id)
            removable = artifact.value("removable", false);
        if (!removable)
          throw std::invalid_argument(
              "Remove registrations using this artifact before deleting its "
              "cached weights");
        std::lock_guard<std::mutex> lock(mutex_);
        for (const auto &[key, job] : state_["preparations"].items())
          if (job.value("pending", false))
            throw std::invalid_argument("Wait for active preparations before "
                                        "deleting cached artifacts");
        std::filesystem::remove_all(directory_ / "prepared" / id);
        result = {{"removed", true}};
      } else if (body.value("action", "") == "cancel") {
        const auto id = body.at("id").get<std::string>();
        std::lock_guard<std::mutex> lock(mutex_);
        if (!state_["preparations"].contains(id))
          throw std::invalid_argument("Unknown preparation job");
        if (state_["preparations"][id].value("pending", false))
          preparation_cancelled_.insert(id);
        result = {{"accepted", true}};
      } else {
        const auto plan = preparation_plan(body.at("model").get<std::string>());
        if (!plan.value("compatible", false))
          throw std::invalid_argument(
              plan.value("reason", "Unsupported preparation"));
        if (!plan.value("tools_ready", false))
          throw std::invalid_argument(
              "Install the Juiced preparation tool bundle first");
        if (!body.value("acknowledge", false))
          throw std::invalid_argument(
              "Review and acknowledge precision changes before preparation");
        json target;
        for (const auto &candidate : plan["targets"])
          if (candidate["quant"] == body.at("quant"))
            target = candidate;
        if (target.is_null())
          throw std::invalid_argument("Unsupported target quantization");
        const auto name = target.at("name").get<std::string>();
        if (server_.model_manager_->model_exists(name))
          throw std::invalid_argument("That exact model variant is already "
                                      "registered; select it from Models");
        const auto id =
            std::to_string(
                std::chrono::system_clock::now().time_since_epoch().count()) +
            "-" + std::to_string(std::random_device{}());
        json job = {{"id", id},
                    {"model", plan["model"]},
                    {"name", name},
                    {"quant", target["quant"]},
                    {"source", plan["source"]},
                    {"text_only", plan.value("text_only", false)},
                    {"changes", plan["changes"]},
                    {"pending", true},
                    {"writer_state", "queued"},
                    {"phase", "queued"}};
        std::lock_guard<std::mutex> lock(mutex_);
        if (preparation_pending_.size() >= 8)
          throw std::invalid_argument("Preparation queue is full");
        for (const auto &[key, existing] : state_["preparations"].items())
          if (existing.value("name", "") == name &&
              existing.value("pending", false))
            throw std::invalid_argument(
                "That variant is already being prepared");
        state_["preparations"][id] = job;
        persist_locked();
        preparation_pending_.push_back({job, std::move(mutation)});
        wake_.notify_all();
        result = job;
        res.status = 202;
      }
    }
    res.set_content(result.dump(), "application/json");
  } catch (const AdmissionError &error) {
    res.status = error.status;
    if (error.retry_after()) res.set_header("Retry-After", std::to_string(error.retry_after()));
    res.set_content(error.to_json().dump(), "application/json");
  } catch (const std::exception &error) {
    res.status = 400;
    res.set_content(json{{"error", error.what()}}.dump(), "application/json");
  }
  res.set_header("Cache-Control", "no-store");
}
void JuicedService::preparation_run() {
  while (!stopping_) {
    if (preparation_active_) {
      if (!finish_preparation_cleanup())
        std::this_thread::sleep_for(std::chrono::seconds(1));
      continue;
    }
    {
      std::unique_lock<std::mutex> lock(mutex_);
      wake_.wait_for(lock, std::chrono::milliseconds(250), [this] {
        return stopping_ || !preparation_pending_.empty();
      });
      if (stopping_)
        break;
      if (preparation_pending_.empty())
        continue;
      preparation_active_ = std::make_unique<PreparationTask>(std::move(preparation_pending_.front()));
      preparation_pending_.pop_front();
    }
    auto& task = *preparation_active_;
    const auto& job = task.job;
    const auto id = job.at("id").get<std::string>();
    const auto dir = directory_ / "preparations" / id;
    const auto cache = directory_ / "prepared";
    try {
      if (preparation_admission_barrier_) preparation_admission_barrier_();
      task.operation = server_.router_->admit_management(id, "preparation");
      {
        std::lock_guard<std::mutex> lock(mutex_);
        if (stopping_ || preparation_cancelled_.count(id))
          throw std::runtime_error(stopping_ ? "Interrupted" : "Cancelled");
      }
      auto info = server_.model_manager_->get_model_info(job.at("model"));
      json companions = json::object();
      for (const auto &[role, path] : info.resolved_paths)
        if (role != "main" &&
            !(job.value("text_only", false) && role == "mmproj"))
          companions[role] = path;
      std::filesystem::create_directories(dir);
      const auto tools =
          configured("JUICED_PREPARATION_ROOT", "/opt/juiced/preparation");
      auto command = job;
      command.update(
          {{"cache", cache.string()},
           {"companions", companions},
           {"quantizer", configured("JUICED_QUANTIZER",
                                    "/opt/juiced/preparation/llama-quantize")},
           {"converter", tools + "/source/convert_hf_to_gguf.py"},
           {"threads", 4}});
      save(dir / "job.json", command);
      task.process = preparation_process_factory_ ? preparation_process_factory_()
          : std::make_unique<NativeProcess>(ProcessOutput{false, false});
      {
        std::lock_guard<std::mutex> lock(mutex_);
        if (stopping_ || preparation_cancelled_.count(id)) throw std::runtime_error(stopping_ ? "Interrupted" : "Cancelled");
        state_["preparations"][id].update({{"writer_state", "dispatching"}, {"writer_manager", cache_run_id_}});
        persist_locked();
      }
      task.admission.started();
      task.operation.started();
      task.process->start({tools + "/bin/python3",
                     {tools + "/prepare.py", (dir / "job.json").string()},
                     {},
                     0,
                     ""});
      bool cancelled = false;
      while (task.process->running()) {
        {
          std::lock_guard<std::mutex> lock(mutex_);
          cancelled = preparation_cancelled_.count(id);
        }
        if (stopping_ || cancelled) {
          break;
        }
        const auto progress = read(dir / "progress.json");
        if (progress.is_object()) {
          std::lock_guard<std::mutex> lock(mutex_);
          state_["preparations"][id].update(progress);
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(200));
      }
      if (stopping_ || cancelled)
        throw std::runtime_error(cancelled ? "Cancelled" : "Interrupted");
      task.process->stop();
      if (!task.process->exited()) throw std::runtime_error("Preparation writer exit is unconfirmed");
      const auto result = read(dir / "result.json");
      if (!result.is_object() || !result.contains("path")) {
        const auto progress = read(dir / "progress.json");
        throw std::runtime_error(
            progress.is_object()
                ? progress.value("error", "Preparation process failed")
                : "Preparation process failed");
      }
      const auto output = result.at("path").get<std::string>();
      GgufMetadata metadata;
      if (!read_gguf_metadata(metadata, output))
        throw std::runtime_error(
            "Prepared output has no readable GGUF metadata");
      json checkpoints = companions;
      checkpoints["main"] = output;
      auto labels = info.labels;
      labels.erase(
          std::remove_if(labels.begin(), labels.end(),
                         [&](const std::string &label) {
                           return label == "hot" ||
                                  (job.value("text_only", false) &&
                                   (label == "vision" || label == "audio" ||
                                    label == "video" || label == "multimodal"));
                         }),
          labels.end());
      json definition = {
          {"recipe", "llamacpp"},
          {"source", "local_path"},
          {"checkpoints", checkpoints},
          {"labels", labels},
          {"size", std::filesystem::file_size(output) / 1e9},
          {"juiced_preparation",
           {{"source", job["model"]}, {"cache_key", result["cache_key"]}}}};
      server_.model_manager_->register_user_model(job.at("name"), definition,
                                                  "local_path", false);
      task.outcome = {{"pending", false},
                       {"phase", "complete"},
                       {"cached", result.value("cached", false)},
                       {"cache_key", result["cache_key"]},
                       {"inference_tested", false}};
    } catch (const std::exception &error) {
      std::lock_guard<std::mutex> lock(mutex_);
      task.outcome = {{"pending", false},
           {"phase", preparation_cancelled_.count(id) ? "cancelled"
                     : stopping_                      ? "interrupted"
                                                      : "failed"},
           {"error", error.what()}};
    }
    finish_preparation_cleanup();
  }
}

bool JuicedService::finish_preparation_cleanup() {
  if (!preparation_active_) return true;
  auto& task = *preparation_active_;
  const auto id = task.job.at("id").get<std::string>();
  try {
    if (task.process) {
      task.process->stop();
      if (!task.process->exited()) throw std::runtime_error("Preparation writer exit is unconfirmed");
    }
    std::filesystem::remove_all(directory_ / "prepared" / (".partial-" + id));
    {
      std::lock_guard<std::mutex> lock(mutex_);
      state_["preparations"][id].update(task.outcome);
      state_["preparations"][id].update({{"writer_state", "stopped"}, {"cleanup_pending", false}});
      persist_locked();
      preparation_cancelled_.erase(id);
    }
    task.admission.completed();
    task.operation.completed();
    preparation_active_.reset();
    return true;
  } catch (const std::exception& error) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto candidate = state_["preparations"][id];
    candidate.update({{"pending", true}, {"phase", "cleanup-pending"},
        {"cleanup_pending", true}, {"error", error.what()}});
    if (candidate != state_["preparations"][id]) {
      state_["preparations"][id] = std::move(candidate);
      try { persist_locked(); } catch (...) {}
    }
    return false;
  }
}
} // namespace lemon
