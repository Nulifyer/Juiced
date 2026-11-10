#pragma once

#include <filesystem>
#include <fstream>
#include <functional>
#include <iomanip>
#include <mbedtls/md.h>
#include <sstream>
#include <stdexcept>
#include <string_view>

namespace lemon::utils {
inline std::string string_sha256(std::string_view text) {
    unsigned char digest[32];
    if (mbedtls_md(mbedtls_md_info_from_type(MBEDTLS_MD_SHA256),
            reinterpret_cast<const unsigned char*>(text.data()), text.size(), digest) != 0)
        throw std::runtime_error("Cannot checksum observed runtime content");
    std::ostringstream result;
    for (auto byte : digest) result << std::hex << std::setw(2) << std::setfill('0') << int(byte);
    return result.str();
}

inline std::string content_sha256(const std::filesystem::path& file,
                                  const std::function<bool()>& cancelled = {}) {
    mbedtls_md_context_t context;
    mbedtls_md_init(&context);
    struct Cleanup {
        mbedtls_md_context_t* context;
        ~Cleanup() { mbedtls_md_free(context); }
    } cleanup{&context};
    if (mbedtls_md_setup(&context, mbedtls_md_info_from_type(MBEDTLS_MD_SHA256), 0) != 0 ||
        mbedtls_md_starts(&context) != 0)
        throw std::runtime_error("Cannot initialize file checksum");
    std::ifstream input(file, std::ios::binary);
    char block[65536];
    while (input) {
        if (cancelled && cancelled()) throw std::runtime_error("File checksum cancelled");
        input.read(block, sizeof(block));
        if (input.gcount() && mbedtls_md_update(&context,
                reinterpret_cast<const unsigned char*>(block), input.gcount()) != 0)
            throw std::runtime_error("File checksum failed");
    }
    if (!input.eof()) throw std::runtime_error("Cannot read file for checksum");
    if (cancelled && cancelled()) throw std::runtime_error("File checksum cancelled");
    unsigned char digest[32];
    if (mbedtls_md_finish(&context, digest) != 0) throw std::runtime_error("File checksum failed");
    std::ostringstream result;
    for (auto byte : digest) result << std::hex << std::setw(2) << std::setfill('0') << int(byte);
    return result.str();
}
}
