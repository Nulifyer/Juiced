// Native FUTO CPU subprocess for Juiced's asr4all backend.
// Uses the unmodified transcribe.cpp C API, not Whisper inference.
#include "httplib.h"
#include "transcribe.h"
#include "transcribe/asr4all.h"
#include <nlohmann/json.hpp>
#define DR_WAV_IMPLEMENTATION
#include "dr_wav.h"
#include <cmath>
#include <filesystem>
#include <iostream>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <vector>

using json = nlohmann::json;

static void check(transcribe_status status) {
  if (status != TRANSCRIBE_OK)
    throw std::runtime_error(transcribe_status_string(status));
}

static std::string trim(std::string text) {
  const auto first = text.find_first_not_of(" \r\n\t");
  if (first == std::string::npos)
    return "";
  return text.substr(first, text.find_last_not_of(" \r\n\t") - first + 1);
}

// Bound allocation from untrusted WAV headers before decoding sample data.
static std::vector<float> decode(const std::string &bytes) {
  drwav wav;
  if (!drwav_init_memory(&wav, bytes.data(), bytes.size(), nullptr))
    throw std::invalid_argument("Expected a WAV audio container");
  struct Guard {
    drwav *wav;
    ~Guard() { drwav_uninit(wav); }
  } guard{&wav};
  if (wav.sampleRate != 16000 || wav.channels < 1 || wav.channels > 8 ||
      wav.totalPCMFrameCount == 0 || wav.totalPCMFrameCount > 16000 * 120)
    throw std::invalid_argument(
        "Use 16 kHz WAV, 1-8 channels, maximum 120 seconds");
  std::vector<float> input(wav.totalPCMFrameCount * wav.channels);
  if (drwav_read_pcm_frames_f32(&wav, wav.totalPCMFrameCount, input.data()) !=
      wav.totalPCMFrameCount)
    throw std::invalid_argument("Truncated WAV audio");
  std::vector<float> mono(wav.totalPCMFrameCount, 0.0f);
  for (size_t i = 0; i < mono.size(); ++i) {
    for (unsigned c = 0; c < wav.channels; ++c) {
      float value = input[i * wav.channels + c];
      if (!std::isfinite(value))
        throw std::invalid_argument("Nonfinite audio samples");
      mono[i] += value / wav.channels;
    }
  }
  return mono;
}

int main(int argc, char **argv) {
  try {
    std::string path;
    int port = 0, threads = 4, commit = 256, right = 32;
    for (int i = 1; i < argc; ++i) {
      const std::string flag = argv[i];
      if (flag == "--version") {
        std::cout << "FUTO asr4all adapter 1, transcribe.cpp "
                  << transcribe_version() << "\n";
        return 0;
      }
      if (i + 1 == argc)
        throw std::invalid_argument("Missing argument value: " + flag);
      const std::string value = argv[++i];
      if (flag == "-m" || flag == "--model")
        path = value;
      else if (flag == "--port")
        port = std::stoi(value);
      else if (flag == "-t" || flag == "--threads")
        threads = std::stoi(value);
      else if (flag == "--futo-commit")
        commit = std::stoi(value);
      else if (flag == "--futo-right")
        right = std::stoi(value);
      else
        throw std::invalid_argument("Unsupported argument: " + flag);
    }
    if (path.empty() || port < 1024 || port > 65535 || threads < 1 ||
        threads > 32)
      throw std::invalid_argument(
          "Required: -m MODEL --port PORT, optional --threads 1..32");

    transcribe_model_load_params load;
    transcribe_model_load_params_init(&load);
    load.backend = TRANSCRIBE_BACKEND_CPU;
    transcribe_session_params params;
    transcribe_session_params_init(&params);
    params.n_threads = threads;
    transcribe_session *raw = nullptr;
    check(transcribe_open(path.c_str(), &load, &params, &raw));
    std::unique_ptr<transcribe_session, decltype(&transcribe_session_free)>
        session(raw, transcribe_session_free);
    if (std::string(transcribe_model_arch_string(transcribe_get_model(raw))) !=
        "asr4all")
      throw std::invalid_argument(
          "This CPU override accepts FUTO asr4all models only");
    transcribe_asr4all_stream_ext family;
    transcribe_asr4all_stream_ext_init(&family);
    family.commit_frames = commit;
    family.right_ctx_frames = right;
    transcribe_stream_params stream;
    transcribe_stream_params_init(&stream);
    stream.family = &family.ext;
    transcribe_run_params run;
    transcribe_run_params_init(&run);
    run.language = "en";
    auto infer = [&](const std::vector<float> &pcm) {
      check(transcribe_stream_begin(raw, &run, &stream));
      check(transcribe_stream_feed(raw, pcm.data(),
                                   static_cast<int>(pcm.size()), nullptr));
      check(transcribe_stream_finalize(raw, nullptr));
      transcribe_stream_text text;
      transcribe_stream_text_init(&text);
      check(transcribe_stream_get_text(raw, &text));
      return trim(text.full_text ? text.full_text : "");
    };
    // Readiness includes a successful compute, matching the prior Python
    // adapter.
    infer(std::vector<float>(16000, 0.0f));
    std::mutex mutex;
    httplib::Server server;
    server.new_task_queue = [] { return new httplib::ThreadPool(4); };
    server.set_payload_max_length(32 * 1024 * 1024);
    server.Get("/health", [&](const auto &, auto &res) {
      res.set_content(
          json{{"status", "ok"},
               {"model", std::filesystem::path(path).stem().string()},
               {"engine", "FUTO transcribe.cpp"},
               {"threads", threads}}
              .dump(),
          "application/json");
    });
    server.Post("/inference", [&](const auto &req, auto &res) {
      try {
        const auto field = [&](const std::string &name,
                               const std::string &fallback) {
          return req.form.has_field(name) ? req.form.get_field(name) : fallback;
        };
        if (!req.form.has_file("file"))
          throw std::invalid_argument("Missing audio file");
        const auto language = field("language", "en");
        if (language != "en" && language != "english" && language != "auto" &&
            !language.empty())
          throw std::invalid_argument("FUTO asr4all supports English only");
        const auto format = field("response_format", "json");
        if (format != "json" && format != "text")
          throw std::invalid_argument("Supported response formats: json, text");
        if (!field("prompt", "").empty())
          throw std::invalid_argument("Prompt biasing is unsupported");
        if (field("translate", "false") != "false")
          throw std::invalid_argument("Translation is unsupported");
        if (std::stod(field("temperature", "0")) != 0.0)
          throw std::invalid_argument(
              "FUTO does not implement temperature sampling");
        const auto pcm = decode(req.form.get_file("file").content);
        std::lock_guard<std::mutex> lock(mutex);
        const auto text = infer(pcm);
        res.set_content(format == "text" ? text : json{{"text", text}}.dump(),
                        format == "text" ? "text/plain" : "application/json");
      } catch (const std::invalid_argument &error) {
        res.status = 400;
        res.set_content(json{{"error", error.what()}}.dump(),
                        "application/json");
      } catch (const std::exception &error) {
        res.status = 500;
        res.set_content(json{{"error", error.what()}}.dump(),
                        "application/json");
      }
    });
    // Lemonade owns process lifetime and sends termination on unload/restart.
    if (!server.listen("127.0.0.1", port))
      throw std::runtime_error("Cannot bind backend listener");
  } catch (const std::exception &error) {
    std::cerr << "FUTO backend: " << error.what() << "\n";
    return 1;
  }
}
