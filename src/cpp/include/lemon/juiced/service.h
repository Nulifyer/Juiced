#pragma once
#include <atomic>
#include <condition_variable>
#include <deque>
#include <filesystem>
#include <httplib.h>
#include <map>
#include <mutex>
#include <nlohmann/json.hpp>
#include <set>
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
  bool hot(const std::string &model) const;
  void set_hot(const std::string &model, bool pinned);
  bool inference_guard(const httplib::Request &, httplib::Response &);
  bool unleased_protocol(const httplib::Request &, httplib::Response &);

private:
  friend class Server;
  using json = nlohmann::json;
  Server &server_;
  std::filesystem::path directory_;
  mutable std::mutex mutex_;
  std::condition_variable wake_;
  json state_, catalog_ = json::array(), operations_ = json::object();
  std::deque<json> pending_;
  std::atomic<bool> stopping_{false};
  std::thread worker_;
  std::map<std::string, std::set<int>> active_slots_;
  std::set<std::string> active_snapshots_;
  std::filesystem::path snapshot_directory_;
  json cache_profile(const std::string &model);
  void cache_api(const httplib::Request &, httplib::Response &,
                 const std::string &);
  void prune_cache_locked();
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
