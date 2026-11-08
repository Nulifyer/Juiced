#pragma once

#include <cstdint>
#include <functional>
#include <optional>
#include <string>
#include <httplib.h>

namespace lemon {

struct RequestIdentity {
    std::string principal;
    std::string role;
    std::string credential_epoch;
    bool authenticated = false;
};

std::string request_credential(const httplib::Request& request);
RequestIdentity resolve_request_identity(const httplib::Request& request,
                                         const std::string& api_key,
                                         const std::string& admin_key);

struct NativeRequestContext {
    RequestIdentity identity;
    std::string protocol;
    std::string session;
    uint64_t request_id = 0;
    std::function<bool()> cancelled;
};

NativeRequestContext make_request_context(const httplib::Request& request,
                                         const std::string& api_key,
                                         const std::string& admin_key);
std::string request_session_id(const httplib::Request& request);
std::optional<NativeRequestContext> current_request_context();

class RequestContextScope {
public:
    explicit RequestContextScope(NativeRequestContext context);
    ~RequestContextScope();
    RequestContextScope(const RequestContextScope&) = delete;
    RequestContextScope& operator=(const RequestContextScope&) = delete;
private:
    NativeRequestContext context_;
    const NativeRequestContext* previous_;
};

using HttpHandlerDecorator = std::function<httplib::Server::Handler(httplib::Server::Handler)>;
using RequestContextResolver = std::function<NativeRequestContext(const httplib::Request&)>;
httplib::Server::Handler bind_request_context(httplib::Server::Handler handler,
                                             RequestContextResolver resolver);

} // namespace lemon
