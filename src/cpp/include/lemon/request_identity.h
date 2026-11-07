#pragma once

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

} // namespace lemon
