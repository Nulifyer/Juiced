#include "lemon/wrapped_server.h"
#include <cstdio>
#include <condition_variable>
#include <future>
#include <type_traits>
#include <vector>

class LeaseServer : public lemon::WrappedServer {
public:
    LeaseServer() : WrappedServer("lease-fixture", "error") { set_state(lemon::ModelState::READY); }
    void load(const std::string&, const lemon::ModelInfo&, const lemon::RecipeOptions&, bool) override {}
    void unload() override {}
    void restore() override {
        if (fail_restore) throw std::runtime_error("fixture restore failure");
    }
    bool fail_restore = false;
    int references() {
        std::lock_guard<std::mutex> lock(state_mutex_);
        return active_request_count_;
    }
};

int main() {
    using Lease = lemon::WrappedServer::InferenceLease;
    static_assert(!std::is_copy_constructible_v<Lease> && std::is_nothrow_move_constructible_v<Lease>);
    int failed = 0, checks = 0;
    auto check = [&](bool condition, const char* name) {
        ++checks;
        if (!condition) { std::fprintf(stderr, "FAIL: %s\n", name); ++failed; }
    };
    {
        int started = 0;
        lemon::WrappedServer::RequestCompletionScope outcome([&] { ++started; });
        check(!outcome.completed(), "no transfer is not proof of upstream completion");
        lemon::WrappedServer::note_request_started(); lemon::WrappedServer::note_request_completed();
        check(outcome.completed(), "one confirmed transfer is complete");
        lemon::WrappedServer::note_request_started();
        check(!outcome.completed(), "an unfinished later transfer invalidates completion");
        {
            lemon::WrappedServer::RequestCompletionScope nested;
            lemon::WrappedServer::note_request_started(); lemon::WrappedServer::note_request_completed();
            check(nested.completed(), "nested outcome has independent transfer ownership");
        }
        check(!outcome.completed(), "nested success cannot release unfinished outer work");
        lemon::WrappedServer::note_request_completed();
        check(outcome.completed(), "all started transfers must complete before release");
        lemon::WrappedServer::note_request_started(); lemon::WrappedServer::note_request_completed();
        check(started == 3 && outcome.completed(), "nested scopes restore the real dispatch observer");
    }
    {
        LeaseServer server;
        {
            auto lease = server.try_acquire_inference();
            check(bool(lease) && server.references() == 1 && server.is_busy(), "claim marks worker busy");
            check(!server.try_evict_if_idle(), "owned worker cannot be evicted");
        }
        check(server.references() == 0 && !server.is_busy(), "normal scope releases exactly once");
        check(server.try_evict_if_idle(), "released worker can be evicted");
        auto failed_lease = server.try_acquire_inference();
        check(!failed_lease && server.references() == 0, "failed acquire owns no reference");
    }
    {
        LeaseServer server;
        try {
            auto lease = server.try_acquire_inference();
            throw std::runtime_error("inference failure");
        } catch (const std::runtime_error&) {}
        check(server.references() == 0, "exception unwinding releases worker");
        try {
            auto lease = server.try_acquire_inference();
            lease.release();
            lease.release();
            throw std::runtime_error("reload failure after release");
        } catch (const std::runtime_error&) {}
        check(server.references() == 0 && !server.is_busy(), "reload failure cannot release twice");
    }
    {
        LeaseServer a, b;
        auto first = a.try_acquire_inference();
        auto moved = std::move(first);
        check(!first && moved && a.references() == 1, "move transfers sole ownership");
        auto second = b.try_acquire_inference();
        moved = std::move(second);
        check(a.references() == 0 && b.references() == 1 && !second, "move assignment releases its old worker");
        auto* alias = &moved;
        moved = std::move(*alias);
        check(moved && b.references() == 1, "self move does not release active ownership");
    }
    {
        LeaseServer server;
        server.set_state(lemon::ModelState::DOWNSIZED);
        server.fail_restore = true;
        bool caught = false;
        try { auto lease = server.try_acquire_inference(); }
        catch (const std::runtime_error&) { caught = true; }
        check(caught && server.references() == 0 && server.get_state() == lemon::ModelState::DOWNSIZED,
              "failed restore cannot strand loading state or claim a reference");
        server.fail_restore = false;
        auto lease = server.try_acquire_inference();
        check(lease && server.references() == 1, "restored worker can be claimed after a failed attempt");
    }
    {
        auto server = std::make_unique<LeaseServer>();
        auto lease = server->try_acquire_inference();
        lease.release();
        server.reset();
        lease.release();
        check(!lease, "explicit release permits worker destruction before lease destruction");
    }
    {
        LeaseServer server;
        auto lease = server.try_acquire_inference();
        std::atomic<bool> cancelled{true};
        check(cancelled && server.references() == 1, "cancel acknowledgement alone does not destroy ownership");
        lease.release();
        check(server.references() == 0, "owner releases after its terminal boundary");
    }
    {
        LeaseServer server;
        std::atomic<int> notified{0};
        server.set_reclaim_notifier([&] {
            // This also proves notification is outside the worker state mutex.
            if (server.references() == 0) ++notified;
            throw std::runtime_error("fixture reclaim failure");
        });
        {
            auto lease = server.try_acquire_inference();
            server.mark_pending_stale_if_busy();
        }
        check(notified == 1 && server.references() == 0, "failed notifier cannot terminate cleanup or repeat release");
    }
    {
        LeaseServer server;
        constexpr int count = 32;
        std::mutex mutex;
        std::condition_variable cv;
        int claimed = 0;
        bool release = false;
        std::vector<std::thread> workers;
        for (int i = 0; i < count; ++i) workers.emplace_back([&] {
            auto lease = server.try_acquire_inference();
            std::unique_lock<std::mutex> lock(mutex);
            ++claimed; cv.notify_all();
            cv.wait(lock, [&] { return release; });
        });
        {
            std::unique_lock<std::mutex> lock(mutex);
            const bool ready = cv.wait_for(lock, std::chrono::seconds(10), [&] { return claimed == count; });
            check(ready && server.references() == count && !server.try_evict_if_idle(),
                  "concurrent claims retain every active reference");
            release = true;
        }
        cv.notify_all();
        for (auto& worker : workers) worker.join();
        check(server.references() == 0 && server.try_evict_if_idle(), "concurrent release reconciles to idle once");
    }
    {
        auto server = std::make_shared<LeaseServer>();
        std::weak_ptr<LeaseServer> observer = server;
        auto lease = server->try_acquire_inference();
        server.reset();
        check(!observer.expired(), "managed lease retains the exact worker generation after registry ownership is removed");
        lease.release();
        check(observer.expired(), "releasing the final lease destroys the retired worker exactly once");
    }
    {
        auto server = std::make_shared<LeaseServer>();
        auto operation = server->try_acquire_ready_state_operation();
        check(operation && server->is_busy() && server->references() == 1 && server->active_inference_count() == 0,
              "state ownership blocks eviction without inventing an inference request");
        {
            auto inference = server->try_acquire_inference();
            check(server->references() == 2 && server->active_inference_count() == 1,
                  "nested inference and state ownership keep separate activity accounting");
        }
        check(server->is_busy() && server->active_inference_count() == 0,
              "state save remains protected after native inference finishes");
        operation.release();
        check(!server->is_busy() && server->references() == 0,
              "final state operation releases residency ownership");
    }
    {
        auto server = std::make_shared<LeaseServer>();
        auto borrowed_generation = server;
        std::promise<void> retired, teardown;
        auto teardown_allowed = teardown.get_future();
        auto retiring = std::async(std::launch::async, [&] {
            const bool owned = server->try_evict_if_idle();
            retired.set_value(); teardown_allowed.wait();
            server->WrappedServer::unload();
            return owned;
        });
        retired.get_future().wait();
        check(!borrowed_generation->try_acquire_inference() && !borrowed_generation->try_acquire_ready_inference(),
              "atomic retirement blocks borrowed generations before physical teardown");
        teardown.set_value();
        check(retiring.get() && server->get_state() == lemon::ModelState::UNLOADED,
              "default teardown preserves the retired state");
        server->set_state(lemon::ModelState::READY);
        auto acquired = server->try_acquire_inference();
        check(acquired && !server->try_evict_if_idle(),
              "inference acquisition winning the race prevents retirement");
    }
    std::printf("Inference lease checks: %d checks, %d failures\n", checks, failed);
    return failed ? 1 : 0;
}
