#include "lemon/backends/backend_utils.h"
#include "lemon/juiced/service.h"
#include "lemon/utils/process_manager.h"
#include <regex>

namespace lemon {
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
               backend != "metal") {
      res.status = 400;
      result["error"] =
          "Select an installed CPU, Vulkan, ROCm, CUDA or Metal backend.";
    } else {
      const auto *spec = backends::try_get_spec_for_recipe(recipe);
      if (!spec)
        throw std::runtime_error("Runtime descriptor is unavailable");
      const auto binary =
          backends::BackendUtils::get_backend_binary_path(*spec, backend);
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
          "", 8, true,
          backends::BackendUtils::llamacpp_library_environment(binary, backend));
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
