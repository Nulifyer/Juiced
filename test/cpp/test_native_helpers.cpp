#include "lemon/model_manager.h"
#include "lemon/backend_manager.h"
#include "lemon/backends/backend_utils.h"
#include "lemon/backends/fastflowlm/fastflowlm_server.h"
#include "lemon/backends/fastflowlm/fastflowlm_models.h"
#include "lemon/config_file.h"
#include "lemon/runtime_config.h"
#include "lemon/utils/path_utils.h"
#include "lemon/utils/json_utils.h"
#include "support/test_state_isolation.h"
#include <atomic>
#include <chrono>
#include <cstdio>
#include <fstream>
#include <future>
#include <thread>
#ifdef _WIN32
#include <windows.h>
#endif

namespace lemon::utils {
struct JsonPersistenceTestHook {
    static void save(const json& body, const std::filesystem::path& target,
                     std::function<void(const std::filesystem::path&)> before_replace) {
        JsonUtils::save_to_file(body, path_to_utf8(target), std::move(before_replace));
    }
};
}
namespace lemon {
struct ModelPublicationTestHook {
    static void storage_view(ModelManager& models, const std::string& checkpoint) {
        models.get_supported_models();
        std::lock_guard<std::mutex> cache(models.models_cache_mutex_);
        ModelInfo info; info.model_name = "FLM-Storage-Observation"; info.recipe = "flm";
        info.checkpoints["main"] = checkpoint; info.downloaded = true; info.labels = {"chat"};
        models.models_cache_[info.model_name] = std::move(info); models.rebuild_public_model_aliases_locked();
    }
    static void registry_save(ModelManager& models, std::function<void(const json&)> save) { models.user_models_save_override_ = std::move(save); }
    static void removal(ModelManager& models, std::function<void(const std::string&)> barrier) { models.removal_publication_barrier_ = std::move(barrier); }
    static void stop_barrier(ModelManager& models, std::function<void()> callback) {
        models.flm_helper_stop_barrier_ = std::move(callback);
    }
    static bool exited(ModelManager& models) { return models.flm_helper_ && models.flm_helper_->process && models.flm_helper_->process->exited(); }
    static int pid(ModelManager& models) {
        return models.flm_helper_ ? models.flm_helper_->process->handle().pid : 0;
    }
};
}
using namespace lemon;
namespace fs = std::filesystem;
static int checks = 0, failures = 0;
static void check(bool ok, const char* message) {
    ++checks; if (!ok) ++failures;
    std::fprintf(stderr, "%s: %s\n", ok ? "PASS" : "FAIL", message);
}
class DenyReplacement {
public:
    explicit DenyReplacement(const fs::path& target) : directory_(target.parent_path()) {
#ifdef _WIN32
        handle_ = CreateFileW(target.c_str(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE,
            nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
        if (handle_ == INVALID_HANDLE_VALUE) throw std::runtime_error("Cannot hold helper journal fixture");
#else
        permissions_ = fs::status(directory_).permissions();
        fs::permissions(directory_, fs::perms::owner_read | fs::perms::owner_exec);
#endif
    }
    ~DenyReplacement() {
#ifdef _WIN32
        CloseHandle(handle_);
#else
        fs::permissions(directory_, permissions_);
#endif
    }
private:
    fs::path directory_;
#ifdef _WIN32
    HANDLE handle_;
#else
    fs::perms permissions_;
#endif
};

class HelperGate {
public:
    std::promise<void> entered, released;
    std::shared_future<void> go = released.get_future().share();
    std::atomic<bool> signaled{false}, opened{false};
    void pause() { if (!signaled.exchange(true)) entered.set_value(); go.wait(); }
    bool await() { const bool ready = entered.get_future().wait_for(std::chrono::seconds(10)) == std::future_status::ready; if (!ready) open(); return ready; }
    void open() { if (!opened.exchange(true)) released.set_value(); }
    ~HelperGate() { open(); }
};

static void deletion_publication(const std::string& executable) {
    const auto root = test::make_scratch_dir("flm-deletion");
    const auto registry = fs::path(utils::get_config_dir()) / "user_models.json";
    const auto counter = root / "removals";
    const auto old_bin = utils::get_environment_variable_utf8("LEMONADE_FLM_NPU_BIN");
    const auto old_counter = utils::get_environment_variable_utf8("JUICED_TEST_FLM_COUNT");
    utils::set_environment_variable_utf8("LEMONADE_FLM_NPU_BIN", executable);
    utils::set_environment_variable_utf8("JUICED_TEST_FLM_COUNT", counter.u8string());
    struct Restore {
        std::string bin, count;
        ~Restore() {
            if (bin.empty()) utils::unset_environment_variable_utf8("LEMONADE_FLM_NPU_BIN"); else utils::set_environment_variable_utf8("LEMONADE_FLM_NPU_BIN", bin);
            if (count.empty()) utils::unset_environment_variable_utf8("JUICED_TEST_FLM_COUNT"); else utils::set_environment_variable_utf8("JUICED_TEST_FLM_COUNT", count);
        }
    } restore{old_bin, old_counter};
    const auto count = [&] { int value = 0; std::ifstream input(counter); input >> value; return value; };
    const auto definition = [](const std::string& checkpoint, bool plural = false) {
        json value = {{"recipe", "flm"}, {"labels", json::array({"chat"})}};
        if (plural) value["checkpoints"] = {{"main", checkpoint}}; else value["checkpoint"] = checkpoint;
        return value;
    };
    {
        ResourceAdmission ledger; ModelManager models; models.set_resource_admission(ledger);
        const auto checkpoint = "fixture:shared-delete";
        models.register_user_model("FLM-Shared-A", definition(checkpoint));
        models.register_user_model("FLM-Shared-B", definition(checkpoint, true));
        const auto retained = models.user_model_definition("FLM-Shared-B");
        const int before = count();
        check(models.flm_checkpoint_referenced("user.FLM-Shared-A", checkpoint), "native reference view recognizes a shared FLM checkpoint across singular/plural definitions");
        models.delete_model("FLM-Shared-A");
        check(count() == before && models.user_model_definition("FLM-Shared-A").is_null() && models.user_model_definition("FLM-Shared-B") == retained,
              "deleting one FLM registration preserves shared provider storage without invoking its helper");
        ModelPublicationTestHook::storage_view(models, checkpoint);
        check(!models.flm_checkpoint_referenced("user.FLM-Shared-B", checkpoint),
              "the provider's discovered storage view is not another registration retaining its own checkpoint");
        models.delete_model("FLM-Shared-B");
        check(count() == before + 1 && models.user_model_definition("FLM-Shared-B").is_null() && ledger.snapshot()["active"] == 0,
              "deleting the final FLM reference invokes removal exactly once and completes registry publication");
    }
    {
        ResourceAdmission ledger; ModelManager models; models.set_resource_admission(ledger);
        const auto checkpoint = "fixture:concurrent-reference";
        models.register_user_model("FLM-Planned-A", definition(checkpoint));
        HelperGate planned;
        ModelPublicationTestHook::removal(models, [&](const std::string& phase) { if (phase == "flm-planned") planned.pause(); });
        const int before = count();
        auto removal = std::async(std::launch::async, [&] { models.delete_model("FLM-Planned-A"); });
        const bool reached = planned.await();
        models.register_user_model("FLM-Planned-B", definition(checkpoint));
        const auto retained = models.user_model_definition("FLM-Planned-B");
        planned.open(); removal.get(); ModelPublicationTestHook::removal(models, {});
        check(reached && count() == before && models.user_model_definition("FLM-Planned-B") == retained,
              "a reference committed before destructive ownership prevents a stale FLM removal dispatch");
        models.delete_model("FLM-Planned-B");
        check(count() == before + 1, "the remaining concurrent reference has one final destructive removal");
    }
    int ordinal = 0;
    for (const auto phase : {"result", "registry"}) for (bool replacement : {false, true}) for (bool restart : {false, true}) {
        const auto name = "FLM-Publication-" + std::to_string(++ordinal);
        const auto checkpoint = "fixture:publication-" + std::to_string(ordinal);
        const auto unrelated = "FLM-Unrelated-" + std::to_string(ordinal);
        const int before = count();
        const bool result_phase = std::string(phase) == "result";
        {
            ResourceAdmission ledger; ModelManager models; models.set_resource_admission(ledger);
            models.register_user_model(name, definition(checkpoint));
            models.register_user_model(unrelated, definition("fixture:unrelated-" + std::to_string(ordinal)));
            const auto other = models.user_model_definition(unrelated);
            bool armed = true; fs::path staging;
            ModelPublicationTestHook::registry_save(models, [&](const json& body) {
                const bool selected = armed && (result_phase ? (body.contains(name) && body.at(name).value("juiced_removal", json::object()).value("state", "") == "completed") : !body.contains(name));
                if (!selected) { utils::JsonUtils::save_to_file(body, registry.u8string()); return; }
                armed = false;
                if (!replacement) { DenyReplacement denied(registry); utils::JsonUtils::save_to_file(body, registry.u8string()); }
                else {
                    std::unique_ptr<DenyReplacement> denied;
                    utils::JsonPersistenceTestHook::save(body, registry, [&](const fs::path& temporary) { staging = temporary; denied = std::make_unique<DenyReplacement>(registry); });
                }
            });
            bool rejected = false;
            try { models.delete_model(name); } catch (const std::exception&) { rejected = true; }
            ModelPublicationTestHook::registry_save(models, {});
            if (!staging.empty()) fs::remove(staging);
            const auto pending = models.user_model_definition(name);
            check(rejected && count() == before + 1 && pending.contains("juiced_removal") &&
                  pending.at("juiced_removal").at("state") == (result_phase ? "dispatching" : "completed") &&
                  models.user_model_definition(unrelated) == other,
                  "failed caller staging/replacement preserves exact deletion intent and unrelated registrations after one successful command");
            check(ledger.snapshot()["active"] == (result_phase ? 1 : 0),
                  "unpublished collected success retains helper accounting until its caller seals the result");
            bool target_guarded = false, reference_guarded = false;
            try { models.register_user_model(name, definition("fixture:replacement")); } catch (const std::exception&) { target_guarded = true; }
            try { models.register_user_model(name + "-late", definition(checkpoint)); } catch (const std::exception&) { reference_guarded = true; }
            check(target_guarded && reference_guarded && !models.model_exists(name),
                  "unresolved FLM publication reserves both the expected definition and provider checkpoint without advertising a usable model");
            if (result_phase) {
                bool changed_command = false, claimed = false;
                try { models.run_flm_helper("user." + name, "remove", executable, {"remove", "fixture:wrong-target"}, {}, 60, {},
                        {pending.at("juiced_removal").at("publication"), [&] { claimed = true; }}); }
                catch (const AdmissionError& error) { changed_command = error.code == "helper_publication_pending"; }
                check(changed_command && !claimed && count() == before + 1 && models.user_model_definition(name) == pending,
                      "a changed command cannot claim or replay a retained successful removal result");
            }
            if (result_phase) check(!models.cleanup_native_helpers() && ledger.snapshot()["active"] == 1,
                  "generic helper cleanup cannot discard a collected removal result before caller publication");
            else check(models.run_flm_helper("Other-Helper", "fixture", executable, {"--child", "exit"}) == 7,
                  "a durably completed caller result survives an unrelated helper journal update");
            if (!restart) {
                models.delete_model(name);
                check(count() == before + 1 && models.user_model_definition(name).is_null() && models.user_model_definition(unrelated) == other && ledger.snapshot()["active"] == 0,
                      "actual ModelManager retry completes deletion without repeating the provider command");
            }
        }
        if (restart) {
            ResourceAdmission ledger; ModelManager recovered; recovered.set_resource_admission(ledger);
            check(ledger.snapshot()["active"] == (result_phase ? 1 : 0),
                  "manager reconstruction restores a sealed unpublished result but does not invent an active completed command");
            recovered.delete_model(name);
            check(count() == before + 1 && recovered.user_model_definition(name).is_null() && !recovered.user_model_definition(unrelated).is_null() && ledger.snapshot()["active"] == 0,
                  "restart retry publishes the retained exact command result with no second removal");
        }
    }
    {
        ResourceAdmission ledger; ModelManager models; models.set_resource_admission(ledger);
        const auto checkpoint = "fixture:dispatch-ownership";
        models.register_user_model("FLM-Dispatch-A", definition(checkpoint));
        HelperGate owned;
        ModelPublicationTestHook::removal(models, [&](const std::string& phase) {
            if (phase == "flm-intent") owned.pause();
            if (phase == "flm-completed") throw std::runtime_error("controlled caller interruption after sealed success");
        });
        const int before = count();
        auto removal = std::async(std::launch::async, [&] { try { models.delete_model("FLM-Dispatch-A"); } catch (const std::runtime_error&) { return true; } return false; });
        const bool reached = owned.await();
        std::promise<void> attempting;
        auto reference = std::async(std::launch::async, [&] {
            attempting.set_value();
            try { models.register_user_model("FLM-Dispatch-B", definition(checkpoint)); }
            catch (const ModelRegistrationConflict&) { return true; }
            return false;
        });
        attempting.get_future().wait();
        const bool rejected_before_dispatch = reference.wait_for(std::chrono::seconds(1)) == std::future_status::ready;
        models.register_user_model("FLM-Unrelated-During-Dispatch", definition("fixture:other-during-dispatch"));
        const auto unrelated = models.user_model_definition("FLM-Unrelated-During-Dispatch");
        owned.open(); const bool interrupted = removal.get(); const bool rejected = reference.get();
        ModelPublicationTestHook::removal(models, {});
        check(reached && rejected_before_dispatch && interrupted && rejected && count() == before + 1 && models.user_model_definition("FLM-Dispatch-B").is_null() && !unrelated.is_null(),
              "durable checkpoint ownership rejects conflicting references while unrelated registration remains available through dispatch");
        models.delete_model("FLM-Dispatch-A");
        check(count() == before + 1 && models.user_model_definition("FLM-Dispatch-A").is_null(),
              "interrupted completed deletion retries registry publication without another helper");
    }
}

int main(int argc, char** argv) {
    if (argc > 1 && (std::string(argv[1]) == "list" || std::string(argv[1]) == "version" || std::string(argv[1]) == "validate")) {
        std::puts(std::string(argv[1]) == "version" ? R"({"version":"fixture"})" : "[]"); return 0;
    }
    if (argc > 2 && (std::string(argv[1]) == "pull" || std::string(argv[1]) == "remove")) {
        std::puts("[FLM]  Overall progress: 100.0% (1/1 files)"); std::fflush(stdout);
        const auto counter = utils::get_environment_variable_utf8("JUICED_TEST_FLM_COUNT");
        if (std::string(argv[1]) == "remove" && !counter.empty()) {
            int count = 0; { std::ifstream input(fs::u8path(counter)); input >> count; }
            { std::ofstream output(fs::u8path(counter)); output << count + 1; }
        }
        if (std::string(argv[2]) == "fixture:fail") return 7;
        if (std::string(argv[2]) == "fixture:args")
            return argc == 6 && std::string(argv[3]) == "--force" && std::string(argv[4]) == "--modelscope" && std::string(argv[5]) == "1" ? 0 : 9;
        return 0;
    }
    if (argc > 1 && std::string(argv[1]) == "--child") {
        const std::string mode = argc > 2 ? argv[2] : "exit";
        if (mode == "count" || mode == "late-count") {
            int count = 0; { std::ifstream input(argv[3]); input >> count; }
            { std::ofstream output(argv[3]); output << count + 1; }
        }
        if (mode == "closed-output") std::fclose(stdout);
        else if (mode == "oversize") { std::string text(70000, 'x'); std::fwrite(text.data(), 1, text.size(), stdout); std::fflush(stdout); }
        else { std::puts("helper-ready"); std::fflush(stdout); }
        if (mode == "hold" || mode == "oversize") std::this_thread::sleep_for(std::chrono::seconds(30));
        if (mode == "closed-output") std::this_thread::sleep_for(std::chrono::milliseconds(250));
        if (mode == "late-count") {
            const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(30);
            while (!fs::exists(argv[4]) && std::chrono::steady_clock::now() < deadline)
                std::this_thread::sleep_for(std::chrono::milliseconds(10));
        }
#ifdef _WIN32
        if (mode == "negative-status") ExitProcess(0xc0000409);
#endif
        return mode == "exit" || mode == "closed-output" ? 7 : 0;
    }
    try {
        const auto executable = fs::absolute(argv[0]).u8string();
        const auto journal = fs::path(utils::get_config_dir()) / "native_helpers.json";
        auto defaults = ConfigFile::base_defaults(); defaults["offline"] = true; defaults["disable_model_filtering"] = true;
        RuntimeConfig config(defaults); RuntimeConfig::set_global(&config);
        ResourceAdmission ledger([](const std::string&) -> std::optional<uint64_t> { return uint64_t(64) << 30; });
        ledger.configure({{"mode", "legacy"}});
        {
            ModelManager models; models.set_resource_admission(ledger);
            const auto run = [&](const std::string& mode, utils::OutputLineCallback callback = {}, int timeout = 5,
                                 std::function<bool()> cancelled = {}) {
                return models.run_flm_helper("Helper-Fixture", "fixture", executable, {"--child", mode}, callback, timeout, cancelled);
            };
            bool statuses = true;
            for (int i = 0; i < 12; ++i)
                statuses &= utils::ProcessManager::run_process_with_output(executable, {"--child", "exit"}, {}) == 7;
            check(statuses, "streamed process completion preserves nonzero status across repeated fast exits");
            check(run("exit") == 7 && ledger.snapshot()["active"] == 0,
                  "owned helper reports failure and releases its ticket only after confirmed collection");
            check(run("closed-output") == 7 && ledger.snapshot()["active"] == 0,
                  "closing stdout does not prematurely stop an owned helper before its actual exit");
#ifdef _WIN32
            check(run("negative-status") == static_cast<int>(0xc0000409), "owned Windows collection retains signed nonzero exit status");
#endif
            bool pre_dispatch = false;
            const auto never_started = test::make_scratch_dir("cancelled-helper") / "count";
            const auto committed_identity = models.native_helper_status();
            bool identities_rejected = true;
            for (const auto& identity : std::vector<std::pair<std::string, std::string>>{
                {"", "fixture"}, {std::string(4097, 'm'), "fixture"}, {"Helper-Fixture", ""}, {"Helper-Fixture", std::string(65, 'o')}}) {
                bool rejected = false;
                try { models.run_flm_helper(identity.first, identity.second, executable, {"--child", "count", never_started.u8string()}); }
                catch (const AdmissionError& error) { rejected = error.code == "helper_identity_invalid"; }
                identities_rejected &= rejected;
            }
            check(identities_rejected && !fs::exists(never_started) && ledger.snapshot()["active"] == 0 &&
                  models.native_helper_status() == committed_identity,
                  "invalid helper identities reject before dispatch without changing committed ownership");
            try { models.run_flm_helper("Helper-Fixture", "fixture", executable, {"--child", "count", never_started.u8string()}, {}, 5, [] { return true; }); }
            catch (const AdmissionError& error) { pre_dispatch = error.code == "request_cancelled"; }
            check(pre_dispatch && !fs::exists(never_started) && ledger.snapshot()["active"] == 0, "pre-dispatch cancellation acquires no process or uncertain ticket");
            std::atomic<bool> cancel{false};
            check(run("hold", [&](const auto&) { cancel = true; return true; }, 5, [&] { return cancel.load(); }) == -1 && ledger.snapshot()["active"] == 0,
                  "cancellation of a quiet helper confirms termination and releases ownership");
            bool callback_failure = false;
            try { run("hold", [](const auto&) -> bool { throw std::runtime_error("controlled output callback failure"); }); }
            catch (const std::runtime_error& error) { callback_failure = std::string(error.what()) == "controlled output callback failure"; }
            check(callback_failure && ledger.snapshot()["active"] == 0,
                  "callback exceptions propagate after confirmed child cleanup without leaking the ticket");
            bool oversized = false;
            try { run("oversize"); } catch (const std::runtime_error& error) { oversized = std::string(error.what()).find("64 KiB") != std::string::npos; }
            check(oversized && ledger.snapshot()["active"] == 0, "owned output buffering rejects an oversized line and confirms cleanup");
            check(run("hold", {}, 1) == -1 && ledger.snapshot()["active"] == 0,
                  "helper timeout returns failure after confirmed direct-child cleanup");
            ModelPublicationTestHook::stop_barrier(models, [] { throw std::runtime_error("controlled unconfirmed helper stop"); });
            bool retained = false;
            try { run("hold", {}, 1); } catch (const std::runtime_error&) { retained = true; }
            const int pending_pid = ModelPublicationTestHook::pid(models);
            check(retained && pending_pid > 0 && models.native_helper_status()["flm"]["pending"] == true && ledger.snapshot()["active"] == 1,
                  "failed timeout stop retains the actual child handle, durable intent and ticket");
            bool guarded = false;
            try { models.register_user_model("Helper-Fixture", {{"recipe", "llamacpp"}, {"checkpoint", "fixture/repo:Q4"}}); }
            catch (const AdmissionError& error) { guarded = error.code == "helper_cleanup_pending"; }
            check(guarded, "unresolved helper ownership prevents replacing its model definition");
            models.register_user_model("Unrelated-Fixture", {{"recipe", "llamacpp"}, {"checkpoint", "fixture/other:Q4"}});
            check(!models.user_model_definition("Unrelated-Fixture").is_null(), "unrelated registration remains usable while helper cleanup is pending");
            bool activation_guarded = false;
            try { ledger.configure({{"mode", "protected"}}); } catch (const AdmissionError& error) { activation_guarded = error.code == "admission_configuration_busy"; }
            check(activation_guarded && !models.cleanup_native_helpers() && ledger.snapshot()["active"] == 1,
                  "unconfirmed helper cleanup cannot release accounting or permit protected activation");
            BackendManager runtimes; runtimes.set_resource_admission(ledger);
            const auto runtime_dir = fs::path(backends::BackendUtils::get_install_directory("flm", "npu"));
            fs::create_directories(runtime_dir); const auto dependency = runtime_dir / "retained-helper-dependency";
            { std::ofstream output(dependency); output << "keep while the helper may be running"; }
            bool install_guarded = false, uninstall_guarded = false;
            try { runtimes.install_backend("flm", "npu", true); }
            catch (const AdmissionError& error) { install_guarded = error.code == "runtime_reference_busy"; }
            try { runtimes.uninstall_backend("flm", "npu"); }
            catch (const AdmissionError& error) { uninstall_guarded = error.code == "runtime_reference_busy"; }
            check(install_guarded && uninstall_guarded && fs::is_regular_file(dependency),
                  "native runtime installation and removal preserve files referenced by an unconfirmed helper");
            ModelPublicationTestHook::stop_barrier(models, {});
            check(models.cleanup_native_helpers() && ledger.snapshot()["active"] == 0 && models.native_helper_status()["flm"]["pending"] == false,
                  "confirmed retry cleans only the retained child and commits terminal ownership");
            ledger.pause(true); bool paused = false;
            try { run("exit"); } catch (const AdmissionError& error) { paused = error.code == "admission_paused"; }
            ledger.pause(false); ledger.configure({{"mode", "protected"}}); bool protected_mode = false;
            try { run("exit"); } catch (const AdmissionError& error) { protected_mode = error.code == "operation_unqualified"; }
            ledger.configure({{"mode", "legacy"}});
            check(paused && protected_mode && ledger.snapshot()["active"] == 0, "paused and protected policies reject helper dispatch before process creation");
            const auto counter = test::make_scratch_dir("helper-count") / "count";
            std::unique_ptr<DenyReplacement> denied;
            bool publication_failed = false;
            try { models.run_flm_helper("Helper-Fixture", "fixture", executable, {"--child", "count", counter.u8string()},
                    [&](const auto&) { denied = std::make_unique<DenyReplacement>(journal); return true; }); }
            catch (const std::exception&) { publication_failed = true; }
            denied.reset();
            check(publication_failed && ledger.snapshot()["active"] == 1 && models.native_helper_status()["flm"]["pending"] == true,
                  "failed terminal publication retains the collected command result and ticket");
            const int result = models.run_flm_helper("Helper-Fixture", "fixture", executable, {"--child", "count", counter.u8string()});
            int count = 0; { std::ifstream input(counter); input >> count; }
            check(result == 0 && count == 1 && ledger.snapshot()["active"] == 0,
                  "matching retry publishes the collected success without executing a destructive command twice");
            const auto late_counter = test::make_scratch_dir("helper-late-count") / "count";
            const auto late_release = late_counter.parent_path() / "release";
            ModelPublicationTestHook::stop_barrier(models, [] { throw std::runtime_error("controlled failed cancellation stop"); });
            bool late_pending = false;
            try { models.run_flm_helper("Helper-Fixture", "fixture", executable, {"--child", "late-count", late_counter.u8string(), late_release.u8string()}, [](const auto&) { return false; }); }
            catch (const std::runtime_error&) { late_pending = true; }
            { std::ofstream release(late_release); release << "finish original command"; }
            const auto late_deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
            while (late_pending && !ModelPublicationTestHook::exited(models) && std::chrono::steady_clock::now() < late_deadline)
                std::this_thread::sleep_for(std::chrono::milliseconds(10));
            check(late_pending && ledger.snapshot()["active"] == 1 && ModelPublicationTestHook::exited(models),
                  "failed cancellation keeps ownership even when the original command later exits successfully");
            ModelPublicationTestHook::stop_barrier(models, {});
            const auto late_result = models.run_flm_helper("Helper-Fixture", "fixture", executable, {"--child", "late-count", late_counter.u8string(), late_release.u8string()});
            int late_count = 0; { std::ifstream input(late_counter); input >> late_count; }
            check(late_result == 0 && late_count == 1 && ledger.snapshot()["active"] == 0 && models.native_helper_status()["flm"]["state"] == "completed",
                  "retry adopts a collected late success without repeating its destructive side effect");
            const auto previous_bin = utils::get_environment_variable_utf8("LEMONADE_FLM_NPU_BIN");
            utils::set_environment_variable_utf8("LEMONADE_FLM_NPU_BIN", executable);
            {
                backends::FastFlowLMServer flm("error", &models);
                check(flm.download_model("fixture:args", false, RemoteRegistrySource::ModelScope) == "fixture:args" && ledger.snapshot()["active"] == 0,
                      "the actual load-pull wrapper uses owned command execution and preserves registry arguments");
                bool failed_pull = false, failed_remove = false;
                try { flm.download_model("fixture:fail"); } catch (const std::runtime_error& error) { failed_pull = std::string(error.what()).find("7") != std::string::npos; }
                try { backends::fastflowlm::flm_remove("fixture:fail", &models, "Helper-Fixture"); }
                catch (const std::runtime_error& error) { failed_remove = std::string(error.what()).find("7") != std::string::npos; }
                check(failed_pull && failed_remove && ledger.snapshot()["active"] == 0,
                      "actual FLM pull/remove wrappers preserve failure status after confirmed cleanup");
                backends::fastflowlm::flm_remove("fixture:good", &models, "Helper-Fixture");
                check(ledger.snapshot()["active"] == 0, "actual FLM removal completes only after owned direct-child collection");
            }
            if (previous_bin.empty()) utils::unset_environment_variable_utf8("LEMONADE_FLM_NPU_BIN");
            else utils::set_environment_variable_utf8("LEMONADE_FLM_NPU_BIN", previous_bin);
            bool unowned_download = false, unowned_remove = false, unowned_pull = false;
            try { backends::fastflowlm::flm_download("fixture:good", false, {}); }
            catch (const AdmissionError& error) { unowned_download = error.code == "operation_unqualified"; }
            try { backends::fastflowlm::flm_remove("fixture:good"); }
            catch (const AdmissionError& error) { unowned_remove = error.code == "operation_unqualified"; }
            { backends::FastFlowLMServer standalone("error");
              try { standalone.download_model("fixture:good"); } catch (const AdmissionError& error) { unowned_pull = error.code == "operation_unqualified"; } }
            check(unowned_download && unowned_remove && unowned_pull,
                  "all three FLM entry paths reject execution without their native management owner");
        }
        deletion_publication(executable);
        {
            json committed; { std::ifstream input(journal); input >> committed; }
            for (const auto fault : {"version", "pending", "terminal", "publication_pending", "publication"}) {
                auto invalid = committed;
                if (std::string(fault) == "version") invalid["version"] = 2;
                else if (std::string(fault) == "pending") invalid["flm"].erase("pending");
                else if (std::string(fault) == "terminal") invalid["flm"]["state"] = "dispatching";
                else if (std::string(fault) == "publication_pending") invalid["flm"]["publication_pending"] = "false";
                else invalid["flm"].erase("publication");
                { std::ofstream output(journal); output << invalid; }
                bool quarantined = false;
                { ModelManager rejected;
                  try { rejected.set_resource_admission(ledger); }
                  catch (const AdmissionError& error) { quarantined = error.code == "helper_recovery_required"; } }
                check(quarantined && ledger.snapshot()["active"] == 0,
                      "unknown or incomplete helper journals reject activation without assuming cleanup");
            }
            { std::ofstream output(journal); output << committed; }
        }
        {
            json pending = {{"version", 1}, {"flm", {{"model", "Old-Fixture"}, {"operation", "remove"}, {"fingerprint", std::string(64, 'a')}, {"pending", true}, {"state", "dispatching"}}}};
            { std::ofstream output(journal); output << pending; }
            ModelManager recovered; recovered.set_resource_admission(ledger);
            bool blocked = false;
            try { recovered.run_flm_helper("Old-Fixture", "remove", executable, {"--child", "exit"}); }
            catch (const AdmissionError& error) { blocked = error.code == "helper_recovery_required"; }
            check(blocked && !recovered.cleanup_native_helpers() && recovered.native_helper_status()["flm"]["pending"] == true,
                  "manager reconstruction preserves unknown prior-helper ownership and refuses another command");
            bool preserved = false;
            try { ledger.acquire_management("flm:npu", "runtime-uninstall"); }
            catch (const AdmissionError& error) { preserved = error.code == "runtime_reference_busy"; }
            bool activation_blocked = false;
            try { ledger.configure({{"mode", "protected"}}); }
            catch (const AdmissionError& error) { activation_blocked = error.code == "admission_configuration_busy"; }
            check(preserved && activation_blocked && ledger.snapshot()["active"] == 1 && recovered.native_helper_status()["flm"]["recovery_required"] == true,
                  "prior-manager helper uncertainty retains runtime-reference accounting and reclamation exclusion");
        }
        RuntimeConfig::set_global(nullptr);
    } catch (const std::exception& error) { ++failures; std::fprintf(stderr, "Unexpected helper fixture failure: %s\n", error.what()); }
    std::fprintf(stderr, "Native helpers: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
