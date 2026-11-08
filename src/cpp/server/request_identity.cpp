#include "lemon/request_identity.h"

#include <algorithm>
#include <atomic>
#include <cctype>
#include <iomanip>
#include <sstream>
#include <stdexcept>
#include <mbedtls/md.h>

namespace lemon {
namespace {
std::string epoch(const std::string& role, const std::string& api,
                  const std::string& admin) {
    const auto input = "juiced-principal-v1:" + std::to_string(role.size()) + ":" +
        role + std::to_string(api.size()) + ":" + api + admin;
    unsigned char hash[32];
    if (mbedtls_md(mbedtls_md_info_from_type(MBEDTLS_MD_SHA256),
                   reinterpret_cast<const unsigned char*>(input.data()),
                   input.size(), hash) != 0)
        throw std::runtime_error("Cannot resolve request identity");
    std::ostringstream result;
    for (auto byte : hash)
        result << std::hex << std::setw(2) << std::setfill('0') << int(byte);
    return result.str();
}
}

std::string credential_configuration_fingerprint(const std::string& api_key, const std::string& admin_key) {
    return epoch("credential-configuration", api_key, admin_key);
}

std::string request_credential(const httplib::Request& request) {
    if (request.has_header("Authorization")) {
        const auto value = request.get_header_value("Authorization");
        if (value.size() < 7) return {};
        std::string scheme = value.substr(0, 7);
        std::transform(scheme.begin(), scheme.end(), scheme.begin(),
                       [](unsigned char c) { return char(std::tolower(c)); });
        return scheme == "bearer " ? value.substr(7) : std::string();
    }
    return request.get_header_value("x-api-key");
}

RequestIdentity resolve_request_identity(const httplib::Request& request,
                                         const std::string& api_key,
                                         const std::string& admin_key,
                                         const std::string& transition_epoch) {
    const auto credential = request_credential(request);
    RequestIdentity identity{"anonymous", "anonymous", {}, false};
    if (!admin_key.empty() && credential == admin_key)
        identity = {"legacy-admin", "operator", {}, true};
    else if (!api_key.empty() && credential == api_key)
        identity = {"legacy-api", "inference", {}, true};
    else if (!api_key.empty())
        identity = {"unauthenticated", "none", {}, false};
    identity.credential_epoch = epoch(identity.principal + ":" + transition_epoch, api_key, admin_key);
    return identity;
}

namespace {
thread_local const NativeRequestContext* request_context = nullptr;
std::atomic<uint64_t> request_sequence{0};
}

std::string request_session_id(const httplib::Request& request) {
    auto session = request.get_header_value("X-Juiced-Session-Id");
    if (session.empty()) session = request.get_header_value("X-Juiced-Thread-Id");
    if (session.size() >= 160 || session.find_first_not_of(
        "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-") != std::string::npos) return {};
    return session;
}

NativeRequestContext make_request_context(const httplib::Request& request,
                                         const std::string& api_key,
                                         const std::string& admin_key,
                                         const std::string& transition_epoch) {
    std::string protocol = "openai";
    if (request.path == "/mcp") protocol = "mcp";
    else if (request.path.rfind("/sdapi/", 0) == 0) protocol = "automatic1111";
    else if (request.path == "/v1/messages" || request.path == "/v1/messages/count_tokens") protocol = "anthropic";
    else if (request.path.rfind("/api/", 0) == 0 && request.path.rfind("/api/v", 0) != 0) protocol = "ollama";
    return {resolve_request_identity(request, api_key, admin_key, transition_epoch), protocol,
            request_session_id(request), ++request_sequence, request.is_connection_closed};
}

std::optional<NativeRequestContext> current_request_context() {
    return request_context ? std::optional<NativeRequestContext>(*request_context) : std::nullopt;
}

RequestContextScope::RequestContextScope(NativeRequestContext context)
    : context_(std::move(context)), previous_(request_context) { request_context = &context_; }

RequestContextScope::~RequestContextScope() { request_context = previous_; }

httplib::Server::Handler bind_request_context(httplib::Server::Handler handler,
                                             RequestContextResolver resolver) {
    return [handler = std::move(handler), resolver = std::move(resolver)](
        const httplib::Request& request, httplib::Response& response) {
        const auto context = resolver(request);
        RequestContextScope scope(context);
        handler(request, response);
        if (response.content_provider_) {
            auto provider = std::move(response.content_provider_);
            response.content_provider_ = [provider = std::move(provider), context](
                size_t offset, size_t length, httplib::DataSink& sink) {
                RequestContextScope stream_scope(context);
                return provider(offset, length, sink);
            };
        }
        if (response.content_provider_resource_releaser_) {
            auto release = std::move(response.content_provider_resource_releaser_);
            response.content_provider_resource_releaser_ = [release = std::move(release), context](bool ok) {
                RequestContextScope release_scope(context);
                release(ok);
            };
        }
    };
}

} // namespace lemon
