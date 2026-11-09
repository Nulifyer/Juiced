#include "lemon/resource_admission.h"

#include <algorithm>
#include <condition_variable>
#include <deque>
#include <limits>
#include <map>
#include <mutex>
#include <set>
#include <vector>

namespace lemon {
namespace {
using json = nlohmann::json;
using Clock = std::chrono::steady_clock;
using Charges = std::map<std::string, uint64_t>;
constexpr uint64_t input_fixed_bytes = 65536, input_body_factor = 8, input_value_bytes = 512;

uint64_t natural(const json& object, const char* key, uint64_t fallback,
                 uint64_t maximum = std::numeric_limits<int64_t>::max()) {
    if (!object.contains(key)) return fallback;
    const auto& value = object.at(key);
    if (!value.is_number_integer() || (value.is_number_integer() && !value.is_number_unsigned() && value.get<int64_t>() < 0) ||
        value.get<uint64_t>() > maximum)
        throw std::invalid_argument(std::string("Invalid admission ") + key);
    return value.get<uint64_t>();
}
void known_keys(const json& object, std::initializer_list<const char*> keys) {
    if (!object.is_object()) throw std::invalid_argument("Admission settings must be objects");
    for (const auto& entry : object.items()) {
        if (std::none_of(keys.begin(), keys.end(), [&](const char* key) { return entry.key() == key; }))
            throw std::invalid_argument("Unknown admission setting: " + entry.key());
    }
}
std::string text(const json& object, const char* key, const std::string& fallback = {}) {
    if (!object.contains(key)) return fallback;
    if (!object.at(key).is_string()) throw std::invalid_argument(std::string("Admission ") + key + " must be a string");
    return object.at(key).get<std::string>();
}
Charges charges(const json& object, const char* key) {
    Charges result;
    if (!object.contains(key)) return result;
    if (!object.at(key).is_object()) throw std::invalid_argument("Admission charges must be objects");
    for (const auto& item : object.at(key).items()) {
        if (item.key().empty()) throw std::invalid_argument("Empty physical pool identifier");
        result[item.key()] = natural(object.at(key), item.key().c_str(), 0);
    }
    return result;
}
uint64_t ingress_control_reserve(const json& ingress) {
    const auto request = natural(ingress, "request_bytes", ResourceAdmission::maximum_ingress_bytes);
    const auto total = natural(ingress, "total_bytes", 4 * ResourceAdmission::maximum_ingress_bytes);
    return std::min(ResourceAdmission::maximum_control_body_bytes, total - request);
}

uint64_t ingress_host_estimate(uint64_t bytes) {
    return input_fixed_bytes + input_body_factor * bytes +
        input_value_bytes * std::min(bytes, ResourceAdmission::maximum_json_values);
}

std::string operation_name(ResourceAdmission::Operation operation) {
    switch (operation) {
        case ResourceAdmission::Operation::Load: return "load";
        case ResourceAdmission::Operation::State: return "state";
        case ResourceAdmission::Operation::Realtime: return "realtime";
        case ResourceAdmission::Operation::Management: return "management";
        case ResourceAdmission::Operation::Ingress: return "ingress";
        default: return "request";
    }
}
}

struct ResourceAdmission::State {
    struct Pool { std::optional<uint64_t> capacity; uint64_t essential_headroom = 0; std::string source; };
    struct Entry {
        uint64_t id = 0;
        Work work;
        Charges costs;
        Charges resident_costs;
        std::string priority = "interactive";
        size_t concurrency = 64;
        Clock::time_point queue_deadline;
        Clock::time_point completion_deadline;
        std::chrono::milliseconds completion_limit{300000};
        bool started = false;
        bool complete = false;
        bool resident = false;
        bool uncertain = false;
        bool protected_mode = false;
    };
    mutable std::mutex mutex;
    std::condition_variable changed;
    Headroom headroom;
    json config = json::object();
    std::map<std::string, Pool> pools;
    std::deque<std::shared_ptr<Entry>> pending;
    std::map<uint64_t, std::shared_ptr<Entry>> active;
    std::set<uint64_t> stopped_generations;
    uint64_t next_id = 0, rejected = 0, admitted = 0;
    size_t essential_streak = 0;
    bool paused = false, stopped = false, configuration_pending = false;
    uint64_t revision = 0;

    Charges reserved() const {
        Charges result;
        for (const auto& item : active) for (const auto& cost : item.second->costs) result[cost.first] += cost.second;
        return result;
    }
    enum class Capacity { Available, Unknown, Exhausted };
    Capacity capacity(const Entry& entry, const std::map<std::string, Pool>& limits,
                      bool include_active = true) const {
        const bool ingress = entry.work.operation == Operation::Ingress;
        const auto used = include_active ? reserved() : Charges{};
        for (const auto& cost : entry.costs) {
            const auto pool = limits.find(cost.first);
            uint64_t reserve = 0;
            if (pool == limits.end()) {
                if (!ingress || cost.first != "host") return Capacity::Unknown;
            } else {
                if (!pool->second.capacity) return Capacity::Unknown;
                reserve = entry.priority == "essential" ? 0 : pool->second.essential_headroom;
                const auto cap = *pool->second.capacity - std::min(*pool->second.capacity, reserve);
                const auto taken = used.find(cost.first);
                const auto occupied = taken == used.end() ? 0 : taken->second;
                if (occupied > cap || cost.second > cap - occupied) return Capacity::Exhausted;
            }
            if (!headroom) {
                if (ingress) return Capacity::Unknown;
                continue;
            }
            const auto available = headroom(cost.first);
            if (!available) return Capacity::Unknown;
            uint64_t outstanding = 0;
            if (include_active) for (const auto& active_entry : active) {
                if (active_entry.second->resident) continue;
                const auto claim = active_entry.second->costs.find(cost.first);
                if (claim != active_entry.second->costs.end()) outstanding += claim->second;
            }
            const auto usable = *available - std::min(*available, reserve);
            if (outstanding > usable || cost.second > usable - outstanding) return Capacity::Exhausted;
        }
        return Capacity::Available;
    }
    bool eligible(const Entry& entry) const {
        size_t running = 0, state_running = 0, generation_running = 0;
        for (const auto& item : active) {
            if (item.second->resident || item.second->work.operation == Operation::Management ||
                item.second->work.operation == Operation::Ingress) continue;
            if (item.second->uncertain && item.second->work.generation == entry.work.generation &&
                item.second->work.operation != Operation::Request) return false;
            if (item.second->work.operation == Operation::State) { ++state_running; continue; }
            ++running;
            if (item.second->work.generation == entry.work.generation) ++generation_running;
        }
        if (entry.work.operation == Operation::State) {
            if (state_running >= natural(config, "active_limit", 64, 4096)) return false;
        } else if (running >= natural(config, "active_limit", 64, 4096) || generation_running >= entry.concurrency) return false;
        return capacity(entry, pools) == Capacity::Available;
    }
    std::shared_ptr<Entry> selected() const {
        std::shared_ptr<Entry> essential, ordinary;
        std::set<std::string> seen;
        for (const auto& waiter : pending) {
            if (seen.count(waiter->priority) || !eligible(*waiter)) continue;
            seen.insert(waiter->priority);
            if (waiter->priority == "essential") essential = waiter;
            else if (!ordinary) ordinary = waiter;
        }
        return essential && (essential_streak < 8 || !ordinary) ? essential : ordinary;
    }
};

ResourceAdmission::ResourceAdmission(Headroom headroom) : state_(std::make_shared<State>()) {
    state_->headroom = std::move(headroom);
}

void ResourceAdmission::validate_config(const json& config) {
    known_keys(config, {"mode", "pending_limit", "service_pending_limit", "principal_pending_limit", "active_limit",
                        "essential_wait_ms", "interactive_wait_ms", "batch_wait_ms", "pools", "models", "ingress"});
    const auto mode = text(config, "mode", "legacy");
    if (mode != "legacy" && mode != "protected") throw std::invalid_argument("Admission mode must be legacy or protected");
    for (const auto* key : {"pending_limit", "service_pending_limit", "principal_pending_limit", "active_limit"})
        if (!natural(config, key, 1, 4096)) throw std::invalid_argument("Admission queue limits must be positive");
    for (const auto* key : {"essential_wait_ms", "interactive_wait_ms", "batch_wait_ms"})
        if (!natural(config, key, 1, 300000)) throw std::invalid_argument("Admission wait bounds must be positive");
    const auto ingress = config.value("ingress", json::object());
    known_keys(ingress, {"request_bytes", "total_bytes", "active_limit"});
    const auto request_bytes = natural(ingress, "request_bytes", maximum_ingress_bytes, maximum_ingress_bytes);
    if (!request_bytes || !natural(ingress, "total_bytes", 4 * maximum_ingress_bytes) ||
        natural(ingress, "total_bytes", 4 * maximum_ingress_bytes) < request_bytes ||
        !natural(ingress, "active_limit", 4, 4096))
        throw std::invalid_argument("Ingress requires positive request, aggregate and concurrency bounds");
    const auto pools = config.value("pools", json::object());
    if (!pools.is_object()) throw std::invalid_argument("Admission pools must be objects");
    for (const auto& item : pools.items()) {
        if (item.key().empty()) throw std::invalid_argument("Empty physical pool identifier");
        const auto& pool = item.value();
        known_keys(pool, {"capacity_bytes", "essential_headroom_bytes", "source"});
        if (pool.contains("capacity_bytes") && !pool.at("capacity_bytes").is_null()) natural(pool, "capacity_bytes", 0);
        natural(pool, "essential_headroom_bytes", 0);
        if (text(pool, "source").empty()) throw std::invalid_argument("Physical pools require an accounting source");
    }
    const auto models = config.value("models", json::object());
    if (!models.is_object()) throw std::invalid_argument("Admission models must be objects");
    for (const auto& item : models.items()) {
        const auto& model = item.value();
        known_keys(model, {"checkpoint", "options", "priority", "concurrency", "completion_ms", "load_bytes",
                           "resident_bytes", "request_bytes", "state_bytes", "realtime_bytes", "request_limits", "source"});
        if (model.contains("options") && model.at("options").is_object() && model.at("options").contains("ctx_size")) {
            const auto& context = model.at("options").at("ctx_size");
            if (!context.is_number_integer() || context.get<int64_t>() <= 0)
                throw std::invalid_argument("Resource profiles require a positive explicit context ceiling");
        }
        if (model.contains("request_limits")) {
            const auto& limits = model.at("request_limits");
            known_keys(limits, {"input_bytes", "batch", "output_tokens"});
            for (const auto& item : limits.items())
                if (!natural(limits, item.key().c_str(), 0))
                    throw std::invalid_argument("Request limits must be positive");
        }
        const auto priority = text(model, "priority", "interactive");
        if (priority != "essential" && priority != "interactive" && priority != "batch")
            throw std::invalid_argument("Invalid admission priority");
        if (!natural(model, "concurrency", 1, 4096) || !natural(model, "completion_ms", 300000, 86400000))
            throw std::invalid_argument("Admission execution limits must be positive");
        if (!model.contains("options") || !model.at("options").is_object() ||
            text(model, "checkpoint").empty() || text(model, "source").empty())
            throw std::invalid_argument("Admission profiles require checkpoint, options and estimate source");
        const auto resident = charges(model, "resident_bytes"), load = charges(model, "load_bytes");
        for (const auto& cost : resident) {
            const auto peak = load.find(cost.first);
            if (peak == load.end() || peak->second < cost.second)
                throw std::invalid_argument("Load peak must include resident allocations");
        }
        for (const auto* key : {"load_bytes", "resident_bytes", "request_bytes", "state_bytes", "realtime_bytes"})
            for (const auto& cost : charges(model, key))
                if (!pools.contains(cost.first)) throw std::invalid_argument("Model charge references an unknown physical pool");
        if (mode == "protected" && (load.empty() || resident.empty() || charges(model, "request_bytes").empty()))
            throw std::invalid_argument("Protected profiles require explicit load, resident and request ceilings");
    }
}

void ResourceAdmission::configure(const json& config, std::function<void()> publish) {
    validate_config(config);
    json next = config;
    std::map<std::string, State::Pool> pools;
    const auto pool_config = config.value("pools", json::object());
    for (const auto& item : pool_config.items()) {
        State::Pool pool;
        if (item.value().contains("capacity_bytes") && !item.value().at("capacity_bytes").is_null())
            pool.capacity = natural(item.value(), "capacity_bytes", 0);
        pool.essential_headroom = natural(item.value(), "essential_headroom_bytes", 0);
        pool.source = item.value().at("source").get<std::string>();
        pools.emplace(item.key(), std::move(pool));
    }
    std::unique_lock<std::mutex> lock(state_->mutex);
    if (state_->configuration_pending)
        throw AdmissionError("admission_configuration_busy", "Admission configuration is already changing", 409);
    if (state_->config == config && !publish) return;
    const auto ingress = config.value("ingress", json::object());
    uint64_t body_bytes = 0, regular_bytes = 0, readers = 0, control_readers = 0;
    for (const auto& item : state_->active) {
        if (item.second->work.operation != Operation::Ingress)
            throw AdmissionError("admission_configuration_busy", "Drain work and unload models before changing admission settings", 409);
        const auto bytes = item.second->work.workload.at("body_bytes");
        if (bytes > natural(ingress, "request_bytes", maximum_ingress_bytes))
            throw AdmissionError("admission_configuration_busy", "Drain request bodies before lowering ingress bounds", 409);
        body_bytes += bytes;
        const bool control = item.second->work.service == "http-control";
        if (!control) regular_bytes += bytes;
        if (!item.second->complete) { if (control) ++control_readers; else ++readers; }
    }
    const auto total = natural(ingress, "total_bytes", 4 * maximum_ingress_bytes);
    if (!state_->pending.empty() || body_bytes > total || regular_bytes > total - ingress_control_reserve(ingress) ||
        readers > natural(ingress, "active_limit", 4) || control_readers > 1)
        throw AdmissionError("admission_configuration_busy", "Drain work before lowering admission bounds", 409);
    if (config.value("mode", std::string("legacy")) == "protected" && !state_->active.empty()) {
        State::Entry aggregate;
        aggregate.work.operation = Operation::Ingress;
        aggregate.priority = "essential";
        aggregate.costs = state_->reserved();
        const auto capacity = state_->capacity(aggregate, pools, false);
        if (capacity == State::Capacity::Unknown)
            throw AdmissionError("ingress_memory_unknown", "Current input memory headroom cannot be measured", 503);
        if (capacity != State::Capacity::Available)
            throw AdmissionError("admission_configuration_busy", "Active input memory does not fit the proposed host pool", 409);
        aggregate.priority = "interactive";
        aggregate.costs.clear();
        for (const auto& item : state_->active) if (item.second->work.service != "http-control")
            for (const auto& cost : item.second->costs) aggregate.costs[cost.first] += cost.second;
        const auto ordinary_capacity = state_->capacity(aggregate, pools, false);
        if (ordinary_capacity == State::Capacity::Unknown)
            throw AdmissionError("ingress_memory_unknown", "Current input memory headroom cannot be measured", 503);
        if (ordinary_capacity != State::Capacity::Available)
            throw AdmissionError("admission_configuration_busy", "Active input memory would consume essential host reserve", 409);
    }
    state_->configuration_pending = true;
    lock.unlock();
    try { if (publish) publish(); }
    catch (...) {
        lock.lock(); state_->configuration_pending = false; state_->changed.notify_all(); throw;
    }
    lock.lock();
    state_->pools.swap(pools);
    state_->config.swap(next);
    ++state_->revision;
    state_->configuration_pending = false;
    state_->changed.notify_all();
}

ResourceAdmission::Policy ResourceAdmission::policy() const {
    std::lock_guard<std::mutex> lock(state_->mutex);
    return {state_->revision, state_->config.value("mode", std::string("legacy")) == "protected",
            state_->paused || state_->stopped || state_->configuration_pending};
}

ResourceAdmission::Lease ResourceAdmission::acquire_ingress(uint64_t bytes, bool control) {
    Work work; work.service = control ? "http-control" : "http-body"; work.operation = Operation::Ingress;
    work.workload["body_bytes"] = bytes;
    return acquire(std::move(work));
}

ResourceAdmission::Lease ResourceAdmission::acquire_management(const std::string& target, const std::string& service) {
    Work work; work.model = target; work.service = service; work.operation = Operation::Management;
    return acquire(std::move(work));
}

ResourceAdmission::Lease ResourceAdmission::acquire(Work work) {
    auto entry = std::make_shared<State::Entry>();
    entry->work = std::move(work);
    std::unique_lock<std::mutex> lock(state_->mutex);
    auto reject = [&](const char* code, const char* message, int status) -> void {
        ++state_->rejected;
        throw AdmissionError(code, message, status);
    };
    if (state_->configuration_pending) reject("admission_configuration_pending", "Admission configuration is changing", 503);
    if (entry->work.config_revision && *entry->work.config_revision != state_->revision)
        reject("admission_configuration_changed", "Admission policy changed before reservation; retry", 503);
    entry->protected_mode = state_->config.value("mode", std::string("legacy")) == "protected";
    if (entry->work.operation == Operation::Ingress) {
        if (state_->stopped) reject("admission_paused", "Runtime admission is stopped", 503);
        const auto ingress = state_->config.value("ingress", json::object());
        const auto bytes = entry->work.workload.at("body_bytes");
        const bool control = entry->work.service == "http-control";
        if (bytes > natural(ingress, "request_bytes", maximum_ingress_bytes) ||
            (control && bytes > maximum_control_body_bytes))
            reject("ingress_limit_exceeded", "Request body exceeds the configured byte limit", 413);
        uint64_t used = 0, regular_used = 0, readers = 0, controls = 0;
        for (const auto& item : state_->active) if (item.second->work.operation == Operation::Ingress) {
            const bool other_control = item.second->work.service == "http-control";
            const auto other_bytes = item.second->work.workload.at("body_bytes");
            used += other_bytes;
            if (!other_control) regular_used += other_bytes;
            if (!item.second->complete) { if (other_control) ++controls; else ++readers; }
        }
        const auto total = natural(ingress, "total_bytes", 4 * maximum_ingress_bytes);
        const auto regular_limit = total - ingress_control_reserve(ingress);
        if (bytes > total || used > total - bytes ||
            (!control && (bytes > regular_limit || regular_used > regular_limit - bytes || readers >= natural(ingress, "active_limit", 4))) ||
            (control && controls >= 1))
            reject("ingress_capacity_exceeded", "Request body read or buffer capacity is in use; retry", 429);
        entry->costs["host"] = ingress_host_estimate(bytes);
        entry->priority = control ? "essential" : "interactive";
        if (entry->protected_mode) {
            const auto capacity = state_->capacity(*entry, state_->pools);
            if (capacity == State::Capacity::Unknown)
                reject("ingress_memory_unknown", "Host input-memory capacity is unknown", 503);
            if (capacity == State::Capacity::Exhausted)
                reject("ingress_capacity_exceeded", "Host input-memory capacity is in use; retry", 429);
        }
        entry->id = ++state_->next_id;
        entry->completion_deadline = Clock::now() + entry->completion_limit;
        state_->active.emplace(entry->id, entry);
        return Lease(state_, entry->id);
    }
    if (state_->stopped || state_->paused) reject("admission_paused", "Runtime admission is paused", 503);
    if (entry->work.cancelled && entry->work.cancelled()) reject("request_cancelled", "Request disconnected before admission", 499);
    const auto& config = state_->config;
    if (entry->work.operation == Operation::Management) {
        if (entry->protected_mode)
            reject("operation_unqualified", "Background acquisition and mutation envelopes are not qualified for protected admission", 503);
        // Legacy operations retain their existing unbounded behavior. Track the
        // synchronous native owner's lifetime so configuration cannot cross it.
        // These zero-cost tickets do not consume inference queue capacity.
        entry->priority = "background";
        entry->id = ++state_->next_id;
        entry->completion_deadline = Clock::now() + entry->completion_limit;
        state_->active.emplace(entry->id, entry);
        ++state_->admitted;
        return Lease(state_, entry->id);
    }
    const auto profiles = config.value("models", json::object());
    const auto profile = profiles.find(entry->work.model);
    if (profile != profiles.end()) {
        if (profile->at("checkpoint") != entry->work.checkpoint || profile->at("options") != entry->work.options)
            reject("admission_profile_mismatch", "Execution settings differ from the declared resource profile", 409);
        entry->priority = profile->value("priority", std::string("interactive"));
        entry->concurrency = natural(*profile, "concurrency", 1, 4096);
        entry->completion_limit = std::chrono::milliseconds(natural(*profile, "completion_ms", 300000, 86400000));
        const auto key = operation_name(entry->work.operation) + "_bytes";
        entry->costs = charges(*profile, key.c_str());
        entry->resident_costs = charges(*profile, "resident_bytes");
        if (entry->protected_mode && entry->work.operation == Operation::Request) {
            const auto limits = profile->value("request_limits", json::object());
            for (const auto* key : {"input_bytes", "batch"}) {
                if (!limits.contains(key) || !entry->work.workload.count(key))
                    reject("workload_unqualified", "Protected requests require a validated workload envelope", 503);
            }
            for (const auto& fact : entry->work.workload)
                if (!limits.contains(fact.first))
                    reject("workload_unqualified", "The resource profile does not bound every workload dimension", 503);
            for (const auto& limit : limits.items()) {
                const auto fact = entry->work.workload.find(limit.key());
                if (fact == entry->work.workload.end())
                    reject("workload_unqualified", "This request shape has no validated resource envelope", 503);
                if (fact->second > limit.value().get<uint64_t>())
                    reject("workload_limit_exceeded", "Request exceeds its declared resource envelope", 413);
            }
        }
        if (entry->protected_mode && entry->work.operation == Operation::State && !entry->work.workload.empty()) {
            const auto limits = profile->value("request_limits", json::object());
            for (const auto& fact : entry->work.workload) {
                if (!limits.contains(fact.first))
                    reject("workload_unqualified", "State request input has no declared resource bound", 503);
                if (fact.second > limits.at(fact.first).get<uint64_t>())
                    reject("workload_limit_exceeded", "State request exceeds its declared input bound", 413);
            }
        }
        if (config.value("mode", std::string("legacy")) == "protected" && entry->costs.empty())
            reject("operation_unqualified", "This operation has no declared resource ceiling", 503);
    } else if (config.value("mode", std::string("legacy")) == "protected") {
        reject("admission_profile_unknown", "Protected admission requires an explicit execution resource profile", 503);
    }
    for (const auto& cost : entry->costs) {
        const auto& pool = state_->pools.at(cost.first);
        if (!pool.capacity) reject("capacity_unknown", "Physical pool capacity is unknown", 503);
        if (state_->headroom && !state_->headroom(cost.first))
            reject("capacity_unknown", "Current physical pool headroom cannot be measured", 503);
        if (cost.second > *pool.capacity) reject("capacity_exceeded", "Operation cannot fit the physical pool ceiling", 429);
    }
    const auto wait_ms = entry->priority == "essential" ? natural(config, "essential_wait_ms", 2000, 300000)
        : entry->priority == "batch" ? natural(config, "batch_wait_ms", 30000, 300000)
        : natural(config, "interactive_wait_ms", 10000, 300000);
    entry->queue_deadline = Clock::now() + std::chrono::milliseconds(wait_ms);
    entry->id = ++state_->next_id;
    size_t service_pending = 0, principal_pending = 0;
    for (const auto& waiter : state_->pending) {
        if (waiter->work.service == entry->work.service) ++service_pending;
        if (waiter->work.principal == entry->work.principal) ++principal_pending;
    }
    if (state_->pending.size() >= natural(config, "pending_limit", 64, 4096) ||
        service_pending >= natural(config, "service_pending_limit", 16, 4096) ||
        principal_pending >= natural(config, "principal_pending_limit", 8, 4096))
        reject("queue_full", "Runtime request queue is full", 429);
    state_->pending.push_back(entry);
    auto erase = [&] { state_->pending.erase(std::find(state_->pending.begin(), state_->pending.end(), entry)); state_->changed.notify_all(); };
    try {
        while (true) {
            if (state_->configuration_pending) reject("admission_configuration_pending", "Admission configuration is changing", 503);
            if (state_->stopped || state_->paused) reject("admission_paused", "Runtime admission is paused", 503);
            if (entry->work.cancelled && entry->work.cancelled()) reject("request_cancelled", "Request disconnected while queued", 499);
            if (Clock::now() >= entry->queue_deadline) reject("queue_deadline_exceeded", "Runtime queue wait deadline exceeded", 429);
            if (state_->selected() == entry) {
                entry->completion_deadline = Clock::now() + entry->completion_limit;
                entry->work.cancelled = {};
                state_->active.emplace(entry->id, entry);
                erase();
                state_->essential_streak = entry->priority == "essential" ? std::min<size_t>(8, state_->essential_streak + 1) : 0;
                ++state_->admitted;
                return Lease(state_, entry->id);
            }
            state_->changed.wait_until(lock, std::min(entry->queue_deadline, Clock::now() + std::chrono::milliseconds(20)));
        }
    } catch (...) { erase(); throw; }
}

ResourceAdmission::Lease::Lease(std::shared_ptr<State> state, uint64_t id) : state_(std::move(state)), id_(id) {}
ResourceAdmission::Lease::~Lease() { release(); }
ResourceAdmission::Lease::Lease(Lease&& other) noexcept : state_(std::move(other.state_)), id_(other.id_) { other.id_ = 0; }
ResourceAdmission::Lease& ResourceAdmission::Lease::operator=(Lease&& other) noexcept {
    if (this != &other) { release(); state_ = std::move(other.state_); id_ = other.id_; other.id_ = 0; }
    return *this;
}
void ResourceAdmission::Lease::started() {
    if (!id_) return;
    std::lock_guard<std::mutex> lock(state_->mutex);
    auto& entry = *state_->active.at(id_);
    entry.started = !entry.costs.empty() || entry.work.operation == Operation::State ||
        entry.work.operation == Operation::Load;
    entry.complete = false;
}
void ResourceAdmission::Lease::completed() {
    if (!id_) return;
    std::lock_guard<std::mutex> lock(state_->mutex);
    state_->active.at(id_)->complete = true;
}
void ResourceAdmission::Lease::retain_residency() {
    if (!id_) return;
    std::lock_guard<std::mutex> lock(state_->mutex);
    auto& entry = *state_->active.at(id_);
    if (entry.work.operation != Operation::Load) throw std::logic_error("Only load leases can retain residency");
    entry.costs = std::move(entry.resident_costs);
    entry.resident = true;
    entry.complete = true;
    state_->changed.notify_all();
}
bool ResourceAdmission::Lease::deadline_expired() const {
    if (!id_) return false;
    std::lock_guard<std::mutex> lock(state_->mutex);
    return Clock::now() >= state_->active.at(id_)->completion_deadline;
}
bool ResourceAdmission::Lease::protected_mode() const {
    if (!id_) return false;
    std::lock_guard<std::mutex> lock(state_->mutex);
    return state_->active.at(id_)->protected_mode;
}
void ResourceAdmission::Lease::release() noexcept {
    if (!id_) return;
    {
        std::lock_guard<std::mutex> lock(state_->mutex);
        const auto found = state_->active.find(id_);
        if (found != state_->active.end()) {
            if (found->second->started && !found->second->complete) found->second->uncertain = true;
            else state_->active.erase(found);
        }
    }
    id_ = 0;
    state_->changed.notify_all();
}
void ResourceAdmission::pause(bool paused) {
    std::lock_guard<std::mutex> lock(state_->mutex);
    state_->paused = paused;
    state_->changed.notify_all();
}
void ResourceAdmission::shutdown() {
    std::lock_guard<std::mutex> lock(state_->mutex);
    state_->stopped = true;
    state_->changed.notify_all();
}
void ResourceAdmission::reconcile_stopped_generation(uint64_t generation) {
    std::lock_guard<std::mutex> lock(state_->mutex);
    if (generation) state_->stopped_generations.insert(generation);
    for (auto it = state_->active.begin(); it != state_->active.end();) {
        if (it->second->work.generation == generation && it->second->uncertain) it = state_->active.erase(it);
        else ++it;
    }
    state_->changed.notify_all();
}
void ResourceAdmission::reconcile_idle_requests(uint64_t generation) {
    std::lock_guard<std::mutex> lock(state_->mutex);
    for (auto it = state_->active.begin(); it != state_->active.end();) {
        if (it->second->work.generation == generation && it->second->uncertain &&
            it->second->work.operation == Operation::Request) it = state_->active.erase(it);
        else ++it;
    }
    state_->changed.notify_all();
}
bool ResourceAdmission::generation_was_stopped(uint64_t generation) const {
    std::lock_guard<std::mutex> lock(state_->mutex);
    return state_->stopped_generations.count(generation) != 0;
}
bool ResourceAdmission::has_uncertain_generation(uint64_t generation) const {
    std::lock_guard<std::mutex> lock(state_->mutex);
    return std::any_of(state_->active.begin(), state_->active.end(), [generation](const auto& item) {
        return item.second->work.generation == generation && item.second->uncertain;
    });
}
json ResourceAdmission::snapshot() const {
    std::lock_guard<std::mutex> lock(state_->mutex);
    json pools = json::object();
    const auto used = state_->reserved();
    for (const auto& pool : state_->pools) {
        const auto found = used.find(pool.first);
        pools[pool.first] = {{"capacity_bytes", pool.second.capacity ? json(*pool.second.capacity) : json(nullptr)},
            {"reserved_bytes", found == used.end() ? 0 : found->second}, {"source", pool.second.source},
            {"essential_headroom_bytes", pool.second.essential_headroom}};
    }
    if (!pools.contains("host") && used.count("host"))
        pools["host"] = {{"capacity_bytes", nullptr}, {"reserved_bytes", used.at("host")},
            {"source", "native available-byte sampler"}, {"configured", false},
            {"limit_kind", "sampled-headroom"}, {"essential_headroom_bytes", 0}};
    size_t running = 0, resident = 0, uncertain = 0, bodies = 0, readers = 0, control_readers = 0;
    uint64_t body_bytes = 0, host_bytes = 0;
    for (const auto& item : state_->active) {
        if (item.second->work.operation == Operation::Ingress) {
            ++bodies; body_bytes += item.second->work.workload.at("body_bytes");
            host_bytes += item.second->costs.at("host");
            if (!item.second->complete) {
                if (item.second->work.service == "http-control") ++control_readers; else ++readers;
            }
            continue;
        }
        if (item.second->resident) ++resident; else ++running;
        if (item.second->uncertain) ++uncertain;
    }
    const auto ingress = state_->config.value("ingress", json::object());
    return {{"ingress", {{"active", bodies}, {"reading", readers}, {"control_reading", control_readers},
                        {"control_reserved_bytes", ingress_control_reserve(ingress)}, {"reserved_body_bytes", body_bytes},
                        {"reserved_host_bytes", host_bytes}, {"physical_pool", "host"},
                        {"memory_estimate", {{"source", "native-http-input-v1"}, {"fixed_bytes", input_fixed_bytes},
                            {"bytes_per_body_byte", input_body_factor}, {"bytes_per_json_value", input_value_bytes},
                            {"maximum_json_values", maximum_json_values}, {"peak_qualified", false}}},
                        {"request_limit_bytes", natural(ingress, "request_bytes", maximum_ingress_bytes)},
                        {"total_limit_bytes", natural(ingress, "total_bytes", 4 * maximum_ingress_bytes)},
                        {"active_limit", natural(ingress, "active_limit", 4)}}},
        {"mode", state_->config.value("mode", std::string("legacy"))}, {"paused", state_->paused},
        {"stopped", state_->stopped}, {"configuration_revision", state_->revision},
        {"configuration_pending", state_->configuration_pending}, {"pending", state_->pending.size()}, {"active", running},
        {"resident", resident}, {"uncertain", uncertain}, {"pools", pools}, {"latency_qualified", false},
        {"admitted_total", state_->admitted}, {"rejected_total", state_->rejected}};
}
} // namespace lemon
