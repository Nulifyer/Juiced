#pragma once
#include "lemon/backends/backend_descriptor.h"
namespace lemon::backends::asr4all {
inline const BackendDescriptor descriptor = {
    /*recipe*/ "asr4all",
    /*display_name*/ "FUTO transcribe.cpp",
    /*binary*/ "futo-asr-server",
    /*config_section*/ "asr4all",
    /*default_device*/ DEVICE_CPU,
    /*slot_policy*/ SlotPolicy::Standard,
    /*selectable_backend*/ false,
    /*uses_ctx_size*/ false,
    /*dynamic_models*/ false,
    /*options*/
    {
        {"asr4all_threads", "--asr4all-threads", "4", "N",
         "CPU inference threads", "FUTO Options"},
        {"asr4all_commit", "--asr4all-commit", "256", "N",
         "Committed encoder frames per window", "FUTO Options"},
        {"asr4all_right", "--asr4all-right", "32", "N", "Right context frames",
         "FUTO Options"},
    },
    /*support*/
    {{"cpu",
      BackendTier::Experimental,
      BackendFormat::Native,
      {"linux"},
      {{"cpu", {"x86_64"}}},
      "x86_64 CPU"}},
    /*supported_modes*/ {"transcription"},
    /*required_checkpoints*/ {"main"},
    /*default_capabilities*/ {},
    /*web_display_name*/ "FUTO transcribe.cpp",
    /*rocm_channels*/ {},
    /*exposes_prometheus_metrics*/ false,
    /*rocm_requires_cwsr_fix*/ false,
    /*version_policy*/ VersionPolicy::Exact,
    /*self_manages_downloads*/ false,
    /*takes_args*/ false,
    /*arg_variants*/ {},
    /*bin_variants*/ {"cpu"},
};
}
