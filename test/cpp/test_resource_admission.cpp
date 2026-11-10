#include "lemon/resource_admission.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdio>
#include <future>
#include <mutex>
#include <thread>
#include <type_traits>
#include <vector>

using lemon::AdmissionError;
using lemon::ResourceAdmission;
using json = nlohmann::json;
using namespace std::chrono_literals;

template<class T> class ThreadTask {
public:
    template<class F> explicit ThreadTask(F action) {
        std::packaged_task<T()> task(std::move(action));
        result_ = task.get_future();
        thread_ = std::thread(std::move(task));
    }
    ThreadTask(ThreadTask&&) = default;
    ~ThreadTask() { if (thread_.joinable()) thread_.join(); }
    auto get() { return result_.get(); }
    template<class Rep, class Period> auto wait_for(std::chrono::duration<Rep, Period> duration) {
        return result_.wait_for(duration);
    }
private:
    std::future<T> result_;
    std::thread thread_;
};
template<class F> auto run_thread(F action) {
    return ThreadTask<std::invoke_result_t<F>>(std::move(action));
}

int main() {
    int failures = 0, checks = 0;
    auto check = [&](bool ok, const char* name) {
        ++checks;
        std::fprintf(stderr, "CHECK %d: %s\n", checks, name);
        if (!ok) { ++failures; std::fprintf(stderr, "FAIL: %s\n", name); }
    };
    auto error = [&](auto action, const char* code) {
        try { action(); return false; }
        catch (const AdmissionError& e) {
            if (e.code != code) std::fprintf(stderr, "Expected %s, received %s: %s\n", code, e.code.c_str(), e.what());
            return e.code == code;
        }
    };
    json profile = {{"checkpoint", "fixture@immutable"}, {"options", {{"ctx_size", 2048}}},
        {"priority", "interactive"}, {"concurrency", 1}, {"completion_ms", 100},
        {"load_bytes", {{"uma", 600}}}, {"resident_bytes", {{"uma", 400}}},
        {"request_limits", {{"input_bytes", 65536}, {"batch", 1}, {"output_tokens", 128}}},
        {"request_bytes", {{"uma", 300}}}, {"source", "frozen fixture peak"}};
    json config = {{"mode", "protected"}, {"interactive_wait_ms", 120}, {"essential_wait_ms", 120},
        {"batch_wait_ms", 120}, {"pools", {{"uma", {{"capacity_bytes", 1000},
        {"essential_headroom_bytes", 200}, {"source", "one shared physical pool"}}}}},
        {"models", {{"model", profile}}}};
    auto work = [&](uint64_t generation = 1) {
        ResourceAdmission::Work w;
        w.model = "model"; w.service = "chat"; w.generation = generation;
        w.workload = {{"input_bytes", 1}, {"batch", 1}, {"output_tokens", 32}};
        w.checkpoint = "fixture@immutable"; w.options = {{"ctx_size", 2048}};
        return w;
    };
    ResourceAdmission controller;
    controller.configure(config);
    check(controller.snapshot()["pools"].size() == 1, "CPU/GPU UMA is one pool");
    {
        auto load_work = work(); load_work.operation = ResourceAdmission::Operation::Load;
        auto load = controller.acquire(load_work);
        check(controller.snapshot()["pools"]["uma"]["reserved_bytes"] == 600, "load peak reserved atomically");
        load.started(); load.retain_residency();
        check(controller.snapshot()["resident"] == 1 && controller.snapshot()["active"] == 0 &&
              controller.snapshot()["pools"]["uma"]["reserved_bytes"] == 400,
              "load peak converts to one generation's retained weights");
        {
            auto request = controller.acquire(work());
            check(controller.snapshot()["pools"]["uma"]["reserved_bytes"] == 700,
                  "request reserves incremental state without repeating weights");
            request.started(); request.completed();
        }
        check(controller.snapshot()["pools"]["uma"]["reserved_bytes"] == 400,
              "completed request releases only its incremental reservation");
        auto next = work(2); next.operation = ResourceAdmission::Operation::Load;
        check(error([&] { controller.acquire(next); }, "queue_deadline_exceeded"),
              "another load cannot consume essential headroom");
    }
    check(controller.snapshot()["pools"]["uma"]["reserved_bytes"] == 0,
          "worker residency release restores capacity");
    {
        auto lease = controller.acquire(work()); lease.started();
    }
    check(controller.snapshot()["uncertain"] == 1 && controller.snapshot()["active"] == 1,
          "uncertain upstream termination retains capacity");
    check(error([&] { controller.acquire(work()); }, "queue_deadline_exceeded"),
          "same generation cannot reuse a quarantined slot");
    controller.reconcile_stopped_generation(2);
    check(controller.snapshot()["uncertain"] == 1, "different generation cannot clear quarantine");
    controller.reconcile_stopped_generation(1);
    check(controller.snapshot()["uncertain"] == 0 && controller.snapshot()["active"] == 0,
          "observed generation termination reconciles uncertainty");
    {
        auto handoff_config = config; handoff_config["interactive_wait_ms"] = 10000;
        controller.configure(handoff_config);
        auto first = controller.acquire(work());
        auto future = run_thread([&] {
            auto second = controller.acquire(work()); return bool(second);
        });
        const auto ready_deadline = std::chrono::steady_clock::now() + 5s;
        while (controller.snapshot()["pending"] != 1 && std::chrono::steady_clock::now() < ready_deadline)
            std::this_thread::sleep_for(1ms);
        check(controller.snapshot()["pending"] == 1 && future.wait_for(0ms) == std::future_status::timeout,
              "concurrent caller waits for atomic slot ownership");
        first = {};
        check(future.get(), "lease release wakes queued caller");
        controller.configure(config);
    }
    {
        auto cancellation_config = config; cancellation_config["interactive_wait_ms"] = 10000;
        controller.configure(cancellation_config);
        auto first = controller.acquire(work());
        std::atomic<bool> cancelled{false};
        auto queued = work(); queued.cancelled = [&] { return cancelled.load(); };
        auto future = run_thread([&] {
            return error([&] { controller.acquire(queued); }, "request_cancelled");
        });
        const auto pending_deadline = std::chrono::steady_clock::now() + 5s;
        while (controller.snapshot()["pending"] != 1 && std::chrono::steady_clock::now() < pending_deadline)
            std::this_thread::sleep_for(1ms);
        cancelled = true;
        check(future.get(), "disconnected queued client is rejected promptly");
        check(controller.snapshot()["pending"] == 0, "cancelled waiter leaves no claim");
        first = {};
        controller.configure(config);
    }
    controller.pause(true);
    check(error([&] { controller.acquire(work()); }, "admission_paused"), "maintenance pause rejects new work");
    controller.pause(false);
    {
        auto lease = controller.acquire(work());
        check(bool(lease), "resume restores admission without model-policy changes");
        std::this_thread::sleep_for(105ms);
        check(lease.deadline_expired(), "completion deadline is distinct from queue wait");
    }
    {
        auto mismatch = work(); mismatch.options["ctx_size"] = 4096;
        check(error([&] { controller.acquire(mismatch); }, "admission_profile_mismatch"),
              "changed execution options cannot borrow old resource ceilings");
        auto missing = work(); missing.model = "unknown";
        check(error([&] { controller.acquire(missing); }, "admission_profile_unknown"),
              "protected mode rejects unmeasured profiles");
        auto state = work(); state.operation = ResourceAdmission::Operation::State;
        check(error([&] { controller.acquire(state); }, "operation_unqualified"),
              "unqualified operation is rejected before execution");
    }
    auto unknown = config; unknown["pools"]["uma"]["capacity_bytes"] = nullptr;
    controller.configure(unknown);
    check(error([&] { controller.acquire(work()); }, "capacity_unknown"), "unknown physical capacity is not free memory");
    auto zero = config; zero["pools"]["uma"]["capacity_bytes"] = 0;
    controller.configure(zero);
    check(error([&] { controller.acquire(work()); }, "capacity_exceeded"), "known zero rejects rather than using a fallback");
    controller.configure(config);
    {
        auto lease = controller.acquire(work());
        check(error([&] { controller.configure(zero); }, "admission_configuration_busy"),
              "resource topology cannot change under live reservations");
    }
    {
        std::optional<uint64_t> available = 200;
        ResourceAdmission sampled([&](const std::string&) { return available; });
        sampled.configure(config);
        check(error([&] { sampled.acquire(work()); }, "queue_deadline_exceeded"),
              "sampled physical shortage prevents admission below the configured ceiling");
        available = 900;
        auto first = sampled.acquire(work(1));
        auto second = sampled.acquire(work(2));
        check(error([&] { sampled.acquire(work(3)); }, "queue_deadline_exceeded"),
              "unmaterialized concurrent claims cannot reuse the same sampled headroom");
    }
    {
        json bounded = config;
        bounded["principal_pending_limit"] = 1;
        bounded["interactive_wait_ms"] = 300;
        ResourceAdmission queued; queued.configure(bounded);
        auto held = queued.acquire(work());
        auto future = run_thread([&] {
            return error([&] { queued.acquire(work()); }, "admission_paused");
        });
        for (int i = 0; i < 100 && queued.snapshot()["pending"] != 1; ++i) std::this_thread::sleep_for(1ms);
        check(error([&] { queued.acquire(work()); }, "queue_full"), "anonymous clients share the same bounded principal queue");
        queued.pause(true);
        check(future.get() && queued.snapshot()["pending"] == 0, "pause drains pending callers with bounded errors");
    }
    {
        auto legacy = json::object();
        ResourceAdmission compatibility; compatibility.configure(legacy);
        auto unmeasured = work(); unmeasured.model = "operator-approved-legacy";
        auto lease = compatibility.acquire(unmeasured);
        check(bool(lease) && compatibility.snapshot()["mode"] == "legacy", "legacy profiles remain explicitly unprotected");
    }
    {
        for (const bool explicit_unset : {false, true}) {
            auto automatic = config;
            automatic["models"]["model"].erase("concurrency");
            automatic["models"]["model"]["request_bytes"] = {{"uma", 100}};
            automatic["interactive_wait_ms"] = 10000;
            if (explicit_unset) {
                automatic["principal_pending_limit"] = nullptr;
                automatic["models"]["model"]["concurrency"] = nullptr;
            }
            ResourceAdmission no_quotas; no_quotas.configure(automatic);
            const auto optional = no_quotas.snapshot()["optional_limits"];
            check(optional["schema_version"] == 1 && optional["principal_pending_limit"].is_null() &&
                  optional["model_concurrency"]["model"].is_null(),
                  "missing and explicitly unset optional quotas are advertised without inventing identity or model caps");
            std::vector<ResourceAdmission::Lease> concurrent;
            for (int i = 0; i < 5; ++i) concurrent.push_back(no_quotas.acquire(work()));
            check(no_quotas.snapshot()["active"] == 5 && no_quotas.snapshot()["pools"]["uma"]["reserved_bytes"] == 500,
                  "five same-principal requests reserve incremental physical costs without a default model concurrency quota");
            concurrent.clear();
            check(no_quotas.snapshot()["active"] == 0, "unset optional quotas preserve terminal reservation cleanup");

            automatic["active_limit"] = 1;
            no_quotas.configure(automatic);
            auto held = no_quotas.acquire(work());
            std::vector<ThreadTask<bool>> pending;
            for (int i = 0; i < 12; ++i) pending.push_back(run_thread([&] {
                return error([&] { no_quotas.acquire(work()); }, "admission_paused");
            }));
            const auto deadline = std::chrono::steady_clock::now() + 5s;
            while (no_quotas.snapshot()["pending"] != 12 && std::chrono::steady_clock::now() < deadline)
                std::this_thread::sleep_for(1ms);
            const bool queued = no_quotas.snapshot()["pending"] == 12;
            no_quotas.pause(true);
            bool cancelled = true;
            for (auto& caller : pending) cancelled &= caller.get();
            check(queued && cancelled && no_quotas.snapshot()["pending"] == 0,
                  "more than eight same-principal requests use the shared bounded queue when its optional quota is unset");
        }
        auto capped = config;
        capped["principal_pending_limit"] = 3;
        ResourceAdmission explicit_caps; explicit_caps.configure(capped);
        const auto optional = explicit_caps.snapshot()["optional_limits"];
        check(optional["principal_pending_limit"] == 3 && optional["model_concurrency"]["model"] == 1,
              "explicit existing operator caps survive configuration and observation");
        for (const auto value : {json(0), json(-1), json(1.5), json(4097), json("auto")}) {
            for (const bool model : {false, true}) {
                auto invalid = config;
                if (model) invalid["models"]["model"]["concurrency"] = value;
                else invalid["principal_pending_limit"] = value;
                bool rejected = false;
                try { ResourceAdmission::validate_config(invalid); } catch (const std::invalid_argument&) { rejected = true; }
                check(rejected, "optional quota rejects an invalid explicit value rather than silently unsetting it");
            }
        }
    }
    {
        auto state_config = config;
        state_config["models"]["model"]["state_bytes"] = {{"uma", 50}};
        ResourceAdmission state_operations; state_operations.configure(state_config);
        auto state_work = work(); state_work.operation = ResourceAdmission::Operation::State;
        { auto state_lease = state_operations.acquire(state_work); state_lease.started(); }
        state_operations.reconcile_idle_requests(1);
        check(state_operations.snapshot()["uncertain"] == 1,
              "idle inference slots cannot certify a failed state export stopped");
        state_operations.reconcile_stopped_generation(1);
        check(state_operations.snapshot()["uncertain"] == 0, "worker termination releases uncertain state operations");
    }
    {
        auto fair_config = config;
        fair_config["active_limit"] = 1;
        fair_config["principal_pending_limit"] = 32;
        fair_config["service_pending_limit"] = 32;
        fair_config["interactive_wait_ms"] = 10000;
        fair_config["essential_wait_ms"] = 10000;
        fair_config["models"]["essential"] = profile;
        fair_config["models"]["essential"]["priority"] = "essential";
        ResourceAdmission fair; fair.configure(fair_config);
        auto held = fair.acquire(work());
        std::vector<ThreadTask<void>> waiters;
        std::vector<char> order;
        std::mutex order_mutex;
        for (int i = 0; i < 11; ++i) {
            waiters.push_back(run_thread([&, i] {
                auto candidate = work(); if (i < 10) candidate.model = "essential";
                auto granted = fair.acquire(candidate);
                std::lock_guard<std::mutex> lock(order_mutex);
                order.push_back(i < 10 ? 'E' : 'I');
            }));
        }
        const auto ready_deadline = std::chrono::steady_clock::now() + 5s;
        while (fair.snapshot()["pending"] != 11 && std::chrono::steady_clock::now() < ready_deadline)
            std::this_thread::sleep_for(1ms);
        const bool all_queued = fair.snapshot()["pending"] == 11;
        held = {};
        for (auto& waiter : waiters) waiter.get();
        auto ordinary = std::find(order.begin(), order.end(), 'I');
        check(all_queued && order.size() == 11 && ordinary != order.end() && std::distance(order.begin(), ordinary) <= 8,
              "eligible ordinary work runs after at most eight consecutive essential grants");
    }
    {
        auto race_config = config;
        race_config["models"]["model"]["concurrency"] = 8;
        race_config["principal_pending_limit"] = 32;
        race_config["service_pending_limit"] = 32;
        race_config["interactive_wait_ms"] = 10000;
        ResourceAdmission racing; racing.configure(race_config);
        std::atomic<int> running{0}, peak{0};
        std::vector<ThreadTask<void>> callers;
        for (int i = 0; i < 20; ++i) callers.push_back(run_thread([&] {
            auto lease = racing.acquire(work()); lease.started();
            int count = ++running, observed = peak;
            while (count > observed && !peak.compare_exchange_weak(observed, count)) {}
            std::this_thread::sleep_for(2ms);
            --running; lease.completed();
        }));
        for (auto& caller : callers) caller.get();
        check(peak <= 2 && racing.snapshot()["active"] == 0 && racing.snapshot()["pending"] == 0,
              "racing callers cannot oversubscribe one physical pool or leak reservations");
    }
    {
        int samples = 0;
        ResourceAdmission failing([&](const std::string&) -> std::optional<uint64_t> {
            if (++samples > 1) throw std::runtime_error("fixture sample failed");
            return 1000;
        });
        failing.configure(config);
        bool thrown = false;
        try { failing.acquire(work()); } catch (const std::runtime_error&) { thrown = true; }
        check(thrown && failing.snapshot()["pending"] == 0 && failing.snapshot()["active"] == 0,
              "sampling exceptions clean pending entries without partial claims");
    }
    {
        ResourceAdmission ingress([](const std::string&) { return std::optional<uint64_t>(1ULL << 30); });
        json bounds = {{"ingress", {{"request_bytes", 100}, {"total_bytes", 150}, {"active_limit", 2}}}};
        ingress.configure(bounds);
        auto first = ingress.acquire_ingress(80);
        check(ingress.snapshot()["ingress"]["reserved_body_bytes"] == 80 && ingress.snapshot()["active"] == 0,
              "body reservations are visible without consuming inference slots");
        check(error([&] { ingress.acquire_ingress(101); }, "ingress_limit_exceeded"), "oversized bodies reject before buffering");
        check(error([&] { ingress.acquire_ingress(71); }, "ingress_capacity_exceeded"), "aggregate body bytes cannot exceed the configured bound");
        auto second = ingress.acquire_ingress(1);
        check(error([&] { ingress.acquire_ingress(1); }, "ingress_capacity_exceeded"), "body count is independently bounded");
        auto smaller = bounds; smaller["ingress"]["request_bytes"] = 70;
        check(error([&] { ingress.configure(smaller); }, "admission_configuration_busy"), "configuration cannot discard a live body's byte bound");
        auto protected_bounds = bounds; protected_bounds["mode"] = "protected";
        ingress.configure(protected_bounds);
        check(ingress.snapshot()["ingress"]["reserved_body_bytes"] == 81, "compatible configuration retains existing body charges");
        first = {}; second = {};
        ingress.pause(true);
        auto control = ingress.acquire_ingress(100);
        check(bool(control), "paused execution still accepts bounded operator request bodies");
        control = {};
        check(ingress.snapshot()["ingress"]["active"] == 0, "HTTP ownership releases body credit without worker termination");
        ingress.shutdown();
        check(error([&] { ingress.acquire_ingress(1); }, "admission_paused"), "shutdown closes ingress admission");
        bool invalid = false;
        try { ResourceAdmission::validate_config({{"ingress", {{"request_bytes", ResourceAdmission::maximum_ingress_bytes + 1}}}}); }
        catch (const std::invalid_argument&) { invalid = true; }
        check(invalid, "configuration cannot exceed the transport decompression ceiling");
    }
    {
        ResourceAdmission bodies;
        bodies.configure({{"ingress", {{"request_bytes", 100}, {"total_bytes", 200}, {"active_limit", 1}}}});
        auto long_request = bodies.acquire_ingress(10);
        check(error([&] { bodies.acquire_ingress(1); }, "ingress_capacity_exceeded"), "incomplete bodies consume a read slot");
        long_request.completed();
        auto next = bodies.acquire_ingress(20);
        check(bodies.snapshot()["ingress"]["reading"] == 1 && bodies.snapshot()["ingress"]["reserved_body_bytes"] == 30,
              "completed reads free slots while retaining their byte charges through execution");
        auto control = bodies.acquire_ingress(100, true);
        check(bool(control) && bodies.snapshot()["ingress"]["control_reading"] == 1,
              "compact controls use reserved bytes and an independent read slot");
        check(error([&] { bodies.acquire_ingress(1, true); }, "ingress_capacity_exceeded"), "reserved control reading is bounded");
        next = {}; control = {};
        auto large = bodies.acquire_ingress(90);
        check(error([&] { bodies.acquire_ingress(1); }, "ingress_capacity_exceeded"), "retained bytes cannot spend the control reserve");
        check(bodies.snapshot()["ingress"]["reserved_body_bytes"] == 100, "retained body bytes remain charged alongside the next read");
        large = {}; long_request = {};
        check(bodies.snapshot()["ingress"]["active"] == 0, "all body charges release after their HTTP owners finish");
    }
    {
        std::optional<uint64_t> available = 400000;
        ResourceAdmission inputs([&](const std::string& pool) {
            return pool == "host" ? available : std::nullopt;
        });
        json bounds = {{"mode", "protected"}, {"pools", {{"host", {{"capacity_bytes", 400000},
            {"essential_headroom_bytes", 100000}, {"source", "controlled shared host pool"}}}}}};
        inputs.configure(bounds);
        auto first = inputs.acquire_ingress(100);
        auto second = inputs.acquire_ingress(100);
        check(inputs.snapshot()["pools"]["host"]["reserved_bytes"] ==
              inputs.snapshot()["ingress"]["reserved_host_bytes"] &&
              inputs.snapshot()["ingress"]["reserved_host_bytes"].get<uint64_t>() > 200,
              "input parsing and model work charge one physical host pool");
        check(error([&] { inputs.acquire_ingress(100); }, "ingress_capacity_exceeded"),
              "ordinary inputs cannot consume essential host headroom");
        first.completed();
        check(error([&] { inputs.acquire_ingress(100); }, "ingress_capacity_exceeded"),
              "completed reads retain their physical memory estimate");
        auto control = inputs.acquire_ingress(100, true);
        check(bool(control), "bounded recovery controls can use essential host headroom");
        auto smaller = bounds; smaller["pools"]["host"]["capacity_bytes"] = 200000;
        check(error([&] { inputs.configure(smaller); }, "admission_configuration_busy"),
              "configuration cannot publish a host pool smaller than live input estimates");
        first = {}; second = {}; control = {};
        available = 0;
        check(error([&] { inputs.acquire_ingress(1); }, "ingress_capacity_exceeded"),
              "known zero sampled memory is exhaustion");
        available.reset();
        check(error([&] { inputs.acquire_ingress(1); }, "ingress_memory_unknown"),
              "unknown sampled memory never becomes free input capacity");
        inputs.configure(json::object());
        auto legacy_body = inputs.acquire_ingress(1, true);
        const auto old_revision = inputs.snapshot()["configuration_revision"];
        check(error([&] { inputs.configure(bounds); }, "ingress_memory_unknown"),
              "configuration activation distinguishes unknown input headroom from known exhaustion");
        check(inputs.snapshot()["mode"] == "legacy" &&
              inputs.snapshot()["configuration_revision"] == old_revision,
              "unknown memory activation preserves the committed mode and revision");
        legacy_body = {};
        unsigned samples = 0;
        bool published = false;
        ResourceAdmission changing([&](const std::string&) -> std::optional<uint64_t> {
            return ++samples == 1 ? std::optional<uint64_t>(400000) : std::nullopt;
        });
        auto ordinary = changing.acquire_ingress(1);
        const auto before = changing.snapshot()["configuration_revision"];
        bool unknown_status = false;
        try { changing.configure(bounds, [&] { published = true; }); }
        catch (const lemon::AdmissionError& e) {
            unknown_status = e.code == "ingress_memory_unknown" && e.status == 503;
        }
        check(unknown_status && samples == 2 && !published,
              "unknown second activation sample returns503 without publishing");
        check(changing.snapshot()["mode"] == "legacy" &&
              changing.snapshot()["configuration_revision"] == before &&
              changing.snapshot()["ingress"]["active"] == 1,
              "failed activation preserves revision and ordinary body ownership");
        available = 400000;
        auto unknown = bounds; unknown["pools"]["host"]["capacity_bytes"] = nullptr;
        inputs.configure(unknown);
        check(error([&] { inputs.acquire_ingress(1); }, "ingress_memory_unknown"),
              "an explicitly unknown host ceiling remains unknown with a known sample");
        inputs.configure({{"mode", "protected"}});
        first = inputs.acquire_ingress(100);
        second = inputs.acquire_ingress(100);
        auto third = inputs.acquire_ingress(100);
        check(error([&] { inputs.acquire_ingress(100); }, "ingress_capacity_exceeded"),
              "sampler-only input admission subtracts all outstanding physical estimates");
        first = {}; second = {}; third = {};
        check(inputs.snapshot()["ingress"]["reserved_host_bytes"] == 0,
              "local response completion releases physical input ownership");
        ResourceAdmission unobserved;
        unobserved.configure({{"mode", "protected"}});
        check(error([&] { unobserved.acquire_ingress(1); }, "ingress_memory_unknown"),
              "protected input requires a known physical memory sampler");
    }
    {
        std::optional<uint64_t> available = 400000;
        ResourceAdmission shared([&](const std::string&) { return available; });
        auto host_profile = profile;
        host_profile["request_bytes"] = {{"host", 240000}};
        host_profile["load_bytes"] = {{"host", 250000}};
        host_profile["resident_bytes"] = {{"host", 200000}};
        shared.configure({{"mode", "protected"}, {"interactive_wait_ms", 30},
            {"pools", {{"host", {{"capacity_bytes", 400000}, {"source", "shared physical sample"}}}}},
            {"models", {{"model", host_profile}}}});
        auto body = shared.acquire_ingress(100);
        auto request = shared.acquire(work());
        check(shared.snapshot()["pools"]["host"]["reserved_bytes"].get<uint64_t>() > 240000,
              "a model request and its input estimate coexist in one pool");
        check(error([&] { shared.acquire_ingress(100); }, "ingress_capacity_exceeded"),
              "outstanding model work constrains subsequent input admission");
        request = {}; body = {};
        auto large = shared.acquire_ingress(400);
        check(error([&] { shared.acquire(work()); }, "queue_deadline_exceeded"),
              "outstanding input estimates constrain subsequent model admission");
    }
    controller.shutdown();
    check(error([&] { controller.acquire(work()); }, "admission_paused"), "shutdown rejects new requests");
    {
        auto invalid = config;
        invalid["models"]["model"]["options"]["ctx_size"] = 0;
        bool rejected = false;
        try { ResourceAdmission::validate_config(invalid); }
        catch (const std::invalid_argument&) { rejected = true; }
        check(rejected, "model-native zero context is not a protected resource ceiling");
        ResourceAdmission bounded; bounded.configure(config);
        auto oversized = work(); oversized.workload["output_tokens"] = 129;
        check(error([&] { bounded.acquire(oversized); }, "workload_limit_exceeded"),
              "oversized output is rejected before acquiring capacity");
        auto unbounded = work(); unbounded.workload.erase("batch");
        check(error([&] { bounded.acquire(unbounded); }, "workload_unqualified"),
              "missing workload dimensions cannot use a fixed reservation");
    }
    {
        std::optional<uint64_t> available = 400;
        ResourceAdmission sampled([&](const std::string&) { return available; });
        sampled.configure(config);
        check(error([&] { sampled.acquire(work()); }, "queue_deadline_exceeded"),
              "external memory pressure cannot consume the essential sampled reserve");
        available = 1000;
        auto resident_work = work(); resident_work.operation = ResourceAdmission::Operation::Load;
        auto resident = sampled.acquire(resident_work); resident.started(); resident.retain_residency();
        available = 600;
        auto request = sampled.acquire(work());
        check(sampled.snapshot()["pools"]["uma"]["reserved_bytes"] == 700,
              "materialized residency is not subtracted twice from sampled availability");
        available = 350;
        check(error([&] { sampled.acquire(work(2)); }, "queue_deadline_exceeded"),
              "outstanding claims and pressure preserve essential headroom together");
    }
    {
        ResourceAdmission transitions; transitions.configure(config);
        auto stale = work(); stale.config_revision = transitions.policy().revision;
        auto replacement = config; replacement["interactive_wait_ms"] = 121;
        transitions.configure(replacement);
        check(error([&] { transitions.acquire(stale); }, "admission_configuration_changed"),
              "preflight from an older policy cannot enter a new admission revision");
        const auto revision = transitions.policy().revision;
        bool failed = false;
        try { transitions.configure(config, [] { throw std::runtime_error("persistence fixture"); }); }
        catch (const std::runtime_error&) { failed = true; }
        check(failed && transitions.policy().revision == revision && !transitions.snapshot()["configuration_pending"].get<bool>(),
              "failed publication leaves the old policy intact and clears the transition gate");
        std::promise<void> publishing, release;
        auto release_future = release.get_future();
        auto update = run_thread([&] {
            transitions.configure(config, [&] { publishing.set_value(); release_future.wait(); });
        });
        publishing.get_future().wait();
        check(error([&] { transitions.acquire(work()); }, "admission_configuration_pending") &&
              transitions.snapshot()["configuration_revision"] == revision,
              "durable publication gates new work while inspection remains responsive");
        check(error([&] { transitions.configure(replacement); }, "admission_configuration_busy"),
              "concurrent configuration cannot overtake an in-progress publication");
        release.set_value(); update.get();
        check(transitions.policy().revision == revision + 1,
              "successful publication advances one policy revision");
    }
    {
        ResourceAdmission management;
        json legacy = {{"mode", "legacy"}, {"active_limit", 1}};
        management.configure(legacy);
        auto view = management;
        {
            auto mutation = view.acquire_management("asset", "model-acquisition");
            check(management.snapshot()["active"] == 1,
                  "native management views retain one shared ledger lifetime");
            check(error([&] { management.configure(config); }, "admission_configuration_busy"),
                  "protected configuration cannot overtake an active legacy mutation");
            auto request = management.acquire(work());
            check(management.snapshot()["active"] == 2,
                  "legacy management tracking does not consume inference concurrency capacity");
        }
        check(management.snapshot()["active"] == 0 && management.snapshot()["uncertain"] == 0,
              "finished synchronous legacy management releases its transition ticket");
        management.pause(true);
        check(error([&] { view.acquire_management("asset", "runtime-install"); }, "admission_paused"),
              "pause rejects new management work through every shared view");
        management.pause(false);
        management.configure(config);
        check(error([&] { view.acquire_management("model", "model-acquisition"); }, "operation_unqualified"),
              "protected profiles cannot authorize unqualified acquisition through model estimates");
        management.shutdown();
        check(error([&] { view.acquire_management("asset", "runtime-uninstall"); }, "admission_paused"),
              "retained management views preserve owner shutdown rejection");
    }
    {
        ResourceAdmission legacy;
        ResourceAdmission::Work load;
        load.model = "legacy-native-process";
        load.operation = ResourceAdmission::Operation::Load;
        load.generation = 707;
        { auto process = legacy.acquire(load); process.started(); }
        check(legacy.snapshot()["uncertain"] == 1 && legacy.snapshot()["active"] == 1,
              "unconfirmed zero-cost native loads retain logical ownership after release");
        check(error([&] { legacy.configure({{"mode", "protected"}}); }, "admission_configuration_busy"),
              "unconfirmed native ownership blocks promotion even without a physical estimate");
        legacy.reconcile_stopped_generation(707);
        check(legacy.snapshot()["active"] == 0,
              "exact native generation stop releases unestimated logical ownership");
    }
    {
        ResourceAdmission management;
        { auto writer = management.acquire_management("preparation", "helper"); writer.started(); }
        check(management.snapshot()["uncertain"] == 1 && management.snapshot()["active"] == 1,
              "dispatched zero-cost management work retains uncertainty after owner destruction");
        management.reconcile_idle_requests(0);
        check(management.snapshot()["uncertain"] == 1,
              "idle inference cannot certify an unresolved helper writer stopped");
        check(error([&] { management.configure({{"mode", "protected"}}); }, "admission_configuration_busy"),
              "unresolved helper ownership blocks configuration promotion");
    }
    {
        for (const std::string operation : {"runtime-install", "runtime-uninstall"}) {
            ResourceAdmission mutations;
            {
                auto accepted = mutations.acquire_management("llamacpp:cpu", operation);
                check(error([&] { mutations.acquire_management("llamacpp:cpu", "runtime-install"); }, "runtime_reference_busy") &&
                      error([&] { mutations.acquire_management("llamacpp:cpu", "runtime-uninstall"); }, "runtime_reference_busy"),
                      "accepted package mutations exclude install and uninstall through their whole operation");
                auto other = mutations.acquire_management("llamacpp:vulkan", "runtime-install");
                check(bool(other), "package mutation exclusion is scoped to the affected runtime");
            }
            auto retry = mutations.acquire_management("llamacpp:cpu", operation);
            check(bool(retry), "terminal synchronous package mutation releases matching retry");
            retry.started();
        }
        ResourceAdmission uncertain;
        { auto accepted = uncertain.acquire_management("llamacpp:cpu", "runtime-install"); accepted.started(); }
        check(uncertain.snapshot()["uncertain"] == 1 &&
              error([&] { uncertain.acquire_management("llamacpp:cpu", "runtime-install"); }, "runtime_reference_busy") &&
              error([&] { uncertain.acquire_management("llamacpp:cpu", "runtime-uninstall"); }, "runtime_reference_busy"),
              "unconfirmed package mutation retains exclusion after its initiating owner disappears");
    }
    {
        ResourceAdmission references;
        {
            auto runtime = references.acquire_management("flm:npu", "runtime-reference"); runtime.started();
            check(error([&] { references.acquire_management("flm:npu", "runtime-install"); }, "runtime_reference_busy") &&
                  error([&] { references.acquire_management("flm:npu", "runtime-uninstall"); }, "runtime_reference_busy"),
                  "an accepted runtime reference excludes install and uninstall of the same managed tree");
            auto unrelated = references.acquire_management("llamacpp:cpu", "runtime-install");
            check(bool(unrelated), "runtime references preserve unrelated package operations");
            runtime.completed();
        }
        {
            auto mutation = references.acquire_management("flm:npu", "runtime-install");
            check(error([&] { references.acquire_management("flm:npu", "runtime-reference"); }, "runtime_reference_busy"),
                  "runtime mutation prevents a helper from acquiring its files during replacement");
        }
        {
            auto runtime = references.acquire_management("flm:npu", "runtime-reference");
            check(bool(runtime), "confirmed reference and mutation completion restore acquisition");
            runtime.started();
        }
        check(references.snapshot()["uncertain"] == 1 &&
              error([&] { references.acquire_management("flm:npu", "runtime-uninstall"); }, "runtime_reference_busy"),
              "destroying an unresolved runtime owner does not authorize reclamation");
    }
    {
        ResourceAdmission slots;
        slots.configure({{"interactive_wait_ms", 100}});
        auto physical = [](uint64_t request_id, uint64_t generation = 900) {
            ResourceAdmission::Work w;
            w.model = "physical"; w.service = "chat"; w.generation = generation;
            w.request_id = request_id; w.execution_slots = {7};
            return w;
        };
        auto parent_work = physical(1); parent_work.operation = ResourceAdmission::Operation::State;
        auto parent = slots.acquire(parent_work);
        check(parent.execution_slot() == 7, "state request reserves the actual nonzero slot ID");
        {
            auto inference = slots.acquire(physical(1));
            check(inference.execution_slot() == parent.execution_slot(), "nested inference shares its own state slot");
            check(slots.snapshot()["physical_slots"][0]["claimed"] == json::array({7}),
                  "nested tickets count one physical claim");
            check(error([&] { slots.acquire(physical(1)); }, "execution_slots_busy"),
                  "parallel child requests cannot both borrow the same state slot");
            check(error([&] { slots.acquire(physical(2)); }, "execution_slots_busy"),
                  "another request waits for physical capacity and returns a specific deadline reason");
        }
        auto wrong_principal = physical(1); wrong_principal.principal = "different";
        check(error([&] { slots.acquire(wrong_principal); }, "execution_slots_busy"),
              "matching request ID cannot borrow another principal's state slot");
        auto requested = physical(2); requested.requested_slot = 11;
        check(error([&] { slots.acquire(requested); }, "execution_slot_unknown"),
              "a requested slot must belong to the observed inventory");
        auto changed = physical(2); changed.execution_slots = {7, 11};
        check(error([&] { slots.acquire(changed); }, "execution_slots_changed"),
              "one generation cannot publish conflicting slot inventories");
        auto other_generation = slots.acquire(physical(2, 901));
        check(other_generation.execution_slot() == 7, "slot identity includes the exact worker generation");
        std::atomic<bool> cancelled{false};
        auto waiting = physical(3); waiting.cancelled = [&] { return cancelled.load(); };
        auto cancellation = run_thread([&] { return error([&] { slots.acquire(waiting); }, "request_cancelled"); });
        const auto cancel_until = std::chrono::steady_clock::now() + 5s;
        while (slots.snapshot()["pending"] != 1 && std::chrono::steady_clock::now() < cancel_until)
            std::this_thread::sleep_for(1ms);
        cancelled = true;
        check(cancellation.get() && slots.snapshot()["pending"] == 0,
              "cancelled physical wait removes only its queued claim");
        parent = {};
        other_generation = {};
        slots.configure({{"interactive_wait_ms", 10000}});
        {
            auto two = physical(8); two.execution_slots = {7, 11};
            auto left = slots.acquire(two);
            two.request_id = 9;
            auto right = slots.acquire(two);
            check(left.execution_slot() == 7 && right.execution_slot() == 11,
                  "two concurrent requests reserve distinct observed slot IDs");
        }
        auto first = slots.acquire(physical(10));
        std::mutex order_mutex; std::vector<int> order;
        std::vector<ThreadTask<int>> waiters;
        for (int i = 1; i <= 5; ++i) {
            waiters.push_back(run_thread([&, i] {
                auto next = slots.acquire(physical(10 + i));
                std::lock_guard<std::mutex> owner(order_mutex);
                order.push_back(i);
                return next.execution_slot().value_or(-1);
            }));
            const auto until = std::chrono::steady_clock::now() + 5s;
            while (slots.snapshot()["pending"] != i && std::chrono::steady_clock::now() < until)
                std::this_thread::sleep_for(1ms);
            check(slots.snapshot()["pending"] == i, "additional sessions join the existing bounded queue");
        }
        first = {};
        bool all_served = true;
        for (auto& waiter : waiters) all_served &= waiter.get() == 7;
        check(all_served && order == std::vector<int>({1, 2, 3, 4, 5}),
              "five requests from the same principal receive one physical slot in FIFO order");
        check(slots.snapshot()["pending"] == 0 && slots.snapshot()["physical_slots"].empty(),
              "completed physical claims leave no slot reservations");
        slots.configure({{"interactive_wait_ms", 30}});
        auto uncertain = slots.acquire(physical(30)); uncertain.started(); uncertain = {};
        check(slots.snapshot()["uncertain"] == 1, "legacy dispatch still retains an unresolved physical claim");
        check(error([&] { slots.acquire(physical(31)); }, "execution_slots_busy"),
              "uncertain work cannot return a physical slot to another request");
        slots.reconcile_idle_requests(901);
        check(slots.snapshot()["uncertain"] == 1, "idle observation for another generation cannot release the slot");
        slots.reconcile_idle_requests(900);
        check(slots.snapshot()["uncertain"] == 0 && bool(slots.acquire(physical(32))),
              "confirmed idle request observation restores the exact generation's slot");
    }
    {
        ResourceAdmission running;
        ResourceAdmission::Work request;
        request.model = "terminal-publication";
        auto accepted_request = running.acquire(request);
        accepted_request.started(); accepted_request.completed();
        running.maintenance(true);
        check(!running.wait_for_writes(1ms), "completed device work cannot hide a still-owned request finalizer from draining");
        accepted_request = {};
        check(running.wait_for_writes(100ms), "terminal request release completes its participating operation");
        ResourceAdmission cancellation;
        ResourceAdmission::WriteToken cancelled_writer;
        { auto writer = cancellation.write_scope("cancelled-deferred"); cancelled_writer = writer.continuation(); }
        ResourceAdmission::Work cancelled_work;
        cancelled_work.cancelled = [&cancelled_writer] { cancelled_writer = {}; return true; };
        check(error([&] { cancellation.acquire(std::move(cancelled_work)); }, "request_cancelled") &&
              cancellation.wait_for_writes(50ms), "cancellation releases the final continuation without reentering the ledger mutex");
        ResourceAdmission::WriteToken terminal_writer;
        { auto writer = cancellation.write_scope("terminal-deferred"); terminal_writer = writer.continuation(); }
        ResourceAdmission::Work terminal;
        terminal.operation = ResourceAdmission::Operation::Management;
        terminal.cancelled = [token = std::move(terminal_writer)] { return false; };
        auto terminal_lease = cancellation.acquire(std::move(terminal));
        // A moved-from std::function may retain a copy of its callable.
        terminal.cancelled = {};
        terminal_lease.started(); terminal_lease.completed(); terminal_lease = {};
        check(cancellation.wait_for_writes(50ms) && cancellation.snapshot()["writes"]["active_operations"] == 0,
              "terminal ledger cleanup can release its continuation while holding the ledger mutex");
        ResourceAdmission restoration;
        auto restore_config = config; restore_config["interactive_wait_ms"] = 10000;
        restoration.configure(restore_config);
        auto load_work = work(701); load_work.operation = ResourceAdmission::Operation::Load;
        auto occupied = restoration.acquire(load_work);
        ResourceAdmission::WriteToken accepted_mutation;
        { auto operation = restoration.write_scope("profile-reload"); accepted_mutation = operation.continuation(); }
        auto restoring = run_thread([&, token = accepted_mutation] {
            auto publication = token.enter();
            auto second = load_work; second.generation = 702;
            auto loaded = restoration.acquire(second);
            return loaded.execution_slot() == std::nullopt;
        });
        const auto waiting_until = std::chrono::steady_clock::now() + 5s;
        while (restoration.snapshot()["pending"] != 1 && std::chrono::steady_clock::now() < waiting_until)
            std::this_thread::sleep_for(1ms);
        restoration.maintenance(true);
        check(restoration.snapshot()["pending"] == 1 && error([&] { restoration.acquire(load_work); }, "maintenance_active"),
              "maintenance preserves a qualified accepted mutation's queued load while rejecting a fresh load");
        occupied = {};
        check(restoring.get(), "accepted restoration still obeys the physical queue and completes after capacity returns");
        {
            auto publication = accepted_mutation.enter();
            auto inference = work(703);
            check(error([&] { restoration.acquire(inference); }, "maintenance_active"),
                  "a native writer continuation cannot authorize fresh user inference during maintenance");
            load_work.options = {{"ctx_size", 4096}};
            check(error([&] { restoration.acquire(load_work); }, "admission_profile_mismatch"),
                  "accepted reload continuation cannot bypass committed resource profile identity");
        }
        accepted_mutation = {};
        check(restoration.wait_for_writes(100ms), "terminal accepted restoration releases its writer and queue ownership");
        ResourceAdmission writes;
        {
            ResourceAdmission storage;
            bool valid = true;
            int validations = 0;
            storage.set_storage_validator([&] {
                ++validations;
                storage.snapshot();
                if (!valid) throw lemon::AdmissionError("storage_root_unavailable", "fixture", 503);
            });
            ResourceAdmission::WriteToken retained;
            {
                auto writer = storage.write_scope("accepted-before-replacement");
                retained = writer.continuation();
                valid = false;
                check(error([&] { storage.write_scope("nested-after-replacement"); }, "storage_root_unavailable"),
                      "nested mutation revalidates storage outside the ledger mutex");
            }
            check(error([&] { storage.write_scope("new-after-replacement"); }, "storage_root_unavailable"),
                  "new mutation rejects completed root replacement");
            auto publication = retained.enter();
            check(error([&] { publication.validate_storage(); }, "storage_root_unavailable") &&
                  !storage.wait_for_writes(1ms),
                  "deferred publication rejects changed storage without dropping its operation owner");
            auto process = storage.acquire_management("fixture", "cleanup");
            process.started(); process.completed(); process = {};
            check(storage.snapshot()["uncertain"] == 0 && validations == 4,
                  "storage rejection leaves process retirement available and does not inspect roots under ledger locks");
            valid = true;
            publication.validate_storage();
            { auto resumed = storage.write_scope("restored-original-root"); }
            check(validations == 6, "restoring original root identity permits publication and new mutation");
        }
        writes.configure({{"active_limit", 1}});
        ResourceAdmission::WriteToken deferred;
        {
            auto operation = writes.write_scope("configuration");
            auto nested = writes.write_scope("aliases");
            check(writes.snapshot()["writes"]["active_operations"] == 1,
                  "nested native writers retain one accepted operation");
            ResourceAdmission::Work work; work.model = "independent";
            auto inference = writes.acquire(work);
            check(writes.snapshot()["active"] == 1 && writes.snapshot()["pending"] == 0,
                  "metadata lifetimes consume no inference slot or user quota");
            deferred = nested.continuation();
        }
        writes.maintenance(true);
        check(!writes.wait_for_writes(1ms) && writes.snapshot()["writes"]["active_operations"] == 1,
              "a deferred writer retains accepted ownership after its initiating stack returns");
        check(error([&] { writes.write_scope("new-mutation"); }, "maintenance_active"),
              "closed writer admission rejects fresh native mutations");
        auto resumed = run_thread([token = deferred, &writes]() mutable {
            auto continued = token.enter();
            auto publication = writes.write_scope("deferred-publication");
            return writes.snapshot()["writes"]["active_operations"] == 1;
        });
        check(resumed.get(), "explicit continuation authorizes terminal publication on another thread during drain");
        deferred = {};
        check(writes.wait_for_writes(100ms) && writes.snapshot()["writes"]["active_operations"] == 0,
              "only final continuation release completes writer draining");
        writes.pause(false);
        check(error([&] { ResourceAdmission::Work work; writes.acquire(work); }, "maintenance_active"),
              "ordinary admission resume cannot bypass writer maintenance");
        writes.maintenance(false);
        {
            auto operation = writes.write_scope("failed-metadata");
            try { auto nested = writes.write_scope("nested-failure"); throw std::runtime_error("fixture"); }
            catch (const std::runtime_error&) {}
            check(writes.snapshot()["writes"]["active_operations"] == 1,
                  "failed nested publication cannot release its still-running outer operation");
        }
        check(writes.wait_for_writes(100ms), "synchronous writer failure leaves no active thread context");
        {
            auto operation = writes.write_scope("configuration");
            writes.configure({{"active_limit", 2}});
            check(writes.snapshot()["writes"]["active_operations"] == 1,
                  "admission configuration publishes inside its own accepted writer operation");
            auto other = run_thread([&] { return error([&] { writes.configure({{"active_limit", 3}}); }, "admission_configuration_busy"); });
            check(other.get(), "configuration cannot cross another active writer operation");
        }
        { auto helper = writes.acquire_management("unknown", "helper"); helper.started(); }
        writes.maintenance(true);
        check(!writes.wait_for_writes(1ms), "uncertain helper ownership prevents a false drained-writer result");
    }
    std::printf("Resource admission: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
