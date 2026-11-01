#pragma once
#include <atomic>
#include <condition_variable>
#include <deque>
#include <filesystem>
#include <httplib.h>
#include <mutex>
#include <nlohmann/json.hpp>
#include <thread>
namespace lemon {
class Server;
class JuicedService {
public:
  explicit JuicedService(Server &server);
  ~JuicedService();
  void stop();
  void routes(httplib::Server &http);
  bool allows(const std::string &model) const;

private:
  using json = nlohmann::json;
  Server &server_;
  std::filesystem::path directory_;
  mutable std::mutex mutex_;
  std::condition_variable wake_;
  json state_, catalog_ = json::array(), operations_ = json::object();
  std::deque<json> pending_;
  std::atomic<bool> stopping_{false};
  std::thread worker_;
  void persist_locked();
  void reconcile();
  void refresh();
  void execute(const json &command);
  void enqueue(const json &command);
  void run();
  void handle(const httplib::Request &, httplib::Response &,
              const std::string &operation);
  void import_file(const httplib::Request &, httplib::Response &,
                   const std::string &operation);
  void chat(const httplib::Request &, httplib::Response &);
};
} // namespace lemon
