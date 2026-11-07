#include "lemon/request_identity.h"
#include <cstdio>

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
    std::printf("Request identity checks: %d failures\n", failed);
    return failed ? 1 : 0;
}
