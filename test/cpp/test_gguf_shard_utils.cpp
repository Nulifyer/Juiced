#include "lemon/gguf_shard_utils.h"
#include "lemon/backends/llamacpp/llamacpp_server.h"
#include "lemon/model_manager.h"
#include "lemon/utils/content_hash.h"

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <string>

namespace lemon::backends {
struct LlamaCppInputIdentityTestHook {
    static json parse(const std::string& args) { return LlamaCppServer::execution_argument_inputs(args); }
    static json environment(const std::map<std::string, std::string>& values, const std::string& directory) {
        return LlamaCppServer::execution_environment_inputs(values, directory);
    }
};
}

namespace fs = std::filesystem;

static int g_failures = 0;

static void check(const char* name, bool ok) {
    std::printf("[%s] %s\n", ok ? "PASS" : "FAIL", name);
    if (!ok) ++g_failures;
}

static fs::path make_file(const fs::path& dir, const std::string& name, std::uintmax_t bytes) {
    fs::path p = dir / name;
    { std::ofstream(p, std::ios::out | std::ios::trunc); }
    std::error_code ec;
    fs::resize_file(p, bytes, ec);
    if (ec) {
        std::printf("resize_file('%s') failed: %s\n", p.string().c_str(), ec.message().c_str());
    }
    return p;
}

static fs::path make_temp_test_dir() {
    const auto tick = std::chrono::steady_clock::now().time_since_epoch().count();
    return fs::temp_directory_path() / ("lemonade_gguf_shard_test_" + std::to_string(tick));
}

int main() {
    fs::path base_dir = make_temp_test_dir();
    std::error_code ec;
    fs::remove_all(base_dir, ec);
    fs::create_directories(base_dir, ec);

    {
        std::string b;
        bool ok = lemon::is_gguf_shard_filename("Laguna-S-2.1-UD-Q4_K_XL-00001-of-00003.gguf", &b);
        check("detects folder-sharded filename", ok);
        check("extracts correct shard base", b == "Laguna-S-2.1-UD-Q4_K_XL");
    }
    {
        std::string b;
        int t = 0;
        int i = 0;
        check("extracts total shard count",
              lemon::is_gguf_shard_filename("model-00001-of-00005.gguf", &b, &t, &i) && t == 5 && i == 1);
    }
    {
        std::string b;
        check("dot-separated shard is detected",
              lemon::is_gguf_shard_filename("model.00001-of-00003.gguf", &b) && b == "model");
    }
    {
        std::string b;
        check("underscore-separated shard is detected",
              lemon::is_gguf_shard_filename("model_00001-of-00003.gguf", &b) && b == "model");
    }
    {
        std::string b;
        check("bare numbered file is not a shard (no -of-)",
              !lemon::is_gguf_shard_filename("model-7.gguf", &b));
    }
    {
        std::string b;
        check("shard index without a separator is not a shard",
              !lemon::is_gguf_shard_filename("model00001-of-00003.gguf", &b));
    }
    {
        std::string b;
        check("plain model.gguf is not a shard", !lemon::is_gguf_shard_filename("model.gguf", &b));
    }
    {
        std::string b;
        check("interrupted .gguf.partial is not a shard",
              !lemon::is_gguf_shard_filename("model-00001-of-00003.gguf.partial", &b));
    }
    {
        std::string b;
        check("zero total is not a shard",
              !lemon::is_gguf_shard_filename("model-00001-of-00000.gguf", &b));
    }
    {
        std::string b;
        check("zero index is not a shard",
              !lemon::is_gguf_shard_filename("model-00000-of-00003.gguf", &b));
    }
    {
        std::string b;
        check("index greater than total is not a shard",
              !lemon::is_gguf_shard_filename("model-00004-of-00003.gguf", &b));
    }

    {
        check("matching base and total are the same family",
              lemon::same_shard_family("model-00002-of-00003.gguf", "model", 3));
        check("same base, different total is a different family",
              !lemon::same_shard_family("model-00001-of-00005.gguf", "model", 3));
        check("different base is a different family",
              !lemon::same_shard_family("other-00001-of-00003.gguf", "model", 3));
        check("non-shard filename is not a family member",
              !lemon::same_shard_family("model.gguf", "model", 3));
        check("out-of-range index is not a family member",
              !lemon::same_shard_family("model-00004-of-00003.gguf", "model", 3));
    }

    {
        fs::path sub = base_dir / "same_dir";
        fs::create_directories(sub, ec);
        make_file(sub, "same-00001-of-00003.gguf", 10);
        make_file(sub, "same-00002-of-00003.gguf", 20);
        make_file(sub, "same-00003-of-00003.gguf", 30);
        make_file(sub, "same-00001-of-00005.gguf", 500);
        make_file(sub, "other-00001-of-00002.gguf", 1000);
        make_file(sub, "model.gguf", 9000);
        make_file(sub, "model-7.gguf", 8000);

        check("of-3 family sums only its own shards",
              lemon::sharded_gguf_size_bytes(sub / "same-00001-of-00003.gguf") == 10 + 20 + 30);
        check("of-5 family does not absorb the of-3 family",
              lemon::sharded_gguf_size_bytes(sub / "same-00001-of-00005.gguf") == 500);
        check("different-base family sums independently",
              lemon::sharded_gguf_size_bytes(sub / "other-00001-of-00002.gguf") == 1000);
        check("non-shard path aggregates nothing",
              lemon::sharded_gguf_size_bytes(sub / "model.gguf") == 0);
    }

    {
        fs::path sub = base_dir / "widths";
        fs::create_directories(sub, ec);
        make_file(sub, "Q4_K_M-1-of-2.gguf", 50);
        make_file(sub, "Q4_K_M-2-of-2.gguf", 60);
        check("varying index widths are the same family",
              lemon::sharded_gguf_size_bytes(sub / "Q4_K_M-1-of-2.gguf") == 50 + 60);
    }

    {
        fs::path sub = base_dir / "separators";
        fs::create_directories(sub, ec);
        make_file(sub, "model.00001-of-00002.gguf", 70);
        make_file(sub, "model.00002-of-00002.gguf", 80);
        make_file(sub, "other_00001-of-00002.gguf", 90);
        make_file(sub, "other_00002-of-00002.gguf", 100);
        check("dot-separated shards are summed",
              lemon::sharded_gguf_size_bytes(sub / "model.00001-of-00002.gguf") == 70 + 80);
        check("underscore-separated shards are summed",
              lemon::sharded_gguf_size_bytes(sub / "other_00001-of-00002.gguf") == 90 + 100);
    }

    {
        const auto main = base_dir / "identity.gguf", companion = base_dir / "projector.gguf";
        { std::ofstream file(main); file << "abc"; }
        { std::ofstream file(companion); file << "projector"; }
        lemon::ModelInfo info;
        info.recipe = "llamacpp";
        info.checkpoints = {{"main", "fixture:main"}, {"mmproj", "fixture:projector"}};
        info.resolved_paths = {{"main", main.string()}, {"mmproj", companion.string()}};
        const auto captured = lemon::ModelManager::execution_inputs(info, {});
        check("execution identity hashes primary content", captured.at("files").at("main")[0].at("sha256") ==
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        check("execution identity retains companion and source roles", captured.at("files").contains("mmproj") && captured.at("checkpoints") == info.checkpoints);
        const auto modified = fs::last_write_time(main);
        { std::ofstream file(main); file << "abd"; }
        fs::last_write_time(main, modified);
        const auto changed = lemon::ModelManager::execution_inputs(info, {});
        check("same size and restored mtime cannot hide content change", changed.at("files").at("main") != captured.at("files").at("main"));
        check("captured identity remains unchanged after file replacement", captured.at("files").at("main")[0].at("sha256") ==
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        bool cancelled = false;
        try { lemon::ModelManager::execution_inputs(info, [] { return true; }); } catch (const std::exception&) { cancelled = true; }
        check("execution identity honors cancellation before reading", cancelled);
        int polls = 0;
        cancelled = false;
        try { lemon::utils::content_sha256(main, [&] { return ++polls == 2; }); } catch (const std::exception&) { cancelled = true; }
        check("file hashing checks cancellation through completion", cancelled);
        const auto first = make_file(base_dir, "identity-00001-of-00002.gguf", 17);
        const auto second = make_file(base_dir, "identity-00002-of-00002.gguf", 19);
        info.checkpoints = {{"main", "fixture:split"}};
        info.resolved_paths = {{"main", first.string()}};
        check("execution identity includes every split checkpoint", lemon::ModelManager::execution_inputs(info, {}).at("files").at("main").size() == 2);
        fs::remove(second);
        bool missing = false;
        try { lemon::ModelManager::execution_inputs(info, {}); } catch (const std::exception&) { missing = true; }
        check("missing split checkpoint cannot produce an identity", missing);
        make_file(base_dir, "identity-1-of-2.gguf", 23);
        bool duplicate = false;
        try { lemon::ModelManager::execution_inputs(info, {}); } catch (const std::exception&) { duplicate = true; }
        check("duplicate shard indices cannot disguise missing content", duplicate);
    }


    {
        using Hook = lemon::backends::LlamaCppInputIdentityTestHook;
        const auto env = Hook::environment({{"LLAMA_ARG_CHAT_TEMPLATE_FILE", "template.jinja"}, {"OMP_NUM_THREADS", "2"},
            {"LLAMA_ARG_ROPE_FREQ_SCALE", "-1.2e-3"}, {"LEMONADE_API_KEY", "credential-sentinel"}}, base_dir.string());
        check("environment files resolve against the actual child directory", env.at("files").at("LLAMA_ARG_CHAT_TEMPLATE_FILE") == (base_dir / "template.jinja").string());
        check("known numeric environment controls retain values without credentials", !env.at("unresolved").get<bool>() &&
            env.at("scalars").at("OMP_NUM_THREADS") == "2" && env.dump().find("credential-sentinel") == std::string::npos);
        for (const auto& values : std::vector<std::map<std::string, std::string>>{
                {{"LLAMA_ARG_FUTURE_INPUT", "secret-sentinel"}}, {{"LLAMA_API_KEY", "secret-sentinel"}},
                {{"LLAMA_ARG_THREADS", "secret-sentinel"}}, {{"LLAMA_ARG_THREADS", std::string(65, '1')}},
                {{"LLAMA_ARG_MODEL", "unobserved.gguf"}}, {{"LD_PRELOAD", "unresolved-library"}},
                {{"LLAMA_ARG_CHAT_TEMPLATE_FILE", ""}}, {{"LLAMA_ARG_CACHE_TYPE_K", "unknown"}}}) {
            const auto unsupported = Hook::environment(values, base_dir.string());
            check("unsupported environment inputs refuse capture without exposing their values", unsupported.at("unresolved").get<bool>() &&
                unsupported.dump().find("secret-sentinel") == std::string::npos);
        }
        check("observed runtime text uses the same content digest as files", lemon::utils::string_sha256("abc") ==
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        const auto template_args = Hook::parse("--chat-template-file='a template,custom.jinja' --parallel=2 -t 2 --cache-ram 256");
        check("quoted equals template keeps spaces and comma", template_args.at("unknown").empty() &&
            template_args.at("files").at("--chat-template-file:0") == "a template,custom.jinja");
        const auto adapters = Hook::parse("--lora a.gguf,b.gguf --lora c.gguf --control-vector v.gguf --control-vector-layer-range 1 12");
        check("repeated adapter lists retain every input", adapters.at("unknown").empty() && adapters.at("files").size() == 4 &&
            adapters.at("files").at("--lora:2") == "c.gguf" && adapters.at("files").at("--control-vector:0") == "v.gguf");
        const auto scaled = Hook::parse(R"(--lora-scaled 'C:\models\adapter.gguf:0.5,second.gguf:-1e-2' --control-vector-scaled vector.gguf:2)");
        check("scaled files preserve drive colons and finite signed exponents", scaled.at("unknown").empty() &&
            scaled.at("files").at("--lora-scaled:0") == R"(C:\models\adapter.gguf)" &&
            scaled.at("files").at("--lora-scaled:1") == "second.gguf");
        for (const auto& input : {"--chat-template-file", "--lora a.gguf,", "--lora-scaled a.gguf:nan",
                "--lora-scaled a.gguf:inf", "--lora-scaled a.gguf:1suffix", "--lora-scaled a.gguf:",
                "--control-vector-layer-range 1", "positional --parallel 1"}) {
            check("ambiguous input cannot qualify an execution capture", !Hook::parse(input).at("unknown").empty());
        }
        const auto unknown = Hook::parse("--future-input secret-canary --threads 2");
        check("unknown argument diagnostics exclude supplied values", unknown.at("unknown").size() == 1 &&
            unknown.at("unknown")[0] == "--future-input" && unknown.dump().find("secret-canary") == std::string::npos);
        check("ordinary scalar and switch arguments add no external files",
            Hook::parse("-t 2 --parallel 1 --no-mmap --seed -1 --chat-template chatml").at("files").empty() &&
            Hook::parse("-t 2 --parallel 1 --no-mmap --seed -1 --chat-template chatml").at("unknown").empty());
        check("empty external input set needs no filesystem access", lemon::ModelManager::execution_files({}, {}).empty());
        const auto template_path = base_dir / "template.jinja";
        { std::ofstream file(template_path); file << "template A"; }
        const auto before = lemon::ModelManager::execution_files({{"template", template_path.string()}}, {});
        { std::ofstream file(template_path); file << "template B"; }
        check("external file content changes its identity", before != lemon::ModelManager::execution_files({{"template", template_path.string()}}, {}));
        bool cancelled = false;
        try { lemon::ModelManager::execution_files({{"template", template_path.string()}}, [] { return true; }); }
        catch (const std::exception&) { cancelled = true; }
        check("external file identity honors cancellation", cancelled);
        std::map<std::string, std::string> excessive;
        for (int i = 0; i < 4097; ++i) excessive[std::to_string(i)] = "missing-file";
        bool bounded = false;
        try { lemon::ModelManager::execution_files(excessive, {}); }
        catch (const std::exception& error) { bounded = std::string(error.what()).find("too many file roles") != std::string::npos; }
        check("external file roles are bounded before filesystem access", bounded);
        lemon::ModelInfo info;
        info.recipe = "llamacpp";
        info.checkpoints = {{"main", "missing-main"}, {"draft", "missing-draft-a"}};
        info.resolved_paths = {{"main", "missing-path-a"}};
        const auto definition = lemon::ModelManager::execution_definition(info);
        info.checkpoints["draft"] = "missing-draft-b";
        check("value-only definition detects companion changes without file reads", definition != lemon::ModelManager::execution_definition(info));
        info.checkpoints["draft"] = "missing-draft-a";
        info.type = lemon::ModelType::TRANSCRIPTION;
        check("value-only definition detects model type changes", definition != lemon::ModelManager::execution_definition(info));
        info.type = lemon::ModelType::LLM;
        info.resolved_paths["main"] = "missing-path-b";
        check("value-only definition detects resolved path changes", definition != lemon::ModelManager::execution_definition(info));
    }

    fs::remove_all(base_dir, ec);

    if (g_failures == 0) {
        std::printf("\nAll gguf_shard tests passed\n");
        return 0;
    }
    std::printf("\n%d gguf_shard test(s) FAILED\n", g_failures);
    return 1;
}
