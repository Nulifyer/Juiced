#pragma once

#include <nlohmann/json.hpp>
#include <string>

#include "lemon/error_types.h"

namespace lemon {
namespace anthropic {

// Lemonade and the streaming proxy report an upstream HTTP status as
// "status_code"; llama.cpp's own error bodies carry a numeric "code" instead.
inline int backend_error_http_status(const nlohmann::json& error) {
    return ErrorResponse::http_status({{"error", error}});
}

inline nlohmann::json build_anthropic_error(const nlohmann::json& error, int status) {
    const char* type = "api_error";
    switch (status) {
        case 400: type = "invalid_request_error"; break;
        case 401: type = "authentication_error"; break;
        case 402: type = "billing_error"; break;
        case 403: type = "permission_error"; break;
        case 404: type = "not_found_error"; break;
        case 409: type = "conflict_error"; break;
        case 413: type = "request_too_large"; break;
        case 429: type = "rate_limit_error"; break;
        case 504: type = "timeout_error"; break;
        case 529: type = "overloaded_error"; break;
        default:
            // Anthropic names no type for the remaining 4xx, and every one of
            // them means the caller can fix the request.
            if (status >= 400 && status < 500) {
                type = "invalid_request_error";
            }
            break;
    }

    const std::string message = error.is_object()
        ? error.value("message", "backend error")
        : error.dump();

    return nlohmann::json{
        {"type", "error"},
        {"error", {
            {"type", type},
            {"message", message},
        }},
    };
}

}  // namespace anthropic
}  // namespace lemon
