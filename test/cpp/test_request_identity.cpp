#include "lemon/request_identity.h"
#include <atomic>
#include <cstdio>
#include <stdexcept>
#include <thread>

int main() {
    int failed = 0;
    auto check = [&](bool condition, const char* name) {
        if (!condition) { std::fprintf(stderr, "FAIL: %s\n", name); ++failed; }
    };
    httplib::Request bearer, key, dummy, forged, malformed;
    bearer.set_header("Authorization", "Bearer fixture-api");
    key.set_header("x-api-key", "fixture-api");
    dummy.set_header("Authorization", "Bearer different-dummy");
    forged.set_header("X-Client-Id", "legacy-admin");
    forged.set_header("X-Juiced-Session-Id", "another-session");
    malformed.set_header("Authorization", "Basic fixture-api");
    const auto a = lemon::resolve_request_identity(bearer, "fixture-api", "fixture-admin");
    const auto b = lemon::resolve_request_identity(key, "fixture-api", "fixture-admin");
    check(a.authenticated && a.principal == "legacy-api", "accepted inference principal");
    check(a.principal == b.principal && a.credential_epoch == b.credential_epoch,
          "credential transport does not change namespace");
    httplib::Request admin;
    admin.set_header("Authorization", "Bearer fixture-admin");
    const auto c = lemon::resolve_request_identity(admin, "fixture-api", "fixture-admin");
    check(c.role == "operator" && c.principal != a.principal &&
          c.credential_epoch != a.credential_epoch, "operator and inference namespaces differ");
    const auto rotated = lemon::resolve_request_identity(bearer, "rotated", "fixture-admin");
    check(!rotated.authenticated && rotated.credential_epoch != a.credential_epoch,
          "revoked credential cannot retain prior namespace");
    httplib::Request next;
    next.set_header("Authorization", "Bearer rotated");
    check(lemon::resolve_request_identity(next, "rotated", "fixture-admin").credential_epoch !=
          a.credential_epoch, "replacement credential has a new epoch");
    const auto anon = lemon::resolve_request_identity(bearer, "", "");
    check(!anon.authenticated && anon.principal == "anonymous", "dummy key is not identity");
    check(anon.credential_epoch == lemon::resolve_request_identity(dummy, "", "").credential_epoch,
          "anonymous clients share documented trust domain");
    check(anon.principal == lemon::resolve_request_identity(forged, "", "").principal,
          "client headers do not impersonate principals");
    check(anon.credential_epoch != a.credential_epoch, "auth mode changes namespace");
    check(!lemon::resolve_request_identity(malformed, "fixture-api", "fixture-admin").authenticated,
          "malformed authorization scheme is not accepted");
    bearer.headers.clear(); bearer.set_header("Authorization", "bEaReR fixture-api");
    check(lemon::request_credential(bearer) == "fixture-api", "scheme is case insensitive");
    bearer.set_header("x-api-key", "fixture-admin");
    check(lemon::request_credential(bearer) == "fixture-api", "Bearer has unambiguous precedence");
    check(lemon::resolve_request_identity(key, "fixture-api", "fixture-api").role == "operator",
          "legacy shared admin credential behavior is preserved");
    check(!lemon::current_request_context(), "no request context outside a handler");
    key.path = "/api/v1/chat/completions";
    key.set_header("X-Juiced-Session-Id", "fixture.session-1");
    std::atomic<bool> closed{false};
    key.is_connection_closed = [&] { return closed.load(); };
    const auto context = lemon::make_request_context(key, "fixture-api", "fixture-admin");
    check(context.identity.principal == a.principal && context.session == "fixture.session-1" &&
          context.protocol == "openai" && context.request_id > 0,
          "native request context contains normalized identity and session");
    check(!context.cancelled(), "live client is not cancelled");
    closed = true;
    check(context.cancelled(), "client cancellation remains observable");
    auto nested = context;
    nested.identity = c;
    {
        lemon::RequestContextScope outer(context);
        check(lemon::current_request_context()->identity.principal == a.principal,
              "outer request identity is visible");
        try {
            lemon::RequestContextScope inner(nested);
            check(lemon::current_request_context()->identity.role == "operator",
                  "nested operation has its own identity");
            throw std::runtime_error("fixture");
        } catch (const std::runtime_error&) {}
        check(lemon::current_request_context()->identity.principal == a.principal,
              "exception restores previous context");
        std::thread separate([&] {
            check(!lemon::current_request_context(), "another thread inherits no implicit identity");
        });
        separate.join();
    }
    check(!lemon::current_request_context(), "scope exit clears request identity");
    httplib::Request session;
    session.set_header("X-Juiced-Thread-Id", "legacy-thread");
    check(lemon::request_session_id(session) == "legacy-thread", "legacy thread session is preserved");
    session.set_header("X-Juiced-Session-Id", "invalid/session");
    check(lemon::request_session_id(session).empty(), "invalid explicit session cannot select a namespace");
    for (const auto& entry : {std::pair<const char*, const char*>{"/mcp", "mcp"},
                             {"/sdapi/v1/txt2img", "automatic1111"},
                             {"/v1/messages", "anthropic"},
                             {"/v1/messages/count_tokens", "anthropic"},
                             {"/api/chat", "ollama"},
                             {"/api/v0/chat/completions", "openai"},
                             {"/v0/responses", "openai"}}) {
        session.path = entry.first;
        check(lemon::make_request_context(session, "", "").protocol == entry.second,
              "protocol comes from the registered URL");
    }
    uint64_t observed_id = 0;
    bool provider_seen = false, release_seen = false;
    httplib::Response response;
    auto handler = lemon::bind_request_context(
        [&](const httplib::Request&, httplib::Response& res) {
            auto current = lemon::current_request_context();
            check(current && current->identity.principal == a.principal,
                  "decorated handler uses accepted credential owner");
            observed_id = current->request_id;
            res.set_chunked_content_provider("text/event-stream",
                [&](size_t, httplib::DataSink&) {
                    auto stream = lemon::current_request_context();
                    provider_seen = stream && stream->request_id == observed_id &&
                        stream->identity.principal == a.principal &&
                        stream->session == "fixture.session-1" && stream->cancelled();
                    throw std::runtime_error("stream fixture");
                    return false;
                }, [&](bool) {
                    auto release = lemon::current_request_context();
                    release_seen = release && release->request_id == observed_id &&
                        release->identity.principal == a.principal;
                });
        }, [](const httplib::Request& req) {
            return lemon::make_request_context(req, "fixture-api", "fixture-admin");
        });
    {
        lemon::RequestContextScope outer(nested);
        handler(key, response);
        check(lemon::current_request_context()->identity.role == "operator",
              "handler restores enclosing context");
    }
    std::thread streaming([&] {
        lemon::RequestContextScope unrelated(nested);
        httplib::DataSink sink;
        try { response.content_provider_(0, 0, sink); }
        catch (const std::runtime_error&) {}
        check(lemon::current_request_context()->identity.role == "operator",
              "stream failure restores callback thread identity");
        response.content_provider_resource_releaser_(false);
        check(lemon::current_request_context()->identity.role == "operator",
              "stream cleanup restores callback thread identity");
    });
    streaming.join();
    check(provider_seen && release_seen, "stream and cleanup retain original trusted context across threads");
    response.content_provider_resource_releaser_ = nullptr;
    std::printf("Request identity checks: %d failures\n", failed);
    return failed ? 1 : 0;
}
