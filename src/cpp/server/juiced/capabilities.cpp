#include "lemon/backends/backend_registry.h"
#include "lemon/juiced/service.h"
#include "lemon/model_manager.h"
#include "lemon/router.h"
#include "lemon/server.h"
#include "lemon/version.h"
#include <algorithm>
#include <cstdlib>
#include <filesystem>

namespace lemon {
namespace {
using json = nlohmann::json;
const std::map<std::string, std::string> operation_modes = {
    {"chat", "chat.completions"}, {"embeddings", "embeddings"}, {"embedding", "embeddings"},
    {"reranking", "rerank"}, {"transcription", "audio.transcriptions"},
    {"tts", "audio.speech"}, {"image", "images.generations"},
    {"classification", "classify"}, {"audio-generation", "audio.generations"},
    {"3d", "3d.generations"}};
}

std::string JuicedService::service_version() const {
    const auto* version = std::getenv("JUICED_SERVICE_VERSION");
    return version && *version ? version : LEMON_VERSION_STRING;
}

json JuicedService::model_capabilities(const ModelInfo &info,
                                                      const json &model) {
    const auto *descriptor = backends::descriptor_for(info.recipe);
    const bool runtime_service = !is_omni_collection_recipe(info.recipe);
    json components = nullptr;
    json missing = json::array();
    if (info.downloaded && !info.resolved_paths.empty()) {
        components = true;
        if (descriptor) {
            for (const auto &role : descriptor->required_checkpoints) {
                const auto found = info.resolved_paths.find(role);
                std::error_code error;
                if (found == info.resolved_paths.end() ||
                    !std::filesystem::exists(found->second, error) || error) {
                    components = false;
                    missing.push_back(role);
                }
            }
        }
        for (const auto &[role, file] : info.resolved_paths) {
            if (file.empty()) continue;
            std::error_code error;
            if (!std::filesystem::exists(file, error) || error) {
                components = false;
                if (std::find(missing.begin(), missing.end(), json(role)) == missing.end())
                    missing.push_back(role);
            }
        }
    }
    const auto contains = [&](const std::string &label) {
        return std::find(info.labels.begin(), info.labels.end(), label) != info.labels.end();
    };
    json operations = json::array();
    if (descriptor && runtime_service) {
        for (const auto &mode : descriptor->supported_modes) {
            const auto operation = operation_modes.find(mode);
            if (operation == operation_modes.end()) continue;
            const bool selected_mode = contains(mode) ||
                (descriptor->supported_modes.size() == 1) ||
                (mode == "chat" && model.value("service", "") == "Chat");
            if (!selected_mode) continue;
            operations.push_back({{"id", operation->second},
                {"support", "declared"}, {"source", "native_backend_descriptor"},
                {"streaming", nullptr}, {"conformance", "not_recorded"}});
        }
    }
    const auto options = info.recipe_options.to_json();
    const auto backend_key = descriptor ? descriptor->backend_option_name() : "";
    json backend = !backend_key.empty() && options.contains(backend_key)
        ? options.at(backend_key) : json(nullptr);
    json compatibility = {{"compatible", nullptr}, {"reason", "Execution profile not evaluated"}};
    if (info.downloaded && descriptor && backend.is_string() &&
        backend != "auto" && runtime_service) {
        try {
            compatibility = backends::ops_for(info.recipe)->execution_compatibility(
                info, info.recipe, backend.get<std::string>());
        } catch (const std::exception &) {
            compatibility = {{"compatible", nullptr}, {"reason", "Execution profile could not be inspected"}};
        }
    }
    if (!runtime_service)
        compatibility = {{"compatible", false}, {"reason", "Agent collections are outside Juiced model services"}};
    if (components == false)
        compatibility = {{"compatible", false}, {"reason", "Required artifact components are missing"}};
    const auto state = server_.model_manager_->get_execution_profiles(info.model_name);
    return {{"schema_version", 1},
        {"model", model.at("id")},
        {"artifact", {{"architecture", info.gguf.architecture.empty() ? json(nullptr) : json(info.gguf.architecture)},
                      {"downloaded", info.downloaded},
                      {"components_complete", components}, {"missing_components", missing},
                      {"source_claims", info.labels}}},
        {"execution", {{"recipe", info.recipe}, {"requested_backend", backend},
                       {"profile", state.value("selected", "registered")},
                       {"settings_scope", "selected_profile"},
                       {"settings_pending", model.value("runtime", json::object()).value("settings_pending", false)},
                       {"compatibility", compatibility},
                       {"hardware_qualification", "not_recorded"},
                       {"performance_qualification", "not_recorded"},
                       {"worker_loaded", model.value("loaded", false)}}},
        {"runtime_service", runtime_service}, {"operations", operations},
        {"limits", {{"context_tokens", model.value("context_length", json(nullptr))},
                    {"context_scope", "model_or_configured_total_budget"},
                    {"concurrent_requests", nullptr}, {"per_request_context_tokens", nullptr}}},
        {"features", {{"tools", contains("tool-calling") ? json("declared") : json(nullptr)},
                      {"vision", contains("vision") ? json("declared") : json(nullptr)},
                      {"reasoning", contains("reasoning") ? json("declared") : json(nullptr)}}}};
}

void JuicedService::capabilities_api(const httplib::Request &req,
                                    httplib::Response &res) {
    json catalog, revision;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        catalog = catalog_;
        revision = state_["revision"];
        for (auto &model : catalog)
            model["policy"] = state_["policies"].value(model.value("id", ""), "disabled");
    }
    json models = json::array();
    for (const auto &model : catalog) {
        if (!model.value("downloaded", false) || !allows(model.value("id", ""))) continue;
        if (!model.contains("capability_contract")) continue;
        auto contract = model.at("capability_contract");
        contract["policy"] = model.at("policy");
        models.push_back(std::move(contract));
    }
    const bool anonymous = server_.api_key_.empty();
    const auto *product_version = std::getenv("JUICED_SERVICE_VERSION");
    const auto *source_revision = std::getenv("JUICED_SOURCE_REVISION");
    json result = {{"object", "juiced.capabilities"}, {"schema_version", 1},
        {"server_version", LEMON_VERSION_STRING},
        {"product_version", product_version ? json(product_version) : json(nullptr)},
        {"source_revision", source_revision ? json(source_revision) : json(nullptr)},
        {"revision", revision},
        {"models", models}, {"protocols", protocols().at("protocols")},
        {"scope", {{"agent_execution", false}, {"user_workflows", false},
                   {"interaction", "temporary_service_tests"}}},
        {"identity", {{"mode", anonymous ? "anonymous_trusted_lan" : "configured_credentials"},
                      {"credential_headers", {"Authorization: Bearer", "x-api-key"}},
                      {"operator_key_configured", !server_.admin_api_key_.empty()},
                      {"snapshot_namespace", "principal-v1"},
                      {"anonymous_authority", "shared_trusted_lan"},
                      {"session_ids_are_access_control", false},
                      {"credential_rotation", "cold_rebuild"}}}};
    res.set_header("Cache-Control", "no-store");
    res.set_content(result.dump(), "application/json");
}
} // namespace lemon
