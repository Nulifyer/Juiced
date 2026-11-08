#pragma once

#include <chrono>
#include <cstdint>
#include <functional>
#include <map>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>

#include <nlohmann/json.hpp>

namespace lemon {

class AdmissionError : public std::runtime_error {
public:
    AdmissionError(std::string code, std::string message, int status = 503)
        : std::runtime_error(std::move(message)), code(std::move(code)), status(status) {}
    std::string code;
    int status;
};

class ResourceAdmission {
    struct State;
public:
    enum class Operation { Load, Request, State, Realtime };
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
        bool protected_mode() const;
    private:
        friend class ResourceAdmission;
        Lease(std::shared_ptr<State> state, uint64_t id);
        void release() noexcept;
        std::shared_ptr<State> state_;
        uint64_t id_ = 0;
    };
    using Headroom = std::function<std::optional<uint64_t>(const std::string&)>;

    explicit ResourceAdmission(Headroom headroom = {});
    static void validate_config(const nlohmann::json& config);
    void configure(const nlohmann::json& config, std::function<void()> publish = {});
    Policy policy() const;
    Lease acquire(Work work);
    void pause(bool paused);
    void shutdown();
    void reconcile_stopped_generation(uint64_t generation);
    bool has_uncertain_generation(uint64_t generation) const;
    void reconcile_idle_requests(uint64_t generation);
    nlohmann::json snapshot() const;
private:
    std::shared_ptr<State> state_;
};

} // namespace lemon
