#include "lemon/backends/llamacpp/llamacpp_request.h"

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <string>
#include <set>

namespace lemon {
namespace backends {
namespace llamacpp {
namespace {

constexpr std::uint64_t GRAMMAR_REPETITION_LIMIT = 2000;

bool has_integer_value_at_least(const nlohmann::json& value, std::uint64_t threshold) {
    if (value.is_number_unsigned()) {
        return value.get<std::uint64_t>() >= threshold;
    }
    if (value.is_number_integer()) {
        return value.get<std::int64_t>() >= static_cast<std::int64_t>(threshold);
    }
    if (!value.is_number_float()) return false;

    const double number = value.get<double>();
    return std::isfinite(number) && number >= 0.0 && std::floor(number) == number &&
           number >= static_cast<double>(threshold);
}

void sanitize_schema(nlohmann::json& schema) {
    if (schema.is_array()) {
        for (auto& value : schema) {
            sanitize_schema(value);
        }
        return;
    }
    if (!schema.is_object()) return;

    // llama.cpp rejects grammar repetitions at this threshold. Remove oversized
    // bounds instead of clamping them, which would reject valid tool arguments.
    // Remove this workaround once ggml-org/llama.cpp#17473 is fixed in pinned builds.
    for (const std::string key : {"minLength", "maxLength"}) {
        auto limit = schema.find(key);
        if (limit != schema.end() && has_integer_value_at_least(*limit, GRAMMAR_REPETITION_LIMIT)) {
            schema.erase(limit);
        }
    }

    // Array grammars emit the first item separately, so an item bound of N
    // produces a repetition bound of N - 1.
    for (const std::string key : {"minItems", "maxItems"}) {
        auto limit = schema.find(key);
        if (limit != schema.end() && has_integer_value_at_least(*limit, GRAMMAR_REPETITION_LIMIT + 1)) {
            schema.erase(limit);
        }
    }

    for (const std::string key : {
             "additionalItems",
             "additionalProperties",
             "allOf",
             "anyOf",
             "contains",
             "contentSchema",
             "else",
             "if",
             "items",
             "not",
             "oneOf",
             "prefixItems",
             "propertyNames",
             "then",
             "unevaluatedItems",
             "unevaluatedProperties",
         }) {
        auto subschema = schema.find(key);
        if (subschema != schema.end()) {
            sanitize_schema(*subschema);
        }
    }

    for (const std::string key : {
             "$defs",
             "definitions",
             "dependencies",
             "dependentSchemas",
             "patternProperties",
             "properties",
         }) {
        auto subschemas = schema.find(key);
        if (subschemas == schema.end() || !subschemas->is_object()) continue;
        for (auto& item : subschemas->items()) {
            sanitize_schema(item.value());
        }
    }
}

}  // namespace

nlohmann::json sanitize_tool_schema_limits(nlohmann::json request) {
    auto tools = request.find("tools");
    if (tools == request.end() || !tools->is_array()) return request;

    for (auto& tool : *tools) {
        if (!tool.is_object()) continue;

        auto parameters = tool.find("parameters");
        if (parameters != tool.end()) {
            sanitize_schema(*parameters);
        }

        auto function = tool.find("function");
        if (function == tool.end() || !function->is_object()) continue;

        parameters = function->find("parameters");
        if (parameters != function->end()) {
            sanitize_schema(*parameters);
        }
    }

    return request;
}

RequestEnvelope resolve_request(InferenceOperation operation, const nlohmann::json& original, bool protected_mode) {
    using json = nlohmann::json;
    const auto original_bytes = protected_mode ? request_storage_ceiling(original) : 0;
    RequestEnvelope result{original, {}};
    if (operation == InferenceOperation::Chat || operation == InferenceOperation::Responses)
        result.body = sanitize_tool_schema_limits(std::move(result.body));
    if (!protected_mode) return result;
    auto& request = result.body;
    request.erase("priority");
    const bool generation = operation == InferenceOperation::Chat || operation == InferenceOperation::Completion || operation == InferenceOperation::Responses;
    const std::set<std::string> common = {"model", "stream", "stream_options", "temperature", "top_p", "top_k", "min_p",
        "seed", "stop", "repeat_penalty", "frequency_penalty", "presence_penalty", "typical_p", "repeat_last_n",
        "tools", "tool_choice", "response_format", "json_schema", "grammar", "logit_bias", "cache_prompt", "id_slot",
        "return_tokens", "ignore_eos", "chat_template_kwargs", "reasoning_format", "reasoning_budget", "n_keep", "n_discard",
        "n_cache_reuse", "max_tokens", "max_completion_tokens", "n_predict", "max_output_tokens", "num_predict",
        "n", "n_cmpl", "best_of", "logprobs", "top_logprobs", "prompt_logprobs", "n_probs"};
    for (const auto& item : request.items()) {
        const bool input = (operation == InferenceOperation::Chat && item.key() == "messages") ||
            (operation == InferenceOperation::Completion && item.key() == "prompt") ||
            (operation == InferenceOperation::Responses && (item.key() == "input" || item.key() == "instructions")) ||
            (operation == InferenceOperation::Embedding && (item.key() == "input" || item.key() == "encoding_format")) ||
            (operation == InferenceOperation::Rerank && (item.key() == "query" || item.key() == "documents" || item.key() == "top_n"));
        if (!input && !(generation ? common.count(item.key()) != 0 : item.key() == "model"))
            throw AdmissionError("workload_unqualified", "This llama.cpp parameter has no protected request contract: " + item.key());
    }
    const auto positive = [&](const char* key) {
        const auto& value = request.at(key);
        if (!value.is_number_integer() || value.get<int64_t>() <= 0 || value.get<uint64_t>() > INT32_MAX)
            throw AdmissionError("workload_unqualified", "Protected llama.cpp bounds must be positive 32-bit integers");
        return value.get<uint64_t>();
    };
    const auto text_content = [](const json& content) {
        if (content.is_null() || content.is_string()) return;
        if (!content.is_array()) throw AdmissionError("workload_unqualified", "Protected llama.cpp content must be text");
        for (const auto& part : content)
            if (!part.is_object() || !part.contains("type") ||
                (part.at("type") != "text" && part.at("type") != "input_text" && part.at("type") != "output_text") ||
                !part.contains("text") || !part.at("text").is_string())
                throw AdmissionError("workload_unqualified", "Multimodal llama.cpp input is not qualified for protected admission");
    };
    if (generation) {
        const bool responses = operation == InferenceOperation::Responses;
        uint64_t output = 0;
        for (const auto* key : {"max_tokens", "max_completion_tokens", "n_predict", "max_output_tokens", "num_predict"}) {
            if (!request.contains(key)) continue;
            if (std::string(key) == "num_predict" || (responses ? std::string(key) != "max_output_tokens" : std::string(key) == "max_output_tokens"))
                throw AdmissionError("workload_unqualified", "Token-limit alias is unsupported for this llama.cpp operation");
            const auto value = positive(key);
            if (output && output != value)
                throw AdmissionError("workload_unqualified", "Conflicting generation ceilings are not qualified");
            output = value;
        }
        if (!output) throw AdmissionError("workload_unqualified", "Protected generation requires an effective token ceiling");
        for (const auto* key : {"max_tokens", "max_completion_tokens", "n_predict", "max_output_tokens", "num_predict"}) request.erase(key);
        request[responses ? "max_output_tokens" : "max_tokens"] = output;
        result.workload["output_tokens"] = output;
        uint64_t batch = 0;
        if (request.contains("best_of")) throw AdmissionError("workload_unqualified", "best_of has no qualified llama.cpp execution contract");
        for (const auto* key : {"n", "n_cmpl"}) if (request.contains(key)) {
            const auto value = positive(key);
            if (batch && batch != value) throw AdmissionError("workload_unqualified", "Conflicting completion counts are not qualified");
            batch = value;
        }
        request.erase("n");
        request["n_cmpl"] = batch ? batch : 1;
        result.workload["batch"] = batch ? batch : 1;
        for (const auto* key : {"logprobs", "top_logprobs", "prompt_logprobs", "n_probs"}) {
            if (request.contains(key) && request.at(key) != nullptr && request.at(key) != false && request.at(key) != 0)
                throw AdmissionError("workload_unqualified", "Probability output has no protected llama.cpp resource envelope");
            request.erase(key);
        }
        request["n_probs"] = 0;
        if (operation == InferenceOperation::Chat) {
            if (!request.contains("messages") || !request.at("messages").is_array())
                throw AdmissionError("workload_unqualified", "Protected chat requires messages");
            for (const auto& message : request.at("messages")) {
                if (!message.is_object()) throw AdmissionError("workload_unqualified", "Messages must be objects");
                if (message.contains("content")) text_content(message.at("content"));
            }
        } else if (operation == InferenceOperation::Completion) {
            if (!request.contains("prompt") || !request.at("prompt").is_string())
                throw AdmissionError("workload_unqualified", "Protected completion requires one text prompt");
        } else {
            if (!request.contains("input")) throw AdmissionError("workload_unqualified", "Protected Responses requires text input");
            const auto& input = request.at("input");
            if (input.is_array()) {
                for (const auto& item : input) {
                    if (!item.is_object() || !item.contains("content")) throw AdmissionError("workload_unqualified", "Responses item has no qualified text shape");
                    text_content(item.at("content"));
                }
            } else if (!input.is_string()) throw AdmissionError("workload_unqualified", "Protected Responses requires text input");
        }
    } else if (operation == InferenceOperation::Embedding || operation == InferenceOperation::Rerank) {
        const auto key = operation == InferenceOperation::Embedding ? "input" : "documents";
        if (!request.contains(key)) throw AdmissionError("workload_unqualified", "Protected text service requires bounded input");
        const auto& input = request.at(key);
        if (input.is_string()) result.workload["batch"] = 1;
        else if (input.is_array() && !input.empty()) {
            for (const auto& item : input) if (!item.is_string())
                throw AdmissionError("workload_unqualified", "Protected text batches require strings");
            result.workload["batch"] = input.size();
        } else throw AdmissionError("workload_unqualified", "Protected text service requires text input");
    } else throw AdmissionError("workload_unqualified", "This llama.cpp operation has no protected request contract");
    result.workload["input_bytes"] = std::max(original_bytes, request_storage_ceiling(request));
    return result;
}

}  // namespace llamacpp
}  // namespace backends
}  // namespace lemon
