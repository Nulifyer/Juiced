#pragma once

#include <chrono>
#include <cstdint>
#include <functional>
#include <map>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>
#include <vector>

#include <nlohmann/json.hpp>

namespace lemon {

class AdmissionError : public std::runtime_error {
public:
    AdmissionError(std::string code, std::string message, int status = 503)
        : std::runtime_error(std::move(message)), code(std::move(code)), status(status) {}
    nlohmann::json to_json() const {
        nlohmann::json error = {{"message", what()}, {"type", "admission_error"},
            {"code", code}, {"status_code", status},
            {"details", {{"code", code}, {"status_code", status}}}};
        if (retry_after()) error["details"]["retry_after"] = retry_after();
        return {{"error", std::move(error)}};
    }
    int retry_after() const { return status == 429 || status == 503 ? 1 : 0; }
    std::string code;
    int status;
};

class ResourceAdmission {
    struct State;
    struct WriterRecord;
    static thread_local std::weak_ptr<WriterRecord> writer_context_;
public:
    enum class Operation { Load, Request, State, Realtime, Management, Ingress, Write };
    struct Work {
        std::string model;
        std::string service;
        std::string principal = "anonymous";
        uint64_t generation = 0;
        std::string checkpoint;
        nlohmann::json options = nlohmann::json::object();
        Operation operation = Operation::Request;
        std::function<bool()> cancelled;
        std::optional<uint64_t> config_revision;
        std::map<std::string, uint64_t> workload;
        uint64_t request_id = 0;
        std::vector<int> execution_slots;
        std::optional<int> requested_slot;
    };
    struct Policy {
        uint64_t revision;
        bool protected_mode;
        bool paused;
    };
    class Lease {
    public:
        Lease() = default;
        ~Lease();
        Lease(Lease&&) noexcept;
        Lease& operator=(Lease&&) noexcept;
        Lease(const Lease&) = delete;
        Lease& operator=(const Lease&) = delete;
        explicit operator bool() const { return id_ != 0; }
        void started();
        void completed();
        void retain_residency();
        bool deadline_expired() const;
        std::chrono::steady_clock::time_point completion_deadline() const;
        bool protected_mode() const;
        std::optional<int> execution_slot() const;
    private:
        friend class ResourceAdmission;
        Lease(std::shared_ptr<State> state, uint64_t id);
        void release() noexcept;
        std::shared_ptr<State> state_;
        uint64_t id_ = 0;
    };
    class WriteScope;
    class WriteToken {
    public:
        WriteToken() = default;
        WriteScope enter() const;
    private:
        friend class WriteScope;
        explicit WriteToken(std::shared_ptr<WriterRecord> record) : record_(std::move(record)) {}
        std::shared_ptr<WriterRecord> record_;
    };
    class WriteScope {
    public:
        WriteScope() = default;
        ~WriteScope();
        WriteScope(const WriteScope&) = delete;
        WriteScope& operator=(const WriteScope&) = delete;
        WriteToken continuation() const;
    private:
        friend class ResourceAdmission;
        friend class WriteToken;
        explicit WriteScope(std::shared_ptr<WriterRecord> record);
        std::shared_ptr<WriterRecord> record_;
        std::weak_ptr<WriterRecord> previous_;
    };
    WriteScope write_scope(const std::string& owner) const;
    void maintenance(bool requested);
    // Drained participating operations do not prove that idle workers or external
    // and prior-manager writers stopped; maintenance must validate those owners.
    bool wait_for_writes(std::chrono::milliseconds timeout) const;

    using Headroom = std::function<std::optional<uint64_t>(const std::string&)>;

    explicit ResourceAdmission(Headroom headroom = {});
    static void validate_config(const nlohmann::json& config);
    void configure(const nlohmann::json& config, std::function<void()> publish = {});
    Policy policy() const;
    Lease acquire(Work work);
    static constexpr uint64_t maximum_ingress_bytes = 8ULL * 1024 * 1024;
    static constexpr uint64_t maximum_control_body_bytes = 64ULL * 1024;
    static constexpr unsigned maximum_json_depth = 64;
    static constexpr uint64_t maximum_json_values = 32768;
    Lease acquire_ingress(uint64_t bytes, bool control = false);
    // Copies share this ledger's state. Native management owners keep a view so
    // shutdown and configuration transitions remain authoritative after binding.
    Lease acquire_management(const std::string& target, const std::string& service);
    void pause(bool paused);
    void shutdown();
    void reconcile_stopped_generation(uint64_t generation);
    bool has_uncertain_generation(uint64_t generation) const;
    bool generation_was_stopped(uint64_t generation) const;
    void reconcile_idle_requests(uint64_t generation);
    nlohmann::json snapshot() const;
private:
    std::shared_ptr<State> state_;
};

} // namespace lemon
