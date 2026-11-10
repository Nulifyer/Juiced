#include "lemon/model_manager.h"
#include "lemon/utils/json_utils.h"
#include "lemon/utils/path_utils.h"
#include "lemon/utils/aixlog.hpp"
#include <algorithm>
#include <exception>
#include <iomanip>
#include <sstream>
#include <mbedtls/md.h>

namespace lemon {
namespace {
constexpr size_t maximum_model_bytes = 4096, maximum_operation_bytes = 64;
class AdoptedHelperProcess final : public ServerProcess {
public:
    AdoptedHelperProcess() : ServerProcess(ProcessOutput{}) {}
    void attach(utils::ProcessHandle handle) noexcept { handle_ = handle; }
    std::string start(const ServerCommand&) override {
        throw std::logic_error("Captured helper ownership must be attached by the process layer");
    }
};
std::string journal_path() { return utils::get_config_dir() + "/native_helpers.json"; }
json read_journal() {
    const auto path = utils::path_from_utf8(journal_path());
    if (!std::filesystem::exists(path)) return {{"version", 1}};
    if (std::filesystem::file_size(path) > 65536) throw std::runtime_error("Native helper journal exceeds 64 KiB");
    const auto invalid = [] { throw AdmissionError("helper_recovery_required", "Native helper journal is incomplete or unsupported; preserve it for recovery", 503); };
    auto value = utils::JsonUtils::load_from_file(journal_path());
    if (!value.is_object() || value.size() != 2 || !value.contains("version") ||
        !value.at("version").is_number_integer() || value.at("version") != 1 ||
        !value.contains("flm") || !value.at("flm").is_object()) invalid();
    const auto& record = value.at("flm");
    const auto bounded_text = [&](const char* key, size_t maximum) {
        const auto field = record.find(key);
        return field != record.end() && field->is_string() && !field->get_ref<const std::string&>().empty() &&
            field->get_ref<const std::string&>().size() <= maximum;
    };
    if (!bounded_text("model", maximum_model_bytes) || !bounded_text("operation", maximum_operation_bytes) || !bounded_text("fingerprint", 64) ||
        !bounded_text("state", 32) || !record.contains("pending") || !record.at("pending").is_boolean()) invalid();
    const auto& fingerprint = record.at("fingerprint").get_ref<const std::string&>();
    if (fingerprint.size() != 64 || !std::all_of(fingerprint.begin(), fingerprint.end(), [](char c) {
        return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'); })) invalid();
    const bool publication = record.contains("publication");
    if (publication && (!bounded_text("publication", 64) || record.at("operation") != "remove")) invalid();
    if (record.at("pending").get<bool>()) {
        if (record.size() != size_t(publication ? 6 : 5) || record.at("state") != "dispatching") invalid();
    } else {
        if (record.size() != size_t(publication ? 9 : 7) || !record.contains("exit_code") || !record.at("exit_code").is_number_integer() ||
            !record.contains("interrupted") || !record.at("interrupted").is_boolean()) invalid();
        if (record.at("state") != (record.at("exit_code") == 0 ? "completed" : "failed")) invalid();
        if (publication && (!record.contains("publication_pending") || !record.at("publication_pending").is_boolean() ||
            (record.at("publication_pending") == true && record.at("exit_code") != 0))) invalid();
    }
    return value;
}
std::string bare(const std::string& name) { return name.rfind("user.", 0) == 0 ? name.substr(5) : name; }
std::string command_identity(const std::string& executable, const std::vector<std::string>& args) {
    const auto text = json{{"program", executable}, {"args", args}}.dump();
    unsigned char hash[32];
    if (mbedtls_md(mbedtls_md_info_from_type(MBEDTLS_MD_SHA256),
        reinterpret_cast<const unsigned char*>(text.data()), text.size(), hash))
        throw std::runtime_error("Cannot identify native helper command");
    std::ostringstream result;
    for (auto value : hash) result << std::hex << std::setw(2) << std::setfill('0') << int(value);
    return result.str();
}
}

void ModelManager::set_resource_admission(const ResourceAdmission& admission) {
    std::lock_guard<std::mutex> owner(flm_helper_mutex_);
    if (flm_helper_) throw std::runtime_error("Cannot rebind native helper ownership to another ledger");
    resource_admission_ = admission;
    const auto previous = read_journal().value("flm", json::object());
    if (!previous.value("pending", false) && !previous.value("publication_pending", false)) return;
    auto recovered = std::make_unique<FlmHelper>();
    recovered->intent = previous;
    if (previous.value("publication_pending", false)) recovered->result = 0;
    recovered->admission = resource_admission_->acquire_management("flm:npu", "runtime-reference");
    recovered->admission.started();
    flm_helper_ = std::move(recovered);
    std::atomic_store(&flm_helper_model_, std::make_shared<const std::string>(previous.value("model", "")));
}

void ModelManager::guard_native_helper_mutation(const std::string& model) {
    const auto active = std::atomic_load(&flm_helper_model_);
    if (active && bare(*active) == bare(model))
        throw AdmissionError("helper_cleanup_pending", "Finish native helper cleanup before changing this model", 409);
    const auto previous = read_journal().value("flm", json::object());
    if ((previous.value("pending", false) || previous.value("publication_pending", false)) && bare(previous.value("model", "")) == bare(model))
        throw AdmissionError("helper_recovery_required", "Previous native helper ownership requires recovery", 409);
}

int ModelManager::finish_flm_helper_locked(bool interrupted, const std::function<void()>& publish_success) {
    auto& helper = *flm_helper_;
    auto operation = helper.writer.enter();
    if (!helper.process && !helper.result) throw AdmissionError("helper_recovery_required", "Previous FLM helper termination requires recovery", 409);
    helper.finish_attempted = true;
    helper.interrupted |= interrupted;
    if (!helper.result) {
        helper.result = helper.process->collect_exit();
        if (!helper.result) {
            if (helper.dispatched && flm_helper_stop_barrier_) flm_helper_stop_barrier_();
            helper.process->stop();
            if (!helper.process->exited()) throw std::runtime_error("Native helper termination is unconfirmed");
            helper.result = -1;
        }
    }
    auto terminal = helper.intent;
    terminal["pending"] = false;
    terminal["state"] = *helper.result == 0 ? "completed" : "failed";
    terminal["exit_code"] = *helper.result;
    terminal["interrupted"] = helper.interrupted;
    const bool publication = terminal.contains("publication");
    if (publication) terminal["publication_pending"] = *helper.result == 0;
    auto journal = read_journal(); journal["flm"] = terminal;
    utils::JsonUtils::save_to_file(journal, journal_path());
    if (publication && *helper.result == 0) {
        if (!publish_success)
            throw AdmissionError("helper_publication_pending", "Finish the owning model deletion publication before another command", 409);
        publish_success();
        journal["flm"]["publication_pending"] = false;
        utils::JsonUtils::save_to_file(journal, journal_path());
    }
    helper.admission.completed();
    helper.admission = {};
    std::atomic_store(&flm_helper_model_, std::shared_ptr<const std::string>{});
    return helper.interrupted ? -1 : *helper.result;
}

int ModelManager::run_flm_helper(const std::string& model, const std::string& operation,
    const std::string& executable, const std::vector<std::string>& args,
    utils::OutputLineCallback on_line, int timeout_seconds, std::function<bool()> cancelled, FlmHelperPublication publication) {
    if (!resource_admission_) throw AdmissionError("operation_unqualified", "Native helper requires a bound management owner", 503);
    auto writer = resource_admission_->write_scope("native-flm-helper");
    if (model.empty() || model.size() > maximum_model_bytes || operation.empty() || operation.size() > maximum_operation_bytes)
        throw AdmissionError("helper_identity_invalid", "Native helper model and operation must fit their journal bounds", 400);
    if (publication.id.size() > 64 || (publication.id.empty() != !bool(publication.commit)) ||
        (!publication.id.empty() && operation != "remove"))
        throw AdmissionError("helper_identity_invalid", "Native deletion publication requires a bounded identity and owning callback", 400);
    auto mutation = model_mutation(model, false);
    if (!mutation.owns_lock()) throw AdmissionError("model_mutation_busy", "Wait for the current model mutation", 409);
    std::unique_lock<std::mutex> owner(flm_helper_mutex_, std::try_to_lock);
    if (!owner.owns_lock()) throw AdmissionError("helper_busy", "Wait for the active FLM command to finish", 409);
    if (cancelled && cancelled()) throw AdmissionError("request_cancelled", "Native helper cancelled before dispatch", 499);
    ResourceAdmission::Lease accepted;
    if (!flm_helper_) accepted = resource_admission_->acquire_management("flm:npu", "runtime-reference");
    const auto fingerprint = command_identity(executable, args);
    if (flm_helper_) {
        const bool matching = flm_helper_->intent.value("fingerprint", "") == fingerprint &&
            bare(flm_helper_->intent.value("model", "")) == bare(model) &&
            flm_helper_->intent.value("operation", "") == operation &&
            flm_helper_->intent.value("publication", "") == publication.id;
        finish_flm_helper_locked(false, matching ? publication.commit : std::function<void()>{});
        const bool already_succeeded = matching && flm_helper_->result && *flm_helper_->result == 0;
        flm_helper_.reset();
        if (already_succeeded) return 0;
    }
    auto journal = read_journal();
    const auto previous = journal.value("flm", json::object());
    if (previous.value("pending", false) || previous.value("publication_pending", false))
        throw AdmissionError("helper_recovery_required", "Previous FLM helper termination requires recovery before another command", 409);
    if (!accepted) accepted = resource_admission_->acquire_management("flm:npu", "runtime-reference");
    auto helper = std::make_unique<FlmHelper>();
    helper->writer = writer.continuation();
    helper->admission = std::move(accepted);
    helper->process = std::make_unique<AdoptedHelperProcess>();
    helper->intent = {{"model", model}, {"operation", operation}, {"fingerprint", fingerprint},
        {"pending", true}, {"state", "dispatching"}};
    if (!publication.id.empty()) helper->intent["publication"] = publication.id;
    journal["flm"] = helper->intent;
    if (journal.dump().size() > 65536) throw AdmissionError("helper_identity_too_large", "Native helper identity exceeds its journal bound", 413);
    // Failed publication is proven to precede dispatch and releases the unstarted ticket.
    utils::JsonUtils::save_to_file(journal, journal_path());
    flm_helper_ = std::move(helper);
    std::atomic_store(&flm_helper_model_, std::make_shared<const std::string>(model));
    utils::OutputProcessLifecycle lifecycle;
    lifecycle.attach = [&](utils::ProcessHandle handle) {
        static_cast<AdoptedHelperProcess*>(flm_helper_->process.get())->attach(handle);
        flm_helper_->dispatched = true;
        flm_helper_->admission.started();
    };
    lifecycle.finish = [&](bool interrupted) { return finish_flm_helper_locked(interrupted, publication.commit); };
    lifecycle.cancelled = std::move(cancelled);
    try {
        const int result = utils::ProcessManager::run_process_with_output(executable, args,
            std::move(on_line), "", timeout_seconds, true, {}, &lifecycle);
        flm_helper_.reset();
        return result;
    } catch (...) {
        const auto failure = std::current_exception();
        // Spawn errors also need terminal intent publication, but a failed finish
        // keeps its original process and ticket for a later explicit retry.
        if (!flm_helper_->finish_attempted) finish_flm_helper_locked(true, publication.commit);
        if (!flm_helper_->admission) flm_helper_.reset();
        std::rethrow_exception(failure);
    }
}

bool ModelManager::cleanup_native_helpers() noexcept {
    try {
        std::unique_lock<std::mutex> owner(flm_helper_mutex_, std::try_to_lock);
        if (!owner.owns_lock()) return false;
        if (!flm_helper_) {
            const auto previous = read_journal().value("flm", json::object());
            return !previous.value("pending", false) && !previous.value("publication_pending", false);
        }
        finish_flm_helper_locked(true);
        flm_helper_.reset();
        return true;
    } catch (const std::exception& error) {
        LOG(ERROR, "ModelManager") << "Native helper cleanup remains pending: " << error.what() << std::endl;
        return false;
    } catch (...) { return false; }
}

json ModelManager::native_helper_status() {
    auto status = read_journal();
    const auto model = std::atomic_load(&flm_helper_model_);
    if (model) status["flm"].update({{"model", *model}, {"pending", true}});
    std::unique_lock<std::mutex> owner(flm_helper_mutex_, std::try_to_lock);
    if (owner.owns_lock() && flm_helper_ && !flm_helper_->process)
        status["flm"]["recovery_required"] = true;
    return status;
}
} // namespace lemon
