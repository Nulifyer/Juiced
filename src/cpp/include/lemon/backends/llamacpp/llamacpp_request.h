#pragma once

#include <nlohmann/json.hpp>
#include "lemon/inference_request.h"

namespace lemon {
namespace backends {
namespace llamacpp {

nlohmann::json sanitize_tool_schema_limits(nlohmann::json request);
RequestEnvelope resolve_request(InferenceOperation operation, const nlohmann::json& request, bool protected_mode);

}  // namespace llamacpp
}  // namespace backends
}  // namespace lemon
