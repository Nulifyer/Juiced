#include <lemon/utils/json_utils.h>
#include <lemon/utils/path_utils.h>
#include <chrono>
#include <cstdint>
#include <exception>
#include <fstream>
#include <sstream>
#include <stdexcept>
#include <thread>
#include <vector>

namespace lemon {
namespace utils {

json JsonUtils::load_from_file(const std::string& file_path) {
    std::ifstream file(path_from_utf8(file_path));
    if (!file.is_open()) {
        throw std::runtime_error("Failed to open file: " + file_path);
    }

    json j;
    try {
        file >> j;
    } catch (const json::exception& e) {
        throw std::runtime_error("Failed to parse JSON from file " + file_path + ": " + e.what());
    }

    return j;
}

void JsonUtils::save_to_file(const json& j, const std::string& file_path) {
    save_to_file(j, file_path, {});
}

void JsonUtils::save_to_file(const json& j, const std::string& file_path,
                            const std::function<void(const std::filesystem::path&)>& before_replace) {
    const auto contents = j.dump(2);
    const auto target = path_from_utf8(file_path);
    if (!target.parent_path().empty()) std::filesystem::create_directories(target.parent_path());
    std::error_code ec;
    const auto previous = std::filesystem::status(target, ec);
    if (ec && ec != std::errc::no_such_file_or_directory)
        throw std::filesystem::filesystem_error("Could not inspect JSON target", target, ec);
    std::ostringstream suffix;
    suffix << ".tmp." << std::this_thread::get_id() << "."
           << std::chrono::steady_clock::now().time_since_epoch().count() << "."
           << reinterpret_cast<std::uintptr_t>(&j);
    auto temporary = target; temporary += suffix.str();
    try {
        std::ofstream file(temporary, std::ios::trunc);
        if (!file.is_open()) throw std::runtime_error("Failed to stage JSON file: " + file_path);
        if (std::filesystem::is_regular_file(previous) && previous.permissions() != std::filesystem::perms::unknown) {
            std::filesystem::permissions(temporary, previous.permissions(), ec);
            if (ec) throw std::filesystem::filesystem_error("Could not preserve JSON permissions", temporary, ec);
        }
        file << contents;
        file.flush(); file.close();
        if (!file) throw std::runtime_error("Failed to flush staged JSON file: " + file_path);
        if (before_replace) before_replace(temporary);
        if (!atomic_replace_file(temporary, target, ec))
            throw std::runtime_error("Failed to replace JSON file " + file_path + ": " + ec.message());
    } catch (...) {
        const auto failure = std::current_exception();
        std::filesystem::remove(temporary, ec);
        if (ec) {
            try { std::rethrow_exception(failure); }
            catch (const std::exception& error) {
                throw std::runtime_error(std::string(error.what()) + "; uncommitted staging retained at " + path_to_utf8(temporary));
            }
        }
        std::rethrow_exception(failure);
    }
}

json JsonUtils::parse(const std::string& json_str) {
    try {
        return json::parse(json_str);
    } catch (const json::exception& e) {
        throw std::runtime_error(std::string("Failed to parse JSON string: ") + e.what());
    }
}

std::string JsonUtils::to_string(const json& j, int indent) {
    return j.dump(indent);
}

json JsonUtils::merge(const json& base, const json& overlay) {
    if (!base.is_object()) {
        return overlay;
    }
    if (!overlay.is_object()) {
        return overlay;
    }

    json result = base;
    for (auto it = overlay.begin(); it != overlay.end(); ++it) {
        if (result.contains(it.key()) && result[it.key()].is_object() && it.value().is_object()) {
            result[it.key()] = merge(result[it.key()], it.value());
        } else {
            result[it.key()] = it.value();
        }
    }

    return result;
}

void JsonUtils::prune_matching(json& overlay, const json& base) {
    if (!overlay.is_object() || !base.is_object()) {
        return;
    }

    for (auto it = overlay.begin(); it != overlay.end(); ) {
        const std::string& key = it.key();
        if (key == "config_version") {
            ++it;
            continue;
        }

        if (base.contains(key)) {
            if (it.value().is_object() && base[key].is_object()) {
                prune_matching(it.value(), base[key]);
                if (it.value().empty()) {
                    it = overlay.erase(it);
                    continue;
                }
            } else if (it.value() == base[key]) {
                it = overlay.erase(it);
                continue;
            }
        }
        ++it;
    }

    if (overlay.size() == 1 && overlay.contains("config_version")) {
        overlay.clear();
    }
}

void JsonUtils::add_legacy_max_tokens_alias(json& request) {
    if (request.contains("max_completion_tokens") && !request.contains("max_tokens")) {
        request["max_tokens"] = request["max_completion_tokens"];
    }
}

json JsonUtils::with_legacy_max_tokens_alias(const json& request) {
    json modified = request;
    add_legacy_max_tokens_alias(modified);
    return modified;
}

bool JsonUtils::has_key(const json& j, const std::string& key) {
    return j.contains(key) && !j[key].is_null();
}

std::string JsonUtils::base64_encode(const std::string& input) {
    static const char base64_chars[] =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ"
        "abcdefghijklmnopqrstuvwxyz"
        "0123456789+/";

    std::string output;
    output.reserve(((input.size() + 2) / 3) * 4);

    int val = 0;
    int valb = -6;
    for (unsigned char c : input) {
        val = (val << 8) + c;
        valb += 8;
        while (valb >= 0) {
            output.push_back(base64_chars[(val >> valb) & 0x3F]);
            valb -= 6;
        }
    }
    if (valb > -6) {
        output.push_back(base64_chars[((val << 8) >> (valb + 8)) & 0x3F]);
    }
    while (output.size() % 4) {
        output.push_back('=');
    }
    return output;
}

std::string JsonUtils::base64_decode(const std::string& input) {
    static const std::string base64_chars =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ"
        "abcdefghijklmnopqrstuvwxyz"
        "0123456789+/";

    static const auto lookup = []() {
        std::vector<int> tbl(256, -1);
        for (int i = 0; i < 64; i++) {
            tbl[static_cast<unsigned char>(base64_chars[i])] = i;
        }
        return tbl;
    }();

    std::string output;
    int val = 0;
    int valb = -8;
    for (unsigned char c : input) {
        if (lookup[c] == -1) break;
        val = (val << 6) + lookup[c];
        valb += 6;
        if (valb >= 0) {
            output.push_back(static_cast<char>((val >> valb) & 0xFF));
            valb -= 8;
        }
    }
    return output;
}

} // namespace utils
} // namespace lemon
