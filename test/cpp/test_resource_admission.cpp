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
        ResourceAdmission ingress;
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
    std::printf("Resource admission: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
