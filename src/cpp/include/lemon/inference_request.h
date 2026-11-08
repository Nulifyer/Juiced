#pragma once

#include "lemon/resource_admission.h"

namespace lemon {

enum class InferenceOperation { Chat, Completion, Responses, Embedding, Rerank, State, Other };

struct RequestEnvelope {
    nlohmann::json body;
    std::map<std::string, uint64_t> workload;
};

inline uint64_t request_storage_ceiling(const nlohmann::json& value, unsigned depth = 0) {
    if (depth > 64 || value.is_binary())
        throw AdmissionError("workload_unqualified", "Request structure has no qualified resource bound");
    uint64_t size = 64;
    if (value.is_string()) size += value.get_ref<const std::string&>().size() * 6;
    else if (value.is_object()) {
        for (const auto& item : value.items())
            size += item.key().size() * 6 + request_storage_ceiling(item.value(), depth + 1);
    } else if (value.is_array()) {
        for (const auto& item : value) size += request_storage_ceiling(item, depth + 1);
    }
    return size;
}

} // namespace lemon
