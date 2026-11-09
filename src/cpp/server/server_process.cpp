#include "lemon/server_process.h"

#include <stdexcept>

#include <lemon/utils/aixlog.hpp>

namespace lemon {

using utils::ProcessManager;

namespace {

bool has_handle(const utils::ProcessHandle& handle) {
#ifdef _WIN32
    return handle.handle != nullptr;
#else
    return handle.pid > 0;
#endif
}

}  // namespace

void ServerProcess::spawn(std::vector<std::string> command_line, const std::string& working_dir,
                          const std::vector<std::pair<std::string, std::string>>& env) {
    command_line_ = std::move(command_line);
    const std::vector<std::string> args(command_line_.begin() + 1, command_line_.end());
    handle_ = ProcessManager::start_process(command_line_.front(), args, working_dir,
                                            output_.inherit, output_.filter_health_logs, env);
    if (!has_handle(handle_)) {
        throw std::runtime_error("Failed to start " + command_line_.front());
    }
    LOG(INFO, "ServerProcess") << "Started " << command_line_.front() << " (PID " << handle_.pid
                               << ")" << std::endl;
}

ServerProcess::~ServerProcess() {
    try { ServerProcess::stop(); }
    catch (const std::exception& error) {
        LOG(ERROR, "ServerProcess") << "Direct-process cleanup remains unconfirmed for PID "
                                   << handle_.pid << ": " << error.what() << std::endl;
    }
}

void ServerProcess::stop() {
    if (!has_handle(handle_)) {
        return;
    }
    if (!ProcessManager::stop_process(handle_))
        throw std::runtime_error("Direct-process exit could not be confirmed for PID " + std::to_string(handle_.pid));
    handle_ = {nullptr, 0};
}

bool ServerProcess::exited() const {
    return !has_handle(handle_) || ProcessManager::has_exited(handle_);
}

bool ServerProcess::running() const {
    return has_handle(handle_) && ProcessManager::is_running(handle_);
}

std::string NativeProcess::start(const ServerCommand& command) {
    std::vector<std::string> command_line = {command.program};
    command_line.insert(command_line.end(), command.args.begin(), command.args.end());
    spawn(std::move(command_line), working_dir_, command.env);
    return "127.0.0.1";
}

}  // namespace lemon
