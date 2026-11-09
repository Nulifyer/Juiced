#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <future>
#include <string>
#include <thread>

#include <httplib.h>
#include <lemon/utils/http_client.h>

int main() {
    using namespace std::chrono_literals;
    using namespace lemon::utils;
    int checks = 0, failures = 0;
    auto check = [&](bool ok, const char* name) {
        ++checks;
        failures += !ok;
        std::printf("[%s] %s\n", ok ? "PASS" : "FAIL", name);
        std::fflush(stdout);
    };
    httplib::Server server;
    std::atomic<int> requests{0};
    server.Get("/busy", [&](const httplib::Request&, httplib::Response& response) {
        ++requests;
        response.status = 503;
        response.set_content("Unavailable", "text/plain");
    });
    std::atomic<int> recover_requests{0};
    server.Get("/recover", [&](const httplib::Request&, httplib::Response& response) {
        if (++recover_requests == 1) response.status = 503;
        response.set_content("payload", "text/plain");
    });
    const int port = server.bind_to_any_port("127.0.0.1");
    if (port <= 0) return 1;
    std::thread listener([&] { server.listen_after_bind(); });
    server.wait_until_ready();
    const std::string base = "http://127.0.0.1:" + std::to_string(port);
    const auto root = std::filesystem::temp_directory_path() /
        ("juiced-download-retry-" + std::to_string(port));
    std::filesystem::create_directories(root);
    std::atomic<bool> retrying{false}, entered{false}, cancel{false};
    std::promise<void> backoff_entered, release_callback;
    auto entered_future = backoff_entered.get_future();
    auto release = release_callback.get_future().share();
    DownloadRetry observation;
    size_t busy_bytes = 0, busy_total = 0;
    DownloadOptions options;
    options.initial_retry_delay_ms = 30000;
    options.max_retry_delay_ms = 30000;
    options.on_retry = [&](const DownloadRetry& value) {
        observation = value;
        retrying = value.attempt > 0;
    };
    auto download = std::async(std::launch::async, [&] {
        return HttpClient::download_file(base + "/busy", (root / "cancel").string(),
            [&](size_t downloaded, size_t total) {
                busy_bytes = (std::max)(busy_bytes, downloaded);
                busy_total = (std::max)(busy_total, total);
                if (retrying && !entered.exchange(true)) {
                    backoff_entered.set_value();
                    release.wait();
                    return true;
                }
                return !cancel;
            }, {}, options, HttpSecurityPolicy::TrustedLoopback);
    });
    check(entered_future.wait_for(5s) == std::future_status::ready,
          "actual 503 reaches retry backoff");
    cancel = true;
    release_callback.set_value();
    const bool stopped = download.wait_for(2s) == std::future_status::ready;
    check(stopped, "cancellation after the first backoff check completes within two seconds");
    if (!stopped) std::exit(1);
    auto result = download.get();
    check(result.cancelled && !result.success, "backoff cancellation is a failed cancelled result");
    check(requests == 1, "cancelled retry issues no second upstream request");
    check(observation.attempt == 1 && observation.max_retries == 5 &&
          observation.delay_ms == 30000 && observation.http_code == 503,
          "retry observation preserves attempt, delay and HTTP status");
    check(busy_bytes == 0 && busy_total == 0,
          "HTTP error bodies never count as model download progress");
    check(!std::filesystem::exists(root / "cancel"), "cancelled retry never publishes a final file");

    int retries = 0;
    options.initial_retry_delay_ms = 25;
    options.on_retry = [&](const DownloadRetry& value) { retries += value.attempt > 0; };
    result = HttpClient::download_file(base + "/recover", (root / "recovered").string(),
        [](size_t, size_t) { return true; }, {}, options, HttpSecurityPolicy::TrustedLoopback);
    check(result.success && !result.cancelled && recover_requests == 2 && retries == 1,
          "uncancelled transient failure still retries and succeeds");
    check(std::filesystem::file_size(root / "recovered") == 7,
          "successful retry publishes the complete payload");
    server.stop();
    listener.join();
    std::filesystem::remove_all(root);
    std::printf("%d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
