#include "lemon/config_file.h"
#include "lemon/error_types.h"
#include "lemon/model_manager.h"
#include "lemon/realtime_session.h"
#include "lemon/router.h"
#include "lemon/runtime_config.h"
#include "lemon/utils/path_utils.h"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <filesystem>
#include <thread>

#ifdef _WIN32
#include <ws2tcpip.h>
#else
#include <arpa/inet.h>
#include <sys/socket.h>
#include <unistd.h>
#endif

namespace {
#ifdef _WIN32
constexpr SOCKET invalid_socket = INVALID_SOCKET;
void close_socket(SOCKET fd) { closesocket(fd); }
void stop_socket(SOCKET fd) { shutdown(fd, SD_BOTH); }
#else
constexpr SOCKET invalid_socket = -1;
void close_socket(SOCKET fd) { close(fd); }
void stop_socket(SOCKET fd) { shutdown(fd, SHUT_RDWR); }
#endif

class LocalStream {
public:
    LocalStream() {
#ifdef _WIN32
        WSADATA data;
        if (WSAStartup(MAKEWORD(2, 2), &data)) throw std::runtime_error("WSA startup failed");
#endif
        listener_ = socket(AF_INET, SOCK_STREAM, 0);
        if (listener_ == invalid_socket) throw std::runtime_error("Fixture socket failed");
        sockaddr_in address{};
        address.sin_family = AF_INET;
        address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        if (bind(listener_, reinterpret_cast<sockaddr*>(&address), sizeof(address)) || listen(listener_, 8))
            throw std::runtime_error("Fixture bind failed");
#ifdef _WIN32
        int length = sizeof(address);
#else
        socklen_t length = sizeof(address);
#endif
        getsockname(listener_, reinterpret_cast<sockaddr*>(&address), &length);
        address_ = "tcp://127.0.0.1:" + std::to_string(ntohs(address.sin_port));
        acceptor_ = std::thread([this] {
            while (!stopping_) {
                fd_set ready;
                FD_ZERO(&ready);
                FD_SET(listener_, &ready);
                timeval timeout{0, 100000};
                if (select(static_cast<int>(listener_ + 1), &ready, nullptr, nullptr, &timeout) <= 0) continue;
                auto fd = accept(listener_, nullptr, nullptr);
                if (fd == invalid_socket) continue;
                auto peer = std::make_unique<Peer>();
                peer->fd = fd;
                auto* raw = peer.get();
                peer->reader = std::thread([this, raw, fd] {
                    ++connected;
                    char buffer[4096];
                    while (recv(fd, buffer, sizeof(buffer), 0) > 0) {}
                    std::lock_guard<std::mutex> lock(mutex_);
                    raw->fd = invalid_socket;
                    close_socket(fd);
                    ++closed;
                });
                peers_.push_back(std::move(peer));
            }
        });
    }
    ~LocalStream() {
        stopping_ = true;
        acceptor_.join();
        close_socket(listener_);
        {
            std::lock_guard<std::mutex> lock(mutex_);
            for (auto& peer : peers_) if (peer->fd != invalid_socket) stop_socket(peer->fd);
        }
        for (auto& peer : peers_) peer->reader.join();
    }
    const std::string& address() const { return address_; }
    std::atomic<int> connected{0}, closed{0};
private:
    struct Peer { SOCKET fd = invalid_socket; std::thread reader; };
    SOCKET listener_ = invalid_socket;
    std::string address_;
    std::atomic<bool> stopping_{false};
    std::thread acceptor_;
    std::mutex mutex_;
    std::vector<std::unique_ptr<Peer>> peers_;
};
}

namespace lemon {
class StreamingWorker : public WrappedServer, public IStreamingTranscriptionServer {
public:
    StreamingWorker(const std::string& name, std::string address)
        : WrappedServer("stream-fixture", "error"), address_(std::move(address)) {
        set_model_metadata(name, "", ModelType::TRANSCRIPTION, DEVICE_CPU, RecipeOptions());
        set_state(ModelState::READY);
    }
    void load(const std::string&, const ModelInfo&, const RecipeOptions&, bool) override {}
    void unload() override { set_state(ModelState::UNLOADED); }
    bool is_backend_alive() const override { return get_state() != ModelState::UNLOADED; }
    std::string get_streaming_address() override { return address_; }
private:
    std::string address_;
};
struct RoutingHelperTestHook {
    static StreamingWorker* add(Router& router, std::unique_ptr<StreamingWorker> worker) {
        std::lock_guard<std::mutex> lock(router.load_mutex_);
        auto* raw = worker.get();
        router.loaded_servers_.push_back(std::move(worker));
        return raw;
    }
};
}

int main() {
    using namespace lemon;
    int failures = 0, checks = 0;
    auto check = [&](bool ok, const char* name) {
        ++checks;
        if (!ok) { ++failures; std::fprintf(stderr, "FAIL: %s\n", name); }
    };
    auto wait_for = [&](const std::function<bool()>& predicate) {
        for (int i = 0; i < 200; ++i) {
            if (predicate()) return true;
            std::this_thread::sleep_for(std::chrono::milliseconds(10));
        }
        return false;
    };
    const auto root = std::filesystem::temp_directory_path() /
        ("juiced-realtime-lease-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
    std::filesystem::create_directories(root);
    utils::set_cache_dir(utils::path_to_utf8(root));
    utils::set_config_dir(utils::path_to_utf8(root));
    utils::set_models_dir(utils::path_to_utf8(root / "hf"));
    {
        auto defaults = ConfigFile::base_defaults();
        defaults["auto_evict"] = false;
        defaults["disable_model_filtering"] = true;
        defaults["auto_check_model_updates"] = false;
        RuntimeConfig config(defaults);
        RuntimeConfig::set_global(&config);
        ModelManager models;
        LocalStream first, second;
        Router router(&config, &models, nullptr);
        auto* one = RoutingHelperTestHook::add(router, std::make_unique<StreamingWorker>("stream-one", first.address()));
        auto* two = RoutingHelperTestHook::add(router, std::make_unique<StreamingWorker>("stream-two", second.address()));
        router.set_model_access_policy([](const std::string&) { return false; });
        check(!router.acquire_streaming_transcription("stream-one") && !one->is_busy(),
              "direct streaming cannot bypass native disabled policy");
        auto denied = router.chat_completion({{"model", "stream-one"}, {"messages", json::array()}});
        check(denied.contains("error") && denied["error"].value("details", json::object()).value("code", "") == "model_disabled" && !one->is_busy(),
              "native inference checks shared policy before claiming the worker");
        std::string rejected_stream;
        bool stream_done = false;
        httplib::DataSink sink;
        sink.write = [&](const char* data, size_t size) { rejected_stream.append(data, size); return true; };
        sink.done = [&] { stream_done = true; };
        router.chat_completion_stream(json{{"model", "stream-one"}, {"messages", json::array()}, {"stream", true}}.dump(), sink);
        check(stream_done && rejected_stream.find("model_disabled") != std::string::npos && !one->is_busy(),
              "streaming inference rejects disabled policy without worker ownership");
        router.set_model_access_policy([&router](const std::string&) {
            router.get_stats();
            return true;
        });
        {
            auto lease = router.acquire_streaming_transcription("stream-one");
            check(lease && lease.instance_id == one->get_instance_id() && one->is_busy(), "address and ownership bind the selected generation");
            check(!one->try_evict_if_idle(), "borrowed streaming worker cannot be evicted");
        }
        check(!one->is_busy(), "unconnected lease releases exactly once");
        {
            auto* state_worker = RoutingHelperTestHook::add(router,
                std::make_unique<StreamingWorker>("state-worker", first.address()));
            auto state = router.acquire_model_state_operation("state-worker");
            check(state && state_worker->is_busy() && router.get_stats()["live"]["active_requests"] == 0,
                  "state operation pins its generation without inflating inference activity");
            auto drain = std::async(std::launch::async, [&] {
                try { router.unload_model("state-worker"); }
                catch (const RouterResidencyConflictException&) { return true; }
                return false;
            });
            if (drain.wait_for(std::chrono::seconds(10)) != std::future_status::ready) {
                std::fprintf(stderr, "FAIL: bounded state-operation drain did not settle\n");
                std::quick_exit(1);
            }
            check(drain.get() && state_worker->is_busy() && router.is_model_loaded("state-worker"),
                  "drain timeout preserves the referenced generation instead of reporting a false unload");
            state.release();
            router.unload_model("state-worker");
            check(!router.is_model_loaded("state-worker"), "settled state operation permits an explicit retry");
        }

        for (const auto state : {ModelState::LOADING, ModelState::DOWNSIZING, ModelState::DOWNSIZED}) {
            one->set_state(state);
            check(!router.acquire_streaming_transcription("stream-one") && !one->is_busy(), "transient worker is rejected without waiting or restoring under Router lock");
        }
        one->set_state(ModelState::READY);
        {
            RealtimeSessionManager manager(&router);
            auto id = manager.create_session([](const json&) {}, {{"model", "stream-one"}});
            check(wait_for([&] { return first.connected == 1; }) && one->is_busy(), "actual TCP connection owns worker");
            manager.update_session(id, {{"model", "stream-one"}});
            check(first.connected == 1 && one->active_inference_count() == 1, "same model update retains one connection and one reference");
            manager.update_session(id, {{"model", "stream-two"}});
            check(wait_for([&] { return first.closed == 1 && second.connected == 1; }) && !one->is_busy() && two->is_busy(), "model switch closes old connection and transfers ownership");
            manager.close_session(id);
            check(wait_for([&] { return second.closed == 1; }) && !two->is_busy(), "session close settles connection and releases worker");
            manager.create_session([](const json&) {}, {{"model", "stream-one"}});
            check(wait_for([&] { return first.connected == 2; }) && one->is_busy(), "second session holds its worker until shutdown");
        }
        check(wait_for([&] { return first.closed == 2; }) && !one->is_busy(), "manager shutdown closes sessions before releasing references");
        one->set_state(ModelState::READY);
        RoutingHelperTestHook::add(router, std::make_unique<StreamingWorker>("stream-failed", "invalid-protocol://fixture"));
        {
            RealtimeSessionManager manager(&router);
            manager.create_session([](const json&) {}, {{"model", "stream-failed"}});
            check(router.get_stats()["live"]["active_requests"] == 0, "failed connection does not leak a worker reference");
        }
        {
            RealtimeSessionManager manager(&router);
            auto id = manager.create_session([](const json&) {}, {{"model", "stream-one"}});
            check(wait_for([&] { return first.connected == 3; }), "unload fixture has an actual live connection");
            std::promise<void> entered;
            auto ready = entered.get_future();
            auto unload = std::async(std::launch::async, [&] {
                entered.set_value();
                router.unload_model("stream-one");
            });
            ready.wait();
            check(unload.wait_for(std::chrono::milliseconds(30)) == std::future_status::timeout,
                  "explicit unload cannot destroy a connected streaming worker");
            auto inspection = std::async(std::launch::async, [&] { return router.get_stats(); });
            const bool responsive = inspection.wait_for(std::chrono::milliseconds(200)) == std::future_status::ready;
            manager.close_session(id);
            if (unload.wait_for(std::chrono::seconds(10)) != std::future_status::ready) {
                std::fprintf(stderr, "FAIL: unload did not settle after disconnect\n");
                std::quick_exit(1);
            }
            unload.get();
            inspection.get();
            check(responsive, "inspection remains responsive while unload waits for a live connection");
            check(wait_for([&] { return first.closed == 3; }), "disconnect releases ownership and permits the pending unload");
        }
    }
    RuntimeConfig::set_global(nullptr);
    std::error_code error;
    std::filesystem::remove_all(root, error);
    std::printf("Realtime worker lease: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
