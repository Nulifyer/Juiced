#include "lemon/backends/backend_ops.h"
#include "lemon/backends/backend_registry.h"
#include "lemon/backends/backend_utils.h"
#include "lemon/juiced/service.h"
#include "lemon/model_manager.h"
#include "lemon/router.h"
#include "lemon/server.h"
#include "lemon/system_info.h"
#include "lemon/utils/process_manager.h"
#include <algorithm>
#include <regex>

namespace lemon {
JuicedService::json
JuicedService::execution_plan(const std::string &requested) {
  auto info = server_.model_manager_->get_model_info(requested);
  const auto name =
      server_.model_manager_->get_public_model_name(info.model_name);
  const auto state = server_.model_manager_->get_execution_profiles(name);
  const auto inventory = SystemInfoCache::get_system_info_with_cache();
  json result = {{"model", name},
                 {"selected", state.value("selected", "registered")},
                 {"architecture", info.gguf.architecture},
                 {"profiles", json::array()},

                 {"source_preserved", true}};
  auto source = info;
  source.recipe = info.extra<std::string>("source_recipe", info.recipe);
  const auto original_options =
      state.value("profiles", json::object())
          .value("registered", json::object())
          .value("options", info.recipe_options.to_json());
  const auto *source_descriptor = backends::descriptor_for(source.recipe);
  const auto original_backend = source_descriptor ? original_options.value(source_descriptor->backend_option_name(), "") : "";
  result["profiles"].push_back({{"id", "registered"},
                                {"recipe", source.recipe},
                                {"original_backend", original_backend},
                                {"label", "Original setup"},
                                {"compatible", true},
                                {"available", true},
                                {"reuse", true},
                                {"changes", json::array()}});
  std::set<std::string> offered;
  for (const auto &registration : backends::all_registrations()) {
    const auto &descriptor = *registration.descriptor;
    if (descriptor.recipe != source.recipe &&
        (info.type != ModelType::LLM ||
         std::find(descriptor.supported_modes.begin(),
                   descriptor.supported_modes.end(),
                   "chat") == descriptor.supported_modes.end()))
      continue;
    if (descriptor.support.empty())
      continue;
    for (const auto &variant : descriptor.support) {
      if (!offered.insert(descriptor.recipe + ":" + variant.backend).second)
        continue;
      const auto compatibility = registration.ops->execution_compatibility(
          source, descriptor.recipe, variant.backend);
      const auto recipe_inventory =
          inventory.value("recipes", json::object())
              .value(descriptor.recipe, json::object());
      const auto status = recipe_inventory.value("backends", json::object())
                              .value(variant.backend, json::object());
      const auto package_state = status.value("state", "unsupported");
      json target = compatibility;
      const auto id = descriptor.recipe + ":" + variant.backend;
      const auto stored =
          state.value("profiles", json::object()).value(id, json::object());
      target["settings"] = state.value("selected", "registered") == id
                               ? info.recipe_options.to_json()
                               : stored.value("options", json::object());
      target["tokenizer"] = stored.value("tokenizer", "");
      target.update(
          {{"id", descriptor.recipe + ":" + variant.backend},
           {"recipe", descriptor.recipe},
           {"backend", variant.backend},
           {"label", descriptor.display_name},
           {"package_state", package_state},
           {"version", status.value("version", "")},
           {"available", package_state != "unsupported" &&
                             package_state != "action_required"},
           {"hardware_reason", status.value("message", "")},
           {"experimental", variant.tier != BackendTier::Core ||
                                compatibility.value("experimental", false)}});
      result["profiles"].push_back(target);
    }
  }
  return result;
}

void JuicedService::execution_api(const httplib::Request &req,
                                  httplib::Response &res) {
  try {
    const auto body =
        req.method == "POST" ? json::parse(req.body) : json::object();
    const auto requested = req.method == "POST"
                               ? body.at("model").get<std::string>()
                               : req.get_param_value("model");
    auto plan = execution_plan(requested);
    if (req.method != "POST") {
      res.set_content(plan.dump(), "application/json");
    } else {
      const auto id = body.at("profile").get<std::string>();
      const auto name = plan.at("model").get<std::string>();
      json target;
      for (const auto &profile : plan["profiles"])
        if (profile["id"] == id)
          target = profile;
      if (target.is_null())
        throw std::invalid_argument("Unknown execution profile");
      if (!target.value("compatible", false))
        throw std::invalid_argument(
            target.value("reason", "Incompatible execution profile"));
      if (!target.value("available", false))
        throw std::invalid_argument(
            target.value("hardware_reason", "Runtime is unavailable"));
      if ((!target.value("changes", json::array()).empty() ||
           target.value("experimental", false)) &&
          !body.value("acknowledge", false))
        throw std::invalid_argument("Review and acknowledge the experimental "
                                    "profile and arithmetic changes");
      auto info = server_.model_manager_->get_model_info(name);
      if (!info.downloaded)
        throw std::invalid_argument(
            "Download this model before selecting an execution profile");
      const bool loaded = server_.router_->is_model_loaded(name);
      if (loaded && !body.value("reload", false))
        throw std::invalid_argument(
            "Reload is required to change the active execution profile");
      auto profiles = server_.model_manager_->get_execution_profiles(name);
      auto profile =
          profiles.value("profiles", json::object()).value(id, json::object());
      if (profiles.value("selected", "registered") == id)
        profile["options"] = info.recipe_options.to_json();
      profile["id"] = id;
      profile["recipe"] = target.at("recipe");
      if (profile.empty() || !profile.contains("options")) {
        profile["options"] = json::object();
        const auto current = info.recipe_options.to_json();
        for (const auto &key :
             {"ctx_size", "pinned", "auto_evict", "evict_idle_timeout"})
          if (current.contains(key))
            profile["options"][key] = current[key];
        if (info.recipe == target["recipe"]) profile["options"] = current;
        if (target["recipe"] == "llamacpp") {
          profile["options"]["llamacpp_backend"] = target["backend"];
          profile["options"].erase("llamacpp_device");
          profile["options"]["llamacpp_args"] =
              current.value("llamacpp_args", "") +
              (target["backend"] == "cpu" ? " -ngl 0" : " -ngl 999");
        }
      }
      if (target.value("backend", "") == "hybrid-rocm") {
        const int memory_mib = body.value(
            "hybrid_memory_mib",
            profile["options"].value("llamacpp_hybrid_memory_mib", 2048));
        const int min_prompt = body.value(
            "hybrid_min_prompt",
            profile["options"].value("llamacpp_hybrid_min_prompt", 1024));
        if (memory_mib < 128 || memory_mib > 16384 || min_prompt < 1024 ||
            min_prompt > 32768)
          throw std::invalid_argument("Hybrid memory must be 128–16384 MiB and "
                                      "minimum prefill 1024–32768 tokens");
        profile["options"]["llamacpp_hybrid_memory_mib"] = memory_mib;
        profile["options"]["llamacpp_hybrid_min_prompt"] = min_prompt;
      }
      if (const auto *descriptor =
              backends::descriptor_for(target.at("recipe"));
          descriptor && descriptor->selectable_backend &&
          target.contains("backend"))
        profile["options"][descriptor->backend_option_name()] = target["backend"];
      if (target["recipe"] == "llamacpp" && target.contains("backend") &&
          body.contains("device")) {
        const auto device = body.at("device").get<std::string>();
        if (!device.empty()) {
          if (target["backend"] == "hybrid-rocm") {
            if (!std::regex_match(device, std::regex("ROCm[0-9]+")))
              throw std::invalid_argument("Hybrid requires one ROCm device");
          } else
            backends::BackendUtils::validate_device_backend_match(
                target.at("backend").get<std::string>(), device);
        }
        profile["options"]["llamacpp_device"] = device;
      }
      if (target.value("requires_tokenizer", false)) {
        auto tokenizer =
            body.value("tokenizer", profile.value("tokenizer", ""));
        if (tokenizer.empty() || tokenizer.size() > 200 ||
            tokenizer.find_first_not_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKL"
                                        "MNOPQRSTUVWXYZ0123456789._-/") !=
                std::string::npos)
          throw std::invalid_argument(
              "Specify the original tokenizer/config Hugging Face repository "
              "or local directory");
        profile["tokenizer"] = tokenizer;
        profile["gguf_import"] = true;
      }
      enqueue({{"model", name},
               {"action", loaded ? "reload" : "select"},
               {"profile", profile},
               {"expected_selected",
                body.value("expected_selected",
                           plan.value("selected", "registered"))}});
      res.status = 202;
      res.set_content(json{{"accepted", true}, {"profile", id}}.dump(),
                      "application/json");
    }
  } catch (const std::exception &e) {
    res.status = 400;
    res.set_content(json{{"error", e.what()}}.dump(), "application/json");
  }
  res.set_header("Cache-Control", "no-store");
}

void JuicedService::execution_devices(const httplib::Request &req,
                                      httplib::Response &res) {
  json result = {{"devices", json::array()}};
  const auto recipe = req.get_param_value("recipe");
  const auto backend = req.get_param_value("backend");
  try {
    if (recipe != "llamacpp") {
      res.status = 501;
      result["error"] = "This runtime does not expose accelerator device "
                        "enumeration; use its automatic selection.";
    } else if (backend == "cpu") {
      result["devices"].push_back({{"id", "none"}, {"name", "CPU"}});
    } else if (backend != "vulkan" && backend != "rocm" && backend != "cuda" &&
               backend != "metal" && backend != "hybrid-rocm") {
      res.status = 400;
      result["error"] =
          "Select an installed CPU, Vulkan, ROCm, CUDA or Metal backend.";
    } else {
      const auto *spec = backends::try_get_spec_for_recipe(recipe);
      if (!spec)
        throw std::runtime_error("Runtime descriptor is unavailable");
      auto binary =
          backends::BackendUtils::get_backend_binary_path(*spec, backend);
      auto environment =
          backends::BackendUtils::llamacpp_library_environment(binary, backend);
      if (backend == "hybrid-rocm") {
        binary = (std::filesystem::path(binary).parent_path() / "hybrid-server")
                     .string();
        environment.push_back({"HYBRID_GPU_DEVICE", "ROCm0"});
        environment.push_back({"GGML_XDNA_HOST_ONLY", "1"});
      }
      const std::regex device(
          R"(^\s*((?:Vulkan|ROCm|CUDA|Metal)[0-9]+):\s*(.+)$)");
      const int status = utils::ProcessManager::run_process_with_output(
          binary, {"--list-devices"},
          [&](const std::string &line) {
            std::smatch match;
            if (std::regex_match(line, match, device))
              result["devices"].push_back(
                  {{"id", match[1].str()}, {"name", match[2].str()}});
            return true;
          },
          "", 8, true, environment);
      if (status != 0)
        throw std::runtime_error(
            "The installed runtime could not enumerate devices. Check the "
            "runtime logs and library dependencies.");
    }
  } catch (const std::exception &e) {
    res.status = 503;
    result["error"] = e.what();
  }
  res.set_header("Cache-Control", "no-store");
  res.set_content(result.dump(), "application/json");
}
} // namespace lemon
