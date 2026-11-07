#include "lemon/request_identity.h"

#include <algorithm>
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
                                         const std::string& admin_key) {
    const auto credential = request_credential(request);
    RequestIdentity identity{"anonymous", "anonymous", {}, false};
    if (!admin_key.empty() && credential == admin_key)
        identity = {"legacy-admin", "operator", {}, true};
    else if (!api_key.empty() && credential == api_key)
        identity = {"legacy-api", "inference", {}, true};
    else if (!api_key.empty())
        identity = {"unauthenticated", "none", {}, false};
    identity.credential_epoch = epoch(identity.principal, api_key, admin_key);
    return identity;
}

} // namespace lemon
